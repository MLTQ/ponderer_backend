use chrono::{Datelike, Local, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static LOCAL_TIME_FALLBACK_WARNED: AtomicBool = AtomicBool::new(false);

/// Foundation-only presence monitor.
///
/// ll.1 intentionally keeps this as a lightweight stub so schema/types can land
/// without changing agent behavior. Real platform sampling is introduced later.
pub struct PresenceMonitor {
    session_start: Instant,
    last_interaction: Option<Instant>,
    process_cache: HashMap<u32, ProcessCategory>,
}

impl PresenceMonitor {
    pub fn new() -> Self {
        Self {
            session_start: Instant::now(),
            last_interaction: None,
            process_cache: HashMap::new(),
        }
    }

    pub fn record_interaction(&mut self) {
        self.last_interaction = Some(Instant::now());
    }

    pub fn sample(&mut self) -> PresenceState {
        let now = Instant::now();
        let user_idle_seconds = self.get_user_idle_seconds().unwrap_or_else(|| {
            self.last_interaction
                .map(|instant| now.saturating_duration_since(instant).as_secs())
                .unwrap_or(0)
        });
        let time_since_interaction = Duration::from_secs(user_idle_seconds);
        let active_processes = self.get_interesting_processes();
        let system_load = self.get_system_load();

        PresenceState {
            user_idle_seconds,
            time_since_interaction,
            session_duration: now.saturating_duration_since(self.session_start),
            time_context: TimeContext::now(),
            system_load,
            active_processes,
        }
    }

    #[cfg(target_os = "macos")]
    fn get_user_idle_seconds(&self) -> Option<u64> {
        let output = Command::new("ioreg")
            .args(["-c", "IOHIDSystem"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        for line in text.lines() {
            if let Some(index) = line.find("HIDIdleTime") {
                let candidate = &line[index..];
                let nanos = parse_first_integer(candidate)?;
                return Some(nanos / 1_000_000_000);
            }
        }
        None
    }

    #[cfg(target_os = "linux")]
    fn get_user_idle_seconds(&self) -> Option<u64> {
        let output = Command::new("xprintidle").output().ok()?;
        if !output.status.success() {
            return None;
        }
        let ms = String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()?;
        Some(ms / 1000)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn get_user_idle_seconds(&self) -> Option<u64> {
        None
    }

    fn get_interesting_processes(&mut self) -> Vec<InterestingProcess> {
        let output = match Command::new("ps")
            .args(["-A", "-o", "pid=,pcpu=,comm=,args="])
            .output()
        {
            Ok(out) if out.status.success() => out,
            _ => return Vec::new(),
        };

        let text = match String::from_utf8(output.stdout) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };

        let mut processes = Vec::new();
        for line in text.lines() {
            let mut parts = line.trim().split_whitespace();
            let pid = match parts.next().and_then(|v| v.parse::<u32>().ok()) {
                Some(v) => v,
                None => continue,
            };
            let cpu = match parts.next().and_then(|v| v.parse::<f32>().ok()) {
                Some(v) => v,
                None => continue,
            };
            if cpu <= 0.25 {
                continue;
            }
            let comm = parts.next().unwrap_or_default();
            let args = parts.collect::<Vec<_>>().join(" ");
            let descriptor = if args.is_empty() {
                comm.to_string()
            } else {
                format!("{comm} {args}")
            };
            let category = self
                .process_cache
                .get(&pid)
                .copied()
                .unwrap_or_else(|| categorize_process(&descriptor));
            self.process_cache.insert(pid, category);
            processes.push(InterestingProcess {
                name: comm.to_string(),
                category,
                cpu_percent: cpu,
            });
        }

        processes.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
        processes.truncate(8);
        processes
    }

    fn get_system_load(&self) -> SystemLoad {
        let cpu_percent = self.sample_cpu_percent().unwrap_or(0.0);
        let memory_percent = self.sample_memory_percent().unwrap_or(0.0);
        let (gpu_temp_celsius, gpu_util_percent) = self.sample_gpu();

        SystemLoad {
            cpu_percent,
            memory_percent,
            gpu_temp_celsius,
            gpu_util_percent,
        }
    }

    fn sample_cpu_percent(&self) -> Option<f32> {
        let output = Command::new("ps")
            .args(["-A", "-o", "pcpu="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let total = text
            .lines()
            .filter_map(|line| line.trim().parse::<f32>().ok())
            .sum::<f32>();
        let cores = self.logical_core_count().unwrap_or(1).max(1) as f32;
        Some((total / cores).clamp(0.0, 100.0))
    }

    fn sample_memory_percent(&self) -> Option<f32> {
        let output = Command::new("ps")
            .args(["-A", "-o", "pmem="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let total = text
            .lines()
            .filter_map(|line| line.trim().parse::<f32>().ok())
            .sum::<f32>();
        Some(total.clamp(0.0, 100.0))
    }

    fn logical_core_count(&self) -> Option<u32> {
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("sysctl")
                .args(["-n", "hw.logicalcpu"])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            return String::from_utf8(output.stdout)
                .ok()?
                .trim()
                .parse::<u32>()
                .ok();
        }
        #[cfg(target_os = "linux")]
        {
            let output = Command::new("nproc").output().ok()?;
            if !output.status.success() {
                return None;
            }
            return String::from_utf8(output.stdout)
                .ok()?
                .trim()
                .parse::<u32>()
                .ok();
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    fn sample_gpu(&self) -> (Option<f32>, Option<f32>) {
        let output = match Command::new("nvidia-smi")
            .args([
                "--query-gpu=temperature.gpu,utilization.gpu",
                "--format=csv,noheader,nounits",
            ])
            .output()
        {
            Ok(out) => out,
            Err(_) => return (None, None),
        };

        if !output.status.success() {
            return (None, None);
        }

        let text = match String::from_utf8(output.stdout) {
            Ok(s) => s,
            Err(_) => return (None, None),
        };
        let line = match text.lines().next() {
            Some(l) => l,
            None => return (None, None),
        };

        let mut values = line.split(',').map(str::trim);
        let temp = values.next().and_then(|v| v.parse::<f32>().ok());
        let util = values.next().and_then(|v| v.parse::<f32>().ok());
        (temp, util)
    }
}

impl Default for PresenceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceState {
    pub user_idle_seconds: u64,
    #[serde(with = "duration_seconds")]
    pub time_since_interaction: Duration,
    #[serde(with = "duration_seconds")]
    pub session_duration: Duration,
    pub time_context: TimeContext,
    pub system_load: SystemLoad,
    pub active_processes: Vec<InterestingProcess>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeContext {
    pub local_hour: u8,
    pub local_minute: u8,
    pub day_of_week: Weekday,
    pub is_weekend: bool,
    pub is_late_night: bool,
    pub is_deep_night: bool,
    pub approx_work_hours: bool,
}

impl TimeContext {
    pub fn now() -> Self {
        let components = std::panic::catch_unwind(|| {
            let now = Local::now();
            (now.hour() as u8, now.minute() as u8, now.weekday())
        });

        let (hour, minute, weekday) = match components {
            Ok(values) => values,
            Err(_) => {
                if !LOCAL_TIME_FALLBACK_WARNED.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "Local clock sampling panicked; falling back to UTC time context"
                    );
                }
                let now = Utc::now();
                (now.hour() as u8, now.minute() as u8, now.weekday())
            }
        };
        Self::from_components(hour, minute, weekday)
    }

    fn from_components(hour: u8, minute: u8, weekday: Weekday) -> Self {
        let is_weekend = matches!(weekday, Weekday::Sat | Weekday::Sun);
        Self {
            local_hour: hour,
            local_minute: minute,
            day_of_week: weekday,
            is_weekend,
            is_late_night: hour >= 23 || hour < 6,
            is_deep_night: (2..5).contains(&hour),
            approx_work_hours: !is_weekend && (8..=18).contains(&hour),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemLoad {
    pub cpu_percent: f32,
    pub memory_percent: f32,
    pub gpu_temp_celsius: Option<f32>,
    pub gpu_util_percent: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterestingProcess {
    pub name: String,
    pub category: ProcessCategory,
    pub cpu_percent: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessCategory {
    Development,
    Creative,
    Research,
    Communication,
    Media,
    System,
}

fn categorize_process(descriptor: &str) -> ProcessCategory {
    let value = descriptor.to_ascii_lowercase();

    if contains_any(
        &value,
        &[
            "code", "cursor", "zed", "xcode", "cargo", "rustc", "clang", "gcc", "node", "npm",
            "python", "docker", "git", "cmake",
        ],
    ) {
        return ProcessCategory::Development;
    }

    if contains_any(
        &value,
        &[
            "figma",
            "blender",
            "photoshop",
            "krita",
            "gimp",
            "ableton",
            "reaper",
            "davinci",
            "final cut",
        ],
    ) {
        return ProcessCategory::Creative;
    }

    if contains_any(
        &value,
        &[
            "chrome",
            "firefox",
            "safari",
            "brave",
            "arc",
            "wikipedia",
            "obsidian",
            "pdf",
        ],
    ) {
        return ProcessCategory::Research;
    }

    if contains_any(
        &value,
        &[
            "slack", "discord", "telegram", "signal", "mail", "messages", "zoom", "teams",
        ],
    ) {
        return ProcessCategory::Communication;
    }

    if contains_any(&value, &["spotify", "vlc", "music", "mpv", "youtube"]) {
        return ProcessCategory::Media;
    }

    ProcessCategory::System
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn parse_first_integer(input: &str) -> Option<u64> {
    let mut buffer = String::new();
    let mut seen_digit = false;

    for ch in input.chars() {
        if ch.is_ascii_digit() {
            buffer.push(ch);
            seen_digit = true;
        } else if seen_digit {
            break;
        }
    }

    if buffer.is_empty() {
        None
    } else {
        buffer.parse::<u64>().ok()
    }
}

mod duration_seconds {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(Duration::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_returns_nonzero_session_duration() {
        let mut monitor = PresenceMonitor::new();
        let state = monitor.sample();
        assert!(state.session_duration.as_secs() < 2);
        assert!(state.time_context.local_hour <= 23);
    }

    #[test]
    fn process_categorization_heuristics() {
        assert_eq!(
            categorize_process("Cursor /Applications/Cursor.app"),
            ProcessCategory::Development
        );
        assert_eq!(
            categorize_process("Slack helper"),
            ProcessCategory::Communication
        );
        assert_eq!(
            categorize_process("Spotify Desktop"),
            ProcessCategory::Media
        );
    }
}
