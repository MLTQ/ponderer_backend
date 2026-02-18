use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::concerns::Concern;
use crate::agent::journal::JournalEntry;
use crate::database::PersonaSnapshot;
use crate::llm_client::{LlmClient, Message as LlmMessage};
use crate::presence::{PresenceState, ProcessCategory};
use crate::skills::SkillEvent;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopObservation {
    pub captured_at: DateTime<Utc>,
    pub screenshot_path: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrientationContext {
    pub presence: PresenceState,
    pub concerns: Vec<Concern>,
    pub recent_journal: Vec<JournalEntry>,
    pub pending_events: Vec<SkillEvent>,
    pub persona: Option<PersonaSnapshot>,
    pub desktop_observation: Option<DesktopObservation>,
}

impl OrientationContext {
    pub fn format_time(&self) -> String {
        let t = &self.presence.time_context;
        format!(
            "{}:{:02} {:?} | weekend={} late_night={} deep_night={} work_hours={}",
            t.local_hour,
            t.local_minute,
            t.day_of_week,
            t.is_weekend,
            t.is_late_night,
            t.is_deep_night,
            t.approx_work_hours
        )
    }

    pub fn format_system(&self) -> String {
        let load = &self.presence.system_load;
        format!(
            "cpu={:.1}% mem={:.1}% gpu_temp={:?} gpu_util={:?}",
            load.cpu_percent, load.memory_percent, load.gpu_temp_celsius, load.gpu_util_percent
        )
    }

    pub fn format_presence(&self) -> String {
        let mut out = format!(
            "idle={}s session={}s top_processes={}",
            self.presence.user_idle_seconds,
            self.presence.session_duration.as_secs(),
            self.presence.active_processes.len()
        );
        for proc in self.presence.active_processes.iter().take(6) {
            out.push_str(&format!(
                "\n- {} [{:?}] cpu={:.1}%",
                proc.name, proc.category, proc.cpu_percent
            ));
        }
        out
    }

    pub fn format_concerns(&self) -> String {
        if self.concerns.is_empty() {
            return "None".to_string();
        }
        self.concerns
            .iter()
            .take(8)
            .map(|concern| format!("- {} ({:?})", concern.summary, concern.salience))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn format_journal(&self) -> String {
        if self.recent_journal.is_empty() {
            return "None".to_string();
        }
        self.recent_journal
            .iter()
            .take(6)
            .map(|entry| {
                format!(
                    "- [{}] {}",
                    entry.timestamp.format("%Y-%m-%d %H:%M"),
                    entry.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn format_events(&self) -> String {
        if self.pending_events.is_empty() {
            return "None".to_string();
        }
        self.pending_events
            .iter()
            .take(12)
            .map(|event| match event {
                SkillEvent::NewContent {
                    id, source, author, ..
                } => {
                    format!("- id={} source={} author={}", id, source, author)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn format_trajectory(&self) -> String {
        let Some(persona) = &self.persona else {
            return "None".to_string();
        };
        match persona.inferred_trajectory.as_deref() {
            Some(traj) => format!("{} | {}", persona.self_description, traj),
            None => persona.self_description.clone(),
        }
    }

    pub fn format_desktop_observation(&self) -> String {
        let Some(obs) = &self.desktop_observation else {
            return "None".to_string();
        };

        format!(
            "captured_at={} path={}\nsummary={}",
            obs.captured_at.format("%Y-%m-%d %H:%M:%S UTC"),
            obs.screenshot_path,
            obs.summary.trim()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Orientation {
    pub user_state: UserStateEstimate,
    pub salience_map: Vec<SalientItem>,
    pub anomalies: Vec<Anomaly>,
    pub pending_thoughts: Vec<PendingThought>,
    pub disposition: Disposition,
    pub mood_estimate: MoodEstimate,
    pub raw_synthesis: String,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum UserStateEstimate {
    DeepWork {
        activity: String,
        duration_estimate_secs: u64,
        confidence: f32,
    },
    LightWork {
        activity: String,
        confidence: f32,
    },
    Idle {
        since_secs: u64,
        confidence: f32,
    },
    Away {
        since_secs: u64,
        likely_reason: Option<String>,
        confidence: f32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalientItem {
    pub source: String,
    pub summary: String,
    pub relevance: f32,
    pub relates_to: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub id: String,
    pub description: String,
    pub severity: AnomalySeverity,
    pub first_noticed: DateTime<Utc>,
    pub related_concerns: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalySeverity {
    Interesting,
    Notable,
    Concerning,
    Urgent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingThought {
    pub id: String,
    pub content: String,
    pub context: String,
    pub priority: f32,
    pub relates_to: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Idle,
    Observe,
    Journal,
    Maintain,
    Surface,
    Interrupt,
}

impl Disposition {
    fn from_str(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "observe" => Disposition::Observe,
            "journal" => Disposition::Journal,
            "maintain" => Disposition::Maintain,
            "surface" => Disposition::Surface,
            "interrupt" => Disposition::Interrupt,
            _ => Disposition::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoodEstimate {
    pub valence: f32,
    pub arousal: f32,
    pub confidence: f32,
}

pub struct OrientationEngine {
    client: LlmClient,
    model: String,
}

impl OrientationEngine {
    pub fn new(api_url: String, model: String, api_key: Option<String>) -> Self {
        Self {
            client: LlmClient::new(api_url, api_key.unwrap_or_default(), model.clone()),
            model,
        }
    }

    pub async fn orient(&self, context: OrientationContext) -> Result<Orientation> {
        let prompt = Self::build_orientation_prompt(&context);
        let messages = vec![
            LlmMessage {
                role: "system".to_string(),
                content: "You are an orientation engine for a desktop companion agent. Return strict JSON only.".to_string(),
            },
            LlmMessage {
                role: "user".to_string(),
                content: prompt,
            },
        ];

        let parsed = self
            .client
            .generate_json::<OrientationLlmResponse>(messages, Some(&self.model))
            .await;

        match parsed {
            Ok(response) => Ok(self.parse_orientation(response, &context)),
            Err(error) => {
                tracing::warn!(
                    "Orientation LLM parse failed, using heuristic fallback: {}",
                    error
                );
                Ok(self.fallback_orientation(
                    &context,
                    Some(format!("fallback after parse error: {}", error)),
                ))
            }
        }
    }

    pub fn build_orientation_prompt(ctx: &OrientationContext) -> String {
        format!(
            "You are the orientation engine for an AI companion living on Max's computer.\n\
             Synthesize current signals into situational awareness.\n\n\
             ## Current Time\n{}\n\n\
             ## System State\n{}\n\n\
             ## User Presence\n{}\n\n\
             ## Active Concerns\n{}\n\n\
             ## Recent Journal Entries\n{}\n\n\
             ## Pending Events\n{}\n\n\
             ## Desktop Observation\n{}\n\n\
             ## Current Persona Trajectory\n{}\n\n\
             Return JSON with keys:\n\
             user_state, salient_items, anomalies, pending_thoughts, disposition, disposition_reason, mood, synthesis.\n\
             Use disposition in [idle, observe, journal, maintain, surface, interrupt].",
            ctx.format_time(),
            ctx.format_system(),
            ctx.format_presence(),
            ctx.format_concerns(),
            ctx.format_journal(),
            ctx.format_events(),
            ctx.format_desktop_observation(),
            ctx.format_trajectory(),
        )
    }

    fn parse_orientation(
        &self,
        response: OrientationLlmResponse,
        ctx: &OrientationContext,
    ) -> Orientation {
        let now = Utc::now();
        let user_state = parse_user_state(response.user_state, ctx.presence.user_idle_seconds);
        let salience_map = response
            .salient_items
            .into_iter()
            .filter_map(normalize_salient_item)
            .filter(|item| !item.summary.trim().is_empty())
            .map(|item| SalientItem {
                source: item.source.unwrap_or_else(|| "orientation".to_string()),
                summary: item.summary,
                relevance: clamp01(item.relevance.unwrap_or(0.5)),
                relates_to: item.relates_to.unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let anomalies = response
            .anomalies
            .into_iter()
            .filter_map(normalize_anomaly)
            .filter(|anomaly| !anomaly.description.trim().is_empty())
            .map(|anomaly| Anomaly {
                id: anomaly
                    .id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                description: anomaly.description,
                severity: parse_anomaly_severity(
                    anomaly.severity.as_deref().unwrap_or("interesting"),
                ),
                first_noticed: now,
                related_concerns: anomaly.related_concerns.unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let pending_thoughts = response
            .pending_thoughts
            .into_iter()
            .filter_map(normalize_pending_thought)
            .filter(|thought| !thought.content.trim().is_empty())
            .map(|thought| PendingThought {
                id: uuid::Uuid::new_v4().to_string(),
                content: thought.content,
                context: thought.context.unwrap_or_else(|| "orientation".to_string()),
                priority: clamp01(thought.priority.unwrap_or(0.5)),
                relates_to: thought.relates_to.unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let mood = parse_mood(response.mood);

        Orientation {
            user_state,
            salience_map,
            anomalies,
            pending_thoughts,
            disposition: Disposition::from_str(&response.disposition),
            mood_estimate: MoodEstimate {
                valence: clamp_signed(mood.valence.unwrap_or(0.0)),
                arousal: clamp01(mood.arousal.unwrap_or(0.4)),
                confidence: clamp01(mood.confidence.unwrap_or(0.6)),
            },
            raw_synthesis: response
                .synthesis
                .unwrap_or_else(|| "No synthesis returned".to_string()),
            generated_at: now,
        }
    }

    fn fallback_orientation(&self, ctx: &OrientationContext, note: Option<String>) -> Orientation {
        let now = Utc::now();
        let idle = ctx.presence.user_idle_seconds;
        let cpu = ctx.presence.system_load.cpu_percent;
        let mem = ctx.presence.system_load.memory_percent;

        let user_state = if idle > 1800 {
            UserStateEstimate::Away {
                since_secs: idle,
                likely_reason: Some("inactive".to_string()),
                confidence: 0.7,
            }
        } else if idle > 300 {
            UserStateEstimate::Idle {
                since_secs: idle,
                confidence: 0.75,
            }
        } else if ctx.presence.active_processes.iter().any(|proc| {
            matches!(
                proc.category,
                ProcessCategory::Development | ProcessCategory::Creative
            )
        }) && cpu > 20.0
        {
            UserStateEstimate::DeepWork {
                activity: "active focused session".to_string(),
                duration_estimate_secs: idle,
                confidence: 0.62,
            }
        } else {
            UserStateEstimate::LightWork {
                activity: "active desktop usage".to_string(),
                confidence: 0.58,
            }
        };

        let mut anomalies = Vec::new();
        if let Some(temp) = ctx.presence.system_load.gpu_temp_celsius {
            if temp >= 90.0 {
                anomalies.push(Anomaly {
                    id: uuid::Uuid::new_v4().to_string(),
                    description: format!("GPU temperature is high ({temp:.1}C)"),
                    severity: AnomalySeverity::Concerning,
                    first_noticed: now,
                    related_concerns: Vec::new(),
                });
            }
        }
        if mem >= 92.0 {
            anomalies.push(Anomaly {
                id: uuid::Uuid::new_v4().to_string(),
                description: format!("Memory usage is very high ({mem:.1}%)"),
                severity: AnomalySeverity::Notable,
                first_noticed: now,
                related_concerns: Vec::new(),
            });
        }

        let mut salience_map = ctx
            .pending_events
            .iter()
            .map(|event| match event {
                SkillEvent::NewContent { source, author, .. } => SalientItem {
                    source: "skill_event".to_string(),
                    summary: format!("New content from {} in {}", author, source),
                    relevance: 0.8,
                    relates_to: Vec::new(),
                },
            })
            .collect::<Vec<_>>();
        salience_map.extend(ctx.concerns.iter().take(6).map(|concern| SalientItem {
            source: "concern".to_string(),
            summary: concern.summary.clone(),
            relevance: 0.65,
            relates_to: vec![concern.id.clone()],
        }));
        if let Some(obs) = &ctx.desktop_observation {
            salience_map.push(SalientItem {
                source: "desktop_observation".to_string(),
                summary: obs.summary.clone(),
                relevance: 0.72,
                relates_to: Vec::new(),
            });
        }

        let disposition = if !ctx.pending_events.is_empty() {
            Disposition::Observe
        } else if !anomalies.is_empty() {
            Disposition::Surface
        } else {
            Disposition::Idle
        };

        let mut synthesis = format!(
            "Heuristic orientation: idle={}s cpu={:.1}% mem={:.1}% events={}",
            idle,
            cpu,
            mem,
            ctx.pending_events.len()
        );
        if let Some(obs) = &ctx.desktop_observation {
            synthesis.push_str(&format!(
                " desktop=\"{}\"",
                obs.summary.replace('\n', " ").trim()
            ));
        }
        if let Some(note) = note {
            synthesis.push_str(&format!(" ({note})"));
        }

        Orientation {
            user_state,
            salience_map,
            anomalies,
            pending_thoughts: Vec::new(),
            disposition,
            mood_estimate: MoodEstimate {
                valence: 0.0,
                arousal: if idle > 300 { 0.2 } else { 0.5 },
                confidence: 0.45,
            },
            raw_synthesis: synthesis,
            generated_at: now,
        }
    }
}

pub fn context_signature(ctx: &OrientationContext) -> String {
    #[derive(Serialize)]
    struct Signature<'a> {
        idle_bucket: u64,
        hour: u8,
        minute_bucket: u8,
        cpu_bucket: u8,
        memory_bucket: u8,
        process_labels: Vec<String>,
        concern_ids: Vec<&'a str>,
        journal_ids: Vec<&'a str>,
        event_ids: Vec<&'a str>,
        persona_id: Option<&'a str>,
        desktop_observation: Option<String>,
    }

    let process_labels = ctx
        .presence
        .active_processes
        .iter()
        .take(6)
        .map(|proc| format!("{}:{:?}", proc.name, proc.category))
        .collect::<Vec<_>>();
    let concern_ids = ctx
        .concerns
        .iter()
        .take(10)
        .map(|c| c.id.as_str())
        .collect();
    let journal_ids = ctx
        .recent_journal
        .iter()
        .take(10)
        .map(|j| j.id.as_str())
        .collect();
    let event_ids = ctx
        .pending_events
        .iter()
        .map(|event| match event {
            SkillEvent::NewContent { id, .. } => id.as_str(),
        })
        .collect::<Vec<_>>();

    let sig = Signature {
        idle_bucket: ctx.presence.user_idle_seconds / 30,
        hour: ctx.presence.time_context.local_hour,
        minute_bucket: ctx.presence.time_context.local_minute / 5,
        cpu_bucket: (ctx.presence.system_load.cpu_percent / 5.0).floor() as u8,
        memory_bucket: (ctx.presence.system_load.memory_percent / 5.0).floor() as u8,
        process_labels,
        concern_ids,
        journal_ids,
        event_ids,
        persona_id: ctx.persona.as_ref().map(|p| p.id.as_str()),
        desktop_observation: ctx
            .desktop_observation
            .as_ref()
            .map(|obs| obs.summary.trim().chars().take(220).collect()),
    };

    serde_json::to_string(&sig).unwrap_or_else(|_| String::new())
}

fn parse_user_state(input: Option<LlmUserStateInput>, idle_secs: u64) -> UserStateEstimate {
    if let Some(LlmUserStateInput::Label(label)) = input {
        let normalized = label.trim().to_ascii_lowercase();
        return match normalized.as_str() {
            "deep_work" | "deepwork" | "focused" => UserStateEstimate::DeepWork {
                activity: "focused work".to_string(),
                duration_estimate_secs: idle_secs,
                confidence: 0.55,
            },
            "light_work" | "lightwork" | "active" | "working" | "busy" => {
                UserStateEstimate::LightWork {
                    activity: "active session".to_string(),
                    confidence: 0.55,
                }
            }
            "away" | "afk" | "offline" => UserStateEstimate::Away {
                since_secs: idle_secs,
                likely_reason: None,
                confidence: 0.6,
            },
            "idle" | "inactive" => UserStateEstimate::Idle {
                since_secs: idle_secs,
                confidence: 0.6,
            },
            _ => default_user_state(idle_secs),
        };
    }

    let input = input.and_then(|state| match state {
        LlmUserStateInput::Detailed(state) => Some(state),
        LlmUserStateInput::Label(_) => None,
    });

    let Some(state) = input else {
        return default_user_state(idle_secs);
    };

    let kind = state.kind.unwrap_or_else(|| "idle".to_string());
    let confidence = clamp01(state.confidence.unwrap_or(0.6));
    match kind.trim().to_ascii_lowercase().as_str() {
        "deep_work" | "deepwork" => UserStateEstimate::DeepWork {
            activity: state.activity.unwrap_or_else(|| "focused work".to_string()),
            duration_estimate_secs: state
                .duration_estimate_secs
                .or(state.since_secs)
                .unwrap_or(idle_secs),
            confidence,
        },
        "light_work" | "lightwork" => UserStateEstimate::LightWork {
            activity: state
                .activity
                .unwrap_or_else(|| "active session".to_string()),
            confidence,
        },
        "away" => UserStateEstimate::Away {
            since_secs: state.since_secs.unwrap_or(idle_secs),
            likely_reason: state.likely_reason,
            confidence,
        },
        _ => UserStateEstimate::Idle {
            since_secs: state.since_secs.unwrap_or(idle_secs),
            confidence,
        },
    }
}

fn normalize_salient_item(input: LlmSalientItemInput) -> Option<LlmSalientItem> {
    match input {
        LlmSalientItemInput::Detailed(item) => Some(item),
        LlmSalientItemInput::Text(summary) => {
            let summary = summary.trim();
            if summary.is_empty() {
                None
            } else {
                Some(LlmSalientItem {
                    source: None,
                    summary: summary.to_string(),
                    relevance: None,
                    relates_to: None,
                })
            }
        }
    }
}

fn normalize_anomaly(input: LlmAnomalyInput) -> Option<LlmAnomaly> {
    match input {
        LlmAnomalyInput::Detailed(item) => Some(item),
        LlmAnomalyInput::Text(description) => {
            let description = description.trim();
            if description.is_empty() {
                None
            } else {
                Some(LlmAnomaly {
                    id: None,
                    description: description.to_string(),
                    severity: Some("interesting".to_string()),
                    related_concerns: None,
                })
            }
        }
    }
}

fn normalize_pending_thought(input: LlmPendingThoughtInput) -> Option<LlmPendingThought> {
    match input {
        LlmPendingThoughtInput::Detailed(item) => Some(item),
        LlmPendingThoughtInput::Text(content) => {
            let content = content.trim();
            if content.is_empty() {
                None
            } else {
                Some(LlmPendingThought {
                    content: content.to_string(),
                    context: None,
                    priority: None,
                    relates_to: None,
                })
            }
        }
    }
}

fn parse_mood(input: Option<LlmMoodInput>) -> LlmMood {
    match input {
        Some(LlmMoodInput::Detailed(mood)) => mood,
        Some(LlmMoodInput::Label(label)) => {
            let label = label.trim().to_ascii_lowercase();
            match label.as_str() {
                "positive" | "happy" => LlmMood {
                    valence: Some(0.5),
                    arousal: Some(0.55),
                    confidence: Some(0.45),
                },
                "negative" | "sad" => LlmMood {
                    valence: Some(-0.5),
                    arousal: Some(0.45),
                    confidence: Some(0.45),
                },
                "anxious" | "stressed" => LlmMood {
                    valence: Some(-0.4),
                    arousal: Some(0.75),
                    confidence: Some(0.45),
                },
                "tired" | "low" => LlmMood {
                    valence: Some(-0.2),
                    arousal: Some(0.2),
                    confidence: Some(0.45),
                },
                _ => LlmMood {
                    valence: Some(0.0),
                    arousal: Some(0.4),
                    confidence: Some(0.45),
                },
            }
        }
        None => LlmMood::default(),
    }
}

fn default_user_state(idle_secs: u64) -> UserStateEstimate {
    if idle_secs > 1800 {
        UserStateEstimate::Away {
            since_secs: idle_secs,
            likely_reason: None,
            confidence: 0.6,
        }
    } else if idle_secs > 300 {
        UserStateEstimate::Idle {
            since_secs: idle_secs,
            confidence: 0.7,
        }
    } else {
        UserStateEstimate::LightWork {
            activity: "active desktop usage".to_string(),
            confidence: 0.55,
        }
    }
}

fn parse_anomaly_severity(raw: &str) -> AnomalySeverity {
    match raw.trim().to_ascii_lowercase().as_str() {
        "notable" => AnomalySeverity::Notable,
        "concerning" => AnomalySeverity::Concerning,
        "urgent" => AnomalySeverity::Urgent,
        _ => AnomalySeverity::Interesting,
    }
}

fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

fn clamp_signed(value: f32) -> f32 {
    value.clamp(-1.0, 1.0)
}

#[derive(Debug, Deserialize)]
struct OrientationLlmResponse {
    #[serde(default)]
    user_state: Option<LlmUserStateInput>,
    #[serde(default, alias = "salience_map", alias = "salient")]
    salient_items: Vec<LlmSalientItemInput>,
    #[serde(default)]
    anomalies: Vec<LlmAnomalyInput>,
    #[serde(default, alias = "pending_actions", alias = "thoughts")]
    pending_thoughts: Vec<LlmPendingThoughtInput>,
    #[serde(default)]
    disposition: String,
    #[serde(default, alias = "mood_estimate")]
    mood: Option<LlmMoodInput>,
    #[serde(default)]
    synthesis: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LlmUserStateInput {
    Detailed(LlmUserState),
    Label(String),
}

#[derive(Debug, Deserialize)]
struct LlmUserState {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    activity: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    duration_estimate_secs: Option<u64>,
    #[serde(default)]
    since_secs: Option<u64>,
    #[serde(default)]
    likely_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LlmSalientItemInput {
    Detailed(LlmSalientItem),
    Text(String),
}

#[derive(Debug, Deserialize)]
struct LlmSalientItem {
    #[serde(default)]
    source: Option<String>,
    #[serde(default, alias = "content", alias = "text")]
    summary: String,
    #[serde(default, alias = "score", alias = "importance")]
    relevance: Option<f32>,
    #[serde(default)]
    relates_to: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LlmAnomalyInput {
    Detailed(LlmAnomaly),
    Text(String),
}

#[derive(Debug, Deserialize)]
struct LlmAnomaly {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, alias = "summary", alias = "issue")]
    description: String,
    #[serde(default, alias = "level")]
    severity: Option<String>,
    #[serde(default)]
    related_concerns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LlmPendingThoughtInput {
    Detailed(LlmPendingThought),
    Text(String),
}

#[derive(Debug, Deserialize)]
struct LlmPendingThought {
    #[serde(default, alias = "summary", alias = "thought")]
    content: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    priority: Option<f32>,
    #[serde(default)]
    relates_to: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LlmMoodInput {
    Detailed(LlmMood),
    Label(String),
}

#[derive(Debug, Deserialize, Default)]
struct LlmMood {
    #[serde(default)]
    valence: Option<f32>,
    #[serde(default)]
    arousal: Option<f32>,
    #[serde(default)]
    confidence: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::{SystemLoad, TimeContext};
    use chrono::Weekday;
    use std::time::Duration;

    fn sample_context() -> OrientationContext {
        OrientationContext {
            presence: PresenceState {
                user_idle_seconds: 45,
                time_since_interaction: Duration::from_secs(45),
                session_duration: Duration::from_secs(3600),
                time_context: TimeContext {
                    local_hour: 14,
                    local_minute: 30,
                    day_of_week: Weekday::Thu,
                    is_weekend: false,
                    is_late_night: false,
                    is_deep_night: false,
                    approx_work_hours: true,
                },
                system_load: SystemLoad {
                    cpu_percent: 21.0,
                    memory_percent: 56.0,
                    gpu_temp_celsius: None,
                    gpu_util_percent: None,
                },
                active_processes: Vec::new(),
            },
            concerns: Vec::new(),
            recent_journal: Vec::new(),
            pending_events: Vec::new(),
            persona: None,
            desktop_observation: None,
        }
    }

    #[test]
    fn orientation_prompt_contains_sections() {
        let prompt = OrientationEngine::build_orientation_prompt(&sample_context());
        assert!(prompt.contains("## Current Time"));
        assert!(prompt.contains("## System State"));
        assert!(prompt.contains("## User Presence"));
        assert!(prompt.contains("## Desktop Observation"));
    }

    #[test]
    fn context_signature_changes_with_idle_bucket() {
        let mut ctx_a = sample_context();
        let mut ctx_b = sample_context();
        ctx_b.presence.user_idle_seconds = 310;
        let sig_a = context_signature(&ctx_a);
        let sig_b = context_signature(&ctx_b);
        assert_ne!(sig_a, sig_b);

        // deterministic for same input
        let sig_a2 = context_signature(&ctx_a);
        assert_eq!(sig_a, sig_a2);

        ctx_a.presence.user_idle_seconds = 45;
    }

    #[test]
    fn context_signature_changes_with_desktop_observation() {
        let mut ctx_a = sample_context();
        let mut ctx_b = sample_context();
        ctx_b.desktop_observation = Some(DesktopObservation {
            captured_at: Utc::now(),
            screenshot_path: "/tmp/shot.png".to_string(),
            summary: "User is editing Rust source in terminal".to_string(),
        });

        assert_ne!(context_signature(&ctx_a), context_signature(&ctx_b));

        ctx_a.desktop_observation = ctx_b.desktop_observation.clone();
        assert_eq!(context_signature(&ctx_a), context_signature(&ctx_b));
    }

    #[test]
    fn default_user_state_transitions() {
        assert!(matches!(
            default_user_state(30),
            UserStateEstimate::LightWork { .. }
        ));
        assert!(matches!(
            default_user_state(400),
            UserStateEstimate::Idle { .. }
        ));
        assert!(matches!(
            default_user_state(3000),
            UserStateEstimate::Away { .. }
        ));
    }

    #[test]
    fn llm_response_deserializes_common_alias_fields() {
        let value = serde_json::json!({
            "salience_map": [
                {
                    "text": "User is coding in Rust",
                    "score": 0.9
                }
            ],
            "anomalies": [
                {
                    "summary": "High memory pressure",
                    "level": "notable"
                }
            ],
            "pending_actions": [
                {
                    "summary": "Offer to help profile memory usage",
                    "priority": 0.7
                }
            ],
            "mood_estimate": {
                "valence": 0.2,
                "arousal": 0.5,
                "confidence": 0.8
            },
            "synthesis": "Context looks healthy overall."
        });

        let parsed: OrientationLlmResponse =
            serde_json::from_value(value).expect("parse orientation aliases");
        assert_eq!(parsed.salient_items.len(), 1);
        match &parsed.salient_items[0] {
            LlmSalientItemInput::Detailed(item) => {
                assert_eq!(item.summary, "User is coding in Rust");
            }
            _ => panic!("expected detailed salient item"),
        }
        assert_eq!(parsed.anomalies.len(), 1);
        match &parsed.anomalies[0] {
            LlmAnomalyInput::Detailed(item) => {
                assert_eq!(item.description, "High memory pressure");
            }
            _ => panic!("expected detailed anomaly"),
        }
        assert_eq!(parsed.pending_thoughts.len(), 1);
        match &parsed.pending_thoughts[0] {
            LlmPendingThoughtInput::Detailed(item) => {
                assert_eq!(item.content, "Offer to help profile memory usage");
            }
            _ => panic!("expected detailed pending thought"),
        }
        assert!(parsed.mood.is_some());
    }

    #[test]
    fn llm_response_deserializes_scalar_friendly_shapes() {
        let value = serde_json::json!({
            "user_state": "active",
            "salient_items": [
                "High CPU usage from system processes"
            ],
            "anomalies": [
                "Potential fragmentation in AI persona trajectory"
            ],
            "pending_thoughts": [
                "User appears engaged in software development"
            ],
            "disposition": "observe",
            "mood": "neutral",
            "synthesis": "User is active and should be observed."
        });

        let parsed: OrientationLlmResponse =
            serde_json::from_value(value).expect("parse scalar-friendly orientation shape");
        let user_state = parse_user_state(parsed.user_state, 42);
        assert!(matches!(user_state, UserStateEstimate::LightWork { .. }));

        let salient: Vec<LlmSalientItem> = parsed
            .salient_items
            .into_iter()
            .filter_map(normalize_salient_item)
            .collect();
        assert_eq!(salient.len(), 1);
        assert_eq!(salient[0].summary, "High CPU usage from system processes");

        let anomalies: Vec<LlmAnomaly> = parsed
            .anomalies
            .into_iter()
            .filter_map(normalize_anomaly)
            .collect();
        assert_eq!(anomalies.len(), 1);
        assert_eq!(
            anomalies[0].description,
            "Potential fragmentation in AI persona trajectory"
        );

        let thoughts: Vec<LlmPendingThought> = parsed
            .pending_thoughts
            .into_iter()
            .filter_map(normalize_pending_thought)
            .collect();
        assert_eq!(thoughts.len(), 1);
        assert_eq!(
            thoughts[0].content,
            "User appears engaged in software development"
        );

        let mood = parse_mood(parsed.mood);
        assert_eq!(mood.valence, Some(0.0));
        assert_eq!(mood.arousal, Some(0.4));
    }
}
