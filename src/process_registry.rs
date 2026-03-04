use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};

const MAX_CAPTURED_OUTPUT_BYTES: usize = 120_000;
const PROCESS_POLL_INTERVAL_MS: u64 = 500;

#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub id: String,
    pub command: String,
    pub working_directory: String,
    pub pid: Option<u32>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub recent_output: String,
}

struct ManagedProcess {
    info: RwLock<ProcessInfo>,
    child: Mutex<tokio::process::Child>,
}

#[derive(Clone, Default)]
pub struct ProcessRegistry {
    processes: Arc<RwLock<HashMap<String, Arc<ManagedProcess>>>>,
}

impl ProcessRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn start(&self, command: &str, working_directory: &str) -> Result<ProcessInfo> {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(working_directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn background process in '{}'",
                    working_directory
                )
            })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let process_id = uuid::Uuid::new_v4().to_string();
        let info = ProcessInfo {
            id: process_id.clone(),
            command: command.to_string(),
            working_directory: working_directory.to_string(),
            pid: child.id(),
            status: "running".to_string(),
            exit_code: None,
            started_at: Utc::now(),
            finished_at: None,
            recent_output: String::new(),
        };
        let managed = Arc::new(ManagedProcess {
            info: RwLock::new(info.clone()),
            child: Mutex::new(child),
        });

        self.processes
            .write()
            .await
            .insert(process_id, managed.clone());

        if let Some(stdout) = stdout {
            spawn_output_reader(managed.clone(), stdout, None);
        }
        if let Some(stderr) = stderr {
            spawn_output_reader(managed.clone(), stderr, Some("[stderr] "));
        }
        spawn_status_poller(managed);

        Ok(info)
    }

    pub async fn list(&self) -> Vec<ProcessInfo> {
        let processes = self.processes.read().await;
        let managed = processes.values().cloned().collect::<Vec<_>>();
        drop(processes);

        let mut infos = Vec::with_capacity(managed.len());
        for process in managed {
            infos.push(process.info.read().await.clone());
        }
        infos.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        infos
    }

    pub async fn get(&self, process_id: &str) -> Option<ProcessInfo> {
        let process = {
            let processes = self.processes.read().await;
            processes.get(process_id).cloned()
        }?;
        let info = process.info.read().await.clone();
        Some(info)
    }

    pub async fn stop(&self, process_id: &str) -> Result<Option<ProcessInfo>> {
        let process = {
            let processes = self.processes.read().await;
            processes.get(process_id).cloned()
        };
        let Some(process) = process else {
            return Ok(None);
        };

        {
            let mut child = process.child.lock().await;
            child
                .start_kill()
                .with_context(|| format!("Failed to stop background process {}", process_id))?;
        }

        {
            let mut info = process.info.write().await;
            if info.status == "running" {
                info.status = "stopping".to_string();
            }
        }

        let info = process.info.read().await.clone();
        Ok(Some(info))
    }
}

fn spawn_output_reader<R>(process: Arc<ManagedProcess>, reader: R, prefix: Option<&'static str>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let segment = match prefix {
                Some(prefix) => format!("{prefix}{line}\n"),
                None => format!("{line}\n"),
            };
            append_output(&process, &segment).await;
        }
    });
}

fn spawn_status_poller(process: Arc<ManagedProcess>) {
    tokio::spawn(async move {
        loop {
            let outcome = {
                let mut child = process.child.lock().await;
                child.try_wait()
            };

            match outcome {
                Ok(Some(status)) => {
                    let mut info = process.info.write().await;
                    info.status = "exited".to_string();
                    info.exit_code = status.code();
                    info.finished_at = Some(Utc::now());
                    break;
                }
                Ok(None) => {
                    tokio::time::sleep(Duration::from_millis(PROCESS_POLL_INTERVAL_MS)).await;
                }
                Err(error) => {
                    let mut info = process.info.write().await;
                    info.status = "failed".to_string();
                    info.finished_at = Some(Utc::now());
                    let segment = format!("[registry error] {error}\n");
                    info.recent_output = push_recent_output(
                        &info.recent_output,
                        &segment,
                        MAX_CAPTURED_OUTPUT_BYTES,
                    );
                    break;
                }
            }
        }
    });
}

async fn append_output(process: &Arc<ManagedProcess>, segment: &str) {
    let mut info = process.info.write().await;
    info.recent_output =
        push_recent_output(&info.recent_output, segment, MAX_CAPTURED_OUTPUT_BYTES);
}

fn push_recent_output(existing: &str, addition: &str, max_bytes: usize) -> String {
    let mut combined = String::with_capacity(existing.len() + addition.len());
    combined.push_str(existing);
    combined.push_str(addition);
    if combined.len() <= max_bytes {
        return combined;
    }

    let target = combined.len().saturating_sub(max_bytes);
    let start = combined
        .char_indices()
        .find_map(|(index, _)| (index >= target).then_some(index))
        .unwrap_or(0);
    let mut trimmed = String::from("[...truncated...]\n");
    trimmed.push_str(&combined[start..]);
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_from_front_when_output_exceeds_budget() {
        let output = push_recent_output("abcdef", "ghijkl", 5);
        assert!(output.contains("hijkl"));
        assert!(output.starts_with("[...truncated...]"));
    }
}
