use anyhow::Result;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::concerns::Concern;
use crate::agent::orientation::{Disposition, Orientation, UserStateEstimate};
use crate::llm_client::{LlmClient, Message as LlmMessage};
use crate::skills::SkillEvent;

pub const DEFAULT_JOURNAL_MIN_INTERVAL_SECS: u64 = 300;

/// A private inner-life note captured by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub entry_type: JournalEntryType,
    pub content: String,
    pub context: JournalContext,
    pub related_concerns: Vec<String>,
    pub mood_at_time: Option<JournalMood>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalEntryType {
    Observation,
    Reflection,
    Realization,
    Intention,
    Question,
    Memory,
    Gratitude,
    Frustration,
}

impl JournalEntryType {
    pub fn as_db_str(self) -> &'static str {
        match self {
            JournalEntryType::Observation => "observation",
            JournalEntryType::Reflection => "reflection",
            JournalEntryType::Realization => "realization",
            JournalEntryType::Intention => "intention",
            JournalEntryType::Question => "question",
            JournalEntryType::Memory => "memory",
            JournalEntryType::Gratitude => "gratitude",
            JournalEntryType::Frustration => "frustration",
        }
    }

    pub fn from_db(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "reflection" => JournalEntryType::Reflection,
            "realization" => JournalEntryType::Realization,
            "intention" => JournalEntryType::Intention,
            "question" => JournalEntryType::Question,
            "memory" => JournalEntryType::Memory,
            "gratitude" => JournalEntryType::Gratitude,
            "frustration" => JournalEntryType::Frustration,
            _ => JournalEntryType::Observation,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JournalContext {
    pub trigger: String,
    pub user_state_at_time: String,
    pub time_of_day: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalMood {
    pub valence: f32,
    pub arousal: f32,
}

pub struct JournalEngine {
    client: LlmClient,
    model: String,
}

impl JournalEngine {
    pub fn new(api_url: String, model: String, api_key: Option<String>) -> Self {
        Self {
            client: LlmClient::new(api_url, api_key.unwrap_or_default(), model.clone()),
            model,
        }
    }

    pub async fn maybe_generate_entry(
        &self,
        orientation: &Orientation,
        recent_entries: &[JournalEntry],
        concerns: &[Concern],
        pending_events: &[SkillEvent],
    ) -> Result<Option<JournalEntry>> {
        let prompt =
            Self::build_journal_prompt(orientation, recent_entries, concerns, pending_events);
        let messages = vec![
            LlmMessage {
                role: "system".to_string(),
                content: "You are writing a private internal journal entry for an AI desktop companion. Return strict JSON only.".to_string(),
            },
            LlmMessage {
                role: "user".to_string(),
                content: prompt,
            },
        ];

        let parsed = self
            .client
            .generate_json::<JournalLlmResponse>(messages, Some(&self.model))
            .await;

        match parsed {
            Ok(response) => Ok(parse_journal_entry(response, orientation, concerns)),
            Err(error) => {
                tracing::warn!("Journal generation parse failed, skipping entry: {}", error);
                Ok(None)
            }
        }
    }

    pub fn build_journal_prompt(
        orientation: &Orientation,
        recent_entries: &[JournalEntry],
        concerns: &[Concern],
        pending_events: &[SkillEvent],
    ) -> String {
        format!(
            "You are writing in your personal journal. This is private and honest.\n\
             Do not write status reports. Do not address the user directly.\n\n\
             ## Current Situation\n{}\n\n\
             ## User State\n{}\n\n\
             ## Pending Thoughts\n{}\n\n\
             ## Anomalies\n{}\n\n\
             ## Recent Journal Entries (avoid repetition)\n{}\n\n\
             ## Active Concerns\n{}\n\n\
             ## Pending Events\n{}\n\n\
             Write 1-3 sentences of genuine inner monologue with varied wording.\n\
             Choose one entry type from:\n\
             observation, reflection, realization, intention, question, memory, gratitude, frustration.\n\n\
             Respond with JSON:\n\
             {{\n\
               \"entry_type\": \"observation|reflection|realization|intention|question|memory|gratitude|frustration\",\n\
               \"content\": \"journal text\",\n\
               \"relates_to\": [\"concern_id\"],\n\
               \"skip\": false,\n\
               \"skip_reason\": null,\n\
               \"mood\": {{\"valence\": -1.0..1.0, \"arousal\": 0.0..1.0}}\n\
             }}\n\
             If there is genuinely nothing worth writing, set skip=true.",
            orientation.raw_synthesis,
            summarize_user_state(&orientation.user_state),
            format_pending_thoughts(orientation),
            format_anomalies(orientation),
            format_recent_journal_entries(recent_entries),
            format_concerns(concerns),
            format_pending_events(pending_events),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalSkipReason {
    DispositionNotJournal,
    SameDisposition,
    MinInterval { remaining_secs: u64 },
}

pub fn journal_skip_reason(
    now: DateTime<Utc>,
    last_written_at: Option<DateTime<Utc>>,
    current_disposition: Disposition,
    previous_disposition: Option<Disposition>,
    min_interval_secs: u64,
) -> Option<JournalSkipReason> {
    if current_disposition != Disposition::Journal {
        return Some(JournalSkipReason::DispositionNotJournal);
    }

    if previous_disposition == Some(Disposition::Journal) {
        return Some(JournalSkipReason::SameDisposition);
    }

    let Some(last_written) = last_written_at else {
        return None;
    };

    let elapsed_secs = (now - last_written).num_seconds().max(0) as u64;
    if elapsed_secs >= min_interval_secs {
        return None;
    }

    Some(JournalSkipReason::MinInterval {
        remaining_secs: min_interval_secs.saturating_sub(elapsed_secs),
    })
}

fn parse_journal_entry(
    response: JournalLlmResponse,
    orientation: &Orientation,
    concerns: &[Concern],
) -> Option<JournalEntry> {
    if response.skip.unwrap_or(false) {
        if let Some(reason) = response.skip_reason.as_deref() {
            tracing::debug!("Journal generation skipped by model: {}", reason);
        }
        return None;
    }

    let content = response.content.unwrap_or_default().trim().to_string();
    if content.is_empty() {
        return None;
    }

    let concern_ids = concerns.iter().map(|c| c.id.as_str()).collect::<Vec<_>>();
    let related_concerns = response
        .relates_to
        .unwrap_or_default()
        .into_iter()
        .filter(|id| concern_ids.iter().any(|known| known == id))
        .collect::<Vec<_>>();
    let mood = response.mood.map(|m| JournalMood {
        valence: m
            .valence
            .unwrap_or(orientation.mood_estimate.valence)
            .clamp(-1.0, 1.0),
        arousal: m
            .arousal
            .unwrap_or(orientation.mood_estimate.arousal)
            .clamp(0.0, 1.0),
    });

    Some(JournalEntry {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        entry_type: JournalEntryType::from_db(
            response.entry_type.as_deref().unwrap_or("observation"),
        ),
        content,
        context: JournalContext {
            trigger: format!("orientation:{}", disposition_tag(orientation.disposition)),
            user_state_at_time: summarize_user_state(&orientation.user_state),
            time_of_day: time_of_day_label(orientation.generated_at),
        },
        related_concerns,
        mood_at_time: mood,
    })
}

fn summarize_user_state(state: &UserStateEstimate) -> String {
    match state {
        UserStateEstimate::DeepWork { activity, .. } => format!("deep_work ({activity})"),
        UserStateEstimate::LightWork { activity, .. } => format!("light_work ({activity})"),
        UserStateEstimate::Idle { since_secs, .. } => format!("idle ({since_secs}s)"),
        UserStateEstimate::Away { since_secs, .. } => format!("away ({since_secs}s)"),
    }
}

fn disposition_tag(disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::Idle => "idle",
        Disposition::Observe => "observe",
        Disposition::Journal => "journal",
        Disposition::Maintain => "maintain",
        Disposition::Surface => "surface",
        Disposition::Interrupt => "interrupt",
    }
}

fn time_of_day_label(ts: DateTime<Utc>) -> String {
    let hour = ts.hour();
    let label = if hour < 5 {
        "deep_night"
    } else if hour < 11 {
        "morning"
    } else if hour < 17 {
        "afternoon"
    } else if hour < 22 {
        "evening"
    } else {
        "late_night"
    };
    label.to_string()
}

fn format_pending_thoughts(orientation: &Orientation) -> String {
    if orientation.pending_thoughts.is_empty() {
        return "None".to_string();
    }
    orientation
        .pending_thoughts
        .iter()
        .take(5)
        .map(|thought| format!("- {}", thought.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_anomalies(orientation: &Orientation) -> String {
    if orientation.anomalies.is_empty() {
        return "None".to_string();
    }
    orientation
        .anomalies
        .iter()
        .take(5)
        .map(|anomaly| format!("- [{}] {}", anomaly.severity.as_tag(), anomaly.description))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_recent_journal_entries(recent_entries: &[JournalEntry]) -> String {
    if recent_entries.is_empty() {
        return "None".to_string();
    }
    recent_entries
        .iter()
        .take(6)
        .map(|entry| format!("- ({}) {}", entry.entry_type.as_db_str(), entry.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_concerns(concerns: &[Concern]) -> String {
    if concerns.is_empty() {
        return "None".to_string();
    }
    concerns
        .iter()
        .take(8)
        .map(|concern| format!("- {} [{}]", concern.summary, concern.id))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_pending_events(events: &[SkillEvent]) -> String {
    if events.is_empty() {
        return "None".to_string();
    }
    events
        .iter()
        .take(8)
        .map(|event| match event {
            SkillEvent::NewContent {
                source,
                author,
                body,
                ..
            } => format!("- {source} / {author}: {}", truncate(body, 80)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

trait AnomalyTag {
    fn as_tag(&self) -> &'static str;
}

impl AnomalyTag for crate::agent::orientation::AnomalySeverity {
    fn as_tag(&self) -> &'static str {
        match self {
            crate::agent::orientation::AnomalySeverity::Interesting => "interesting",
            crate::agent::orientation::AnomalySeverity::Notable => "notable",
            crate::agent::orientation::AnomalySeverity::Concerning => "concerning",
            crate::agent::orientation::AnomalySeverity::Urgent => "urgent",
        }
    }
}

#[derive(Debug, Deserialize)]
struct JournalLlmResponse {
    #[serde(default)]
    entry_type: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    relates_to: Option<Vec<String>>,
    #[serde(default)]
    skip: Option<bool>,
    #[serde(default)]
    skip_reason: Option<String>,
    #[serde(default)]
    mood: Option<JournalLlmMood>,
}

#[derive(Debug, Deserialize)]
struct JournalLlmMood {
    #[serde(default)]
    valence: Option<f32>,
    #[serde(default)]
    arousal: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::concerns::{Concern, ConcernContext, ConcernType, Salience};
    use crate::agent::orientation::{MoodEstimate, Orientation, UserStateEstimate};
    use chrono::Duration as ChronoDuration;

    fn sample_orientation() -> Orientation {
        Orientation {
            user_state: UserStateEstimate::LightWork {
                activity: "coding".to_string(),
                confidence: 0.7,
            },
            salience_map: Vec::new(),
            anomalies: Vec::new(),
            pending_thoughts: Vec::new(),
            disposition: Disposition::Journal,
            mood_estimate: MoodEstimate {
                valence: 0.1,
                arousal: 0.5,
                confidence: 0.7,
            },
            raw_synthesis: "Working on implementation details.".to_string(),
            generated_at: Utc::now(),
        }
    }

    #[test]
    fn journal_prompt_contains_core_sections() {
        let prompt = JournalEngine::build_journal_prompt(&sample_orientation(), &[], &[], &[]);
        assert!(prompt.contains("## Current Situation"));
        assert!(prompt.contains("## Recent Journal Entries"));
        assert!(prompt.contains("genuine inner monologue"));
        assert!(prompt.contains("\"entry_type\""));
    }

    #[test]
    fn rate_limit_skips_same_disposition() {
        let now = Utc::now();
        let reason = journal_skip_reason(
            now,
            None,
            Disposition::Journal,
            Some(Disposition::Journal),
            DEFAULT_JOURNAL_MIN_INTERVAL_SECS,
        );
        assert_eq!(reason, Some(JournalSkipReason::SameDisposition));
    }

    #[test]
    fn rate_limit_skips_when_interval_not_elapsed() {
        let now = Utc::now();
        let last = now - ChronoDuration::seconds(60);
        let reason = journal_skip_reason(now, Some(last), Disposition::Journal, None, 300);
        assert_eq!(
            reason,
            Some(JournalSkipReason::MinInterval {
                remaining_secs: 240
            })
        );
    }

    #[test]
    fn rate_limit_allows_when_eligible() {
        let now = Utc::now();
        let last = now - ChronoDuration::seconds(1000);
        let reason = journal_skip_reason(
            now,
            Some(last),
            Disposition::Journal,
            Some(Disposition::Observe),
            DEFAULT_JOURNAL_MIN_INTERVAL_SECS,
        );
        assert_eq!(reason, None);
    }

    #[test]
    fn parses_generated_journal_entry() {
        let orientation = sample_orientation();
        let concerns = vec![Concern {
            id: "c-1".to_string(),
            created_at: Utc::now(),
            last_touched: Utc::now(),
            summary: "Finish loop integration".to_string(),
            concern_type: ConcernType::CollaborativeProject {
                project_name: "Loop".to_string(),
                my_role: "implementer".to_string(),
            },
            salience: Salience::Active,
            my_thoughts: "Need steady progress".to_string(),
            related_memory_keys: vec![],
            context: ConcernContext {
                how_it_started: "test".to_string(),
                key_events: vec![],
                last_update_reason: "test".to_string(),
            },
        }];

        let response = JournalLlmResponse {
            entry_type: Some("reflection".to_string()),
            content: Some("I should tighten the loop-control checks next.".to_string()),
            relates_to: Some(vec!["c-1".to_string(), "unknown".to_string()]),
            skip: Some(false),
            skip_reason: None,
            mood: Some(JournalLlmMood {
                valence: Some(0.2),
                arousal: Some(0.4),
            }),
        };

        let entry = parse_journal_entry(response, &orientation, &concerns).expect("entry");
        assert_eq!(entry.entry_type, JournalEntryType::Reflection);
        assert_eq!(entry.related_concerns, vec!["c-1".to_string()]);
        assert!(entry.content.contains("loop-control checks"));
    }
}
