use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::database::AgentDatabase;

pub const CONCERN_DECAY_TO_MONITORING_DAYS: i64 = 7;
pub const CONCERN_DECAY_TO_BACKGROUND_DAYS: i64 = 30;
pub const CONCERN_DECAY_TO_DORMANT_DAYS: i64 = 90;
pub const CONCERN_SIGNAL_MIN_CONFIDENCE: f32 = 0.35;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Concern {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub last_touched: DateTime<Utc>,
    pub summary: String,
    pub concern_type: ConcernType,
    pub salience: Salience,
    pub my_thoughts: String,
    pub related_memory_keys: Vec<String>,
    pub context: ConcernContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConcernType {
    CollaborativeProject {
        project_name: String,
        my_role: String,
    },
    HouseholdAwareness {
        category: String,
    },
    SystemHealth {
        component: String,
        monitoring_since: DateTime<Utc>,
    },
    PersonalInterest {
        topic: String,
        curiosity_level: f32,
    },
    Reminder {
        trigger_time: Option<DateTime<Utc>>,
        trigger_condition: Option<String>,
    },
    OngoingConversation {
        thread_id: String,
        with_whom: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Salience {
    Active,
    Monitoring,
    Background,
    Dormant,
}

impl Salience {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Salience::Active => "active",
            Salience::Monitoring => "monitoring",
            Salience::Background => "background",
            Salience::Dormant => "dormant",
        }
    }

    pub fn from_db(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "monitoring" => Salience::Monitoring,
            "background" => Salience::Background,
            "dormant" => Salience::Dormant,
            _ => Salience::Active,
        }
    }

    fn rank(self) -> u8 {
        match self {
            Salience::Active => 3,
            Salience::Monitoring => 2,
            Salience::Background => 1,
            Salience::Dormant => 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConcernContext {
    pub how_it_started: String,
    pub key_events: Vec<String>,
    pub last_update_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConcernSignal {
    pub summary: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub touch_only: bool,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub related_memory_keys: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ConcernIngestReport {
    pub created: Vec<Concern>,
    pub touched: Vec<Concern>,
    pub skipped: usize,
}

#[derive(Debug, Default)]
pub struct ConcernDecayReport {
    pub to_monitoring: usize,
    pub to_background: usize,
    pub to_dormant: usize,
}

impl ConcernDecayReport {
    pub fn total_changes(&self) -> usize {
        self.to_monitoring + self.to_background + self.to_dormant
    }
}

pub struct ConcernsManager;

impl ConcernsManager {
    pub fn ingest_signals(
        db: &AgentDatabase,
        signals: &[ConcernSignal],
        source: &str,
    ) -> Result<ConcernIngestReport> {
        let mut report = ConcernIngestReport::default();
        if signals.is_empty() {
            return Ok(report);
        }

        let mut concerns = db.get_all_concerns()?;
        let now = Utc::now();

        for signal in signals {
            if signal
                .confidence
                .is_some_and(|confidence| confidence < CONCERN_SIGNAL_MIN_CONFIDENCE)
            {
                report.skipped += 1;
                continue;
            }

            let summary = normalize_summary(&signal.summary);
            if summary.is_empty() {
                report.skipped += 1;
                continue;
            }

            if let Some(index) = find_existing_concern_index(&concerns, &summary) {
                let mut concern = concerns[index].clone();
                concern.last_touched = now;
                concern.salience = Salience::Active;
                concern.context.last_update_reason =
                    format!("touched from {} signal", source.trim());
                append_key_event(&mut concern.context, format!("Signal touched: {}", summary));
                merge_related_keys(
                    &mut concern.related_memory_keys,
                    &signal.related_memory_keys,
                );
                merge_notes(&mut concern.my_thoughts, signal.notes.as_deref());
                db.save_concern(&concern)?;
                concerns[index] = concern.clone();
                report.touched.push(concern);
                continue;
            }

            if signal.touch_only {
                report.skipped += 1;
                continue;
            }

            let concern = Concern {
                id: uuid::Uuid::new_v4().to_string(),
                created_at: now,
                last_touched: now,
                summary: summary.clone(),
                concern_type: concern_type_from_signal(signal, &summary),
                salience: Salience::Active,
                my_thoughts: signal.notes.clone().unwrap_or_default(),
                related_memory_keys: dedupe_related_keys(&signal.related_memory_keys),
                context: ConcernContext {
                    how_it_started: format!("created from {}", source.trim()),
                    key_events: vec![format!("Created from signal: {}", summary)],
                    last_update_reason: "created from signal".to_string(),
                },
            };
            db.save_concern(&concern)?;
            concerns.push(concern.clone());
            report.created.push(concern);
        }

        Ok(report)
    }

    pub fn touch_from_text(db: &AgentDatabase, text: &str, reason: &str) -> Result<Vec<Concern>> {
        let haystack = text.to_ascii_lowercase();
        if haystack.trim().is_empty() {
            return Ok(Vec::new());
        }

        let mut updated = Vec::new();
        for mut concern in db.get_all_concerns()? {
            if !text_mentions_concern(&haystack, &concern) {
                continue;
            }
            concern.last_touched = Utc::now();
            concern.salience = Salience::Active;
            concern.context.last_update_reason = reason.to_string();
            append_key_event(
                &mut concern.context,
                format!(
                    "Mention touched concern: {}",
                    truncate_for_log(&concern.summary, 80)
                ),
            );
            db.save_concern(&concern)?;
            updated.push(concern);
        }

        Ok(updated)
    }

    pub fn apply_salience_decay(
        db: &AgentDatabase,
        now: DateTime<Utc>,
    ) -> Result<ConcernDecayReport> {
        let mut report = ConcernDecayReport::default();
        for mut concern in db.get_all_concerns()? {
            let days_since_touch = (now - concern.last_touched).num_days();
            let target = salience_for_days_since_touch(days_since_touch);
            if target == concern.salience {
                continue;
            }

            concern.salience = target;
            concern.context.last_update_reason = format!(
                "salience decay after {} day(s) of inactivity",
                days_since_touch.max(0)
            );
            append_key_event(
                &mut concern.context,
                format!("Salience decay -> {}", concern.salience.as_db_str()),
            );
            db.save_concern(&concern)?;

            match target {
                Salience::Monitoring => report.to_monitoring += 1,
                Salience::Background => report.to_background += 1,
                Salience::Dormant => report.to_dormant += 1,
                Salience::Active => {}
            }
        }
        Ok(report)
    }

    pub fn build_priority_context(
        db: &AgentDatabase,
        max_concerns: usize,
        max_tokens: usize,
    ) -> Result<String> {
        if max_concerns == 0 || max_tokens == 0 {
            return Ok(String::new());
        }

        let mut concerns = db.get_all_concerns()?;
        concerns.retain(|concern| concern.salience != Salience::Dormant);
        if concerns.is_empty() {
            return Ok(String::new());
        }

        concerns.sort_by(|a, b| {
            b.salience
                .rank()
                .cmp(&a.salience.rank())
                .then_with(|| b.last_touched.cmp(&a.last_touched))
        });

        let selected = concerns.into_iter().take(max_concerns).collect::<Vec<_>>();
        let mut lines = Vec::new();
        lines.push("## Concern Priority Context".to_string());
        lines.push(String::new());

        let mut seen_memory_keys = HashSet::new();
        for concern in &selected {
            lines.push(format!(
                "- [{}] {}",
                concern.salience.as_db_str(),
                truncate_for_log(&concern.summary, 120)
            ));
            for key in &concern.related_memory_keys {
                if seen_memory_keys.insert(key.clone()) {
                    if let Some(memory) = db.get_working_memory(key)? {
                        lines.push(format!(
                            "  - memory:{} => {}",
                            key,
                            truncate_for_log(memory.content.trim(), 150)
                        ));
                    }
                }
            }
        }

        let mut out = String::new();
        let mut token_budget = 0usize;
        for line in lines {
            let estimate = line.split_whitespace().count();
            if token_budget + estimate > max_tokens {
                break;
            }
            token_budget += estimate;
            out.push_str(&line);
            out.push('\n');
        }

        Ok(out.trim_end().to_string())
    }
}

pub fn salience_for_days_since_touch(days_since_touch: i64) -> Salience {
    if days_since_touch >= CONCERN_DECAY_TO_DORMANT_DAYS {
        Salience::Dormant
    } else if days_since_touch >= CONCERN_DECAY_TO_BACKGROUND_DAYS {
        Salience::Background
    } else if days_since_touch >= CONCERN_DECAY_TO_MONITORING_DAYS {
        Salience::Monitoring
    } else {
        Salience::Active
    }
}

fn concern_type_from_signal(signal: &ConcernSignal, summary: &str) -> ConcernType {
    match signal
        .kind
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("collaborative_project") | Some("project") => ConcernType::CollaborativeProject {
            project_name: summary.to_string(),
            my_role: "assistant".to_string(),
        },
        Some("household_awareness") | Some("household") => ConcernType::HouseholdAwareness {
            category: summary.to_string(),
        },
        Some("system_health") | Some("system") => ConcernType::SystemHealth {
            component: summary.to_string(),
            monitoring_since: Utc::now(),
        },
        Some("reminder") => ConcernType::Reminder {
            trigger_time: None,
            trigger_condition: None,
        },
        Some("ongoing_conversation") | Some("conversation") => ConcernType::OngoingConversation {
            thread_id: "private_chat".to_string(),
            with_whom: "operator".to_string(),
        },
        _ => ConcernType::PersonalInterest {
            topic: summary.to_string(),
            curiosity_level: 0.6,
        },
    }
}

fn normalize_summary(summary: &str) -> String {
    summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn summary_similarity(a: &str, b: &str) -> bool {
    let a = a.to_ascii_lowercase();
    let b = b.to_ascii_lowercase();
    if a == b {
        return true;
    }
    if a.len() >= 10 && b.contains(&a) {
        return true;
    }
    if b.len() >= 10 && a.contains(&b) {
        return true;
    }
    false
}

fn find_existing_concern_index(concerns: &[Concern], summary: &str) -> Option<usize> {
    concerns
        .iter()
        .position(|concern| summary_similarity(&concern.summary, summary))
}

fn merge_related_keys(current: &mut Vec<String>, incoming: &[String]) {
    let mut seen = current
        .iter()
        .map(|key| key.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    for key in incoming {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let lower = key.to_ascii_lowercase();
        if seen.insert(lower) {
            current.push(key.to_string());
        }
    }
}

fn dedupe_related_keys(incoming: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    merge_related_keys(&mut result, incoming);
    result
}

fn merge_notes(current: &mut String, incoming: Option<&str>) {
    let Some(note) = incoming.map(str::trim).filter(|note| !note.is_empty()) else {
        return;
    };
    if current.trim().is_empty() {
        *current = note.to_string();
        return;
    }
    if current.contains(note) {
        return;
    }
    current.push_str("\n");
    current.push_str(note);
}

fn append_key_event(context: &mut ConcernContext, event: String) {
    if event.trim().is_empty() {
        return;
    }
    context.key_events.push(event);
    if context.key_events.len() > 24 {
        let excess = context.key_events.len().saturating_sub(24);
        context.key_events.drain(0..excess);
    }
}

fn text_mentions_concern(haystack_lower: &str, concern: &Concern) -> bool {
    let summary = concern.summary.to_ascii_lowercase();
    if summary.len() >= 4 && haystack_lower.contains(&summary) {
        return true;
    }

    match &concern.concern_type {
        ConcernType::CollaborativeProject { project_name, .. } => {
            let probe = project_name.to_ascii_lowercase();
            probe.len() >= 4 && haystack_lower.contains(&probe)
        }
        ConcernType::HouseholdAwareness { category } => {
            let probe = category.to_ascii_lowercase();
            probe.len() >= 4 && haystack_lower.contains(&probe)
        }
        ConcernType::SystemHealth { component, .. } => {
            let probe = component.to_ascii_lowercase();
            probe.len() >= 4 && haystack_lower.contains(&probe)
        }
        ConcernType::PersonalInterest { topic, .. } => {
            let probe = topic.to_ascii_lowercase();
            probe.len() >= 4 && haystack_lower.contains(&probe)
        }
        ConcernType::Reminder {
            trigger_condition, ..
        } => trigger_condition
            .as_deref()
            .map(str::to_ascii_lowercase)
            .is_some_and(|probe| probe.len() >= 4 && haystack_lower.contains(&probe)),
        ConcernType::OngoingConversation { with_whom, .. } => {
            let probe = with_whom.to_ascii_lowercase();
            probe.len() >= 3 && haystack_lower.contains(&probe)
        }
    }
}

fn truncate_for_log(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in input.chars().enumerate() {
        if i >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, AgentDatabase) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("concerns_manager.db");
        let db = AgentDatabase::new(&db_path).expect("db");
        (dir, db)
    }

    #[test]
    fn salience_decay_thresholds_match_spec() {
        assert_eq!(salience_for_days_since_touch(0), Salience::Active);
        assert_eq!(salience_for_days_since_touch(6), Salience::Active);
        assert_eq!(salience_for_days_since_touch(7), Salience::Monitoring);
        assert_eq!(salience_for_days_since_touch(30), Salience::Background);
        assert_eq!(salience_for_days_since_touch(90), Salience::Dormant);
    }

    #[test]
    fn ingest_decay_and_reactivate_lifecycle() {
        let (_dir, db) = temp_db();

        let signals = vec![ConcernSignal {
            summary: "Ship concern lifecycle manager".to_string(),
            kind: Some("project".to_string()),
            touch_only: false,
            confidence: Some(0.9),
            notes: Some("Track this until phase 5".to_string()),
            related_memory_keys: vec!["phase-plan".to_string()],
        }];

        let ingest =
            ConcernsManager::ingest_signals(&db, &signals, "private_chat").expect("ingest signals");
        assert_eq!(ingest.created.len(), 1);
        assert!(ingest.touched.is_empty());

        let concern_id = ingest.created[0].id.clone();
        let mut concern = db
            .get_concern(&concern_id)
            .expect("load concern")
            .expect("exists");
        assert_eq!(concern.salience, Salience::Active);

        concern.last_touched = Utc::now() - ChronoDuration::days(95);
        db.save_concern(&concern).expect("set stale touch time");

        let decay = ConcernsManager::apply_salience_decay(&db, Utc::now()).expect("decay");
        assert_eq!(decay.to_dormant, 1);
        assert_eq!(decay.total_changes(), 1);

        let dormant = db
            .get_concern(&concern_id)
            .expect("load dormant concern")
            .expect("still exists");
        assert_eq!(dormant.salience, Salience::Dormant);

        let touched = ConcernsManager::touch_from_text(
            &db,
            "Can you revisit ship concern lifecycle manager today?",
            "operator mention",
        )
        .expect("touch from text");
        assert_eq!(touched.len(), 1);

        let reactivated = db
            .get_concern(&concern_id)
            .expect("load reactivated")
            .expect("exists");
        assert_eq!(reactivated.salience, Salience::Active);
    }

    #[test]
    fn priority_context_includes_active_concerns_and_related_memory() {
        let (_dir, db) = temp_db();
        db.set_working_memory("phase-plan", "Finish concerns and loop integration")
            .expect("set memory");

        let signals = vec![ConcernSignal {
            summary: "Loop integration".to_string(),
            kind: Some("project".to_string()),
            touch_only: false,
            confidence: Some(0.9),
            notes: None,
            related_memory_keys: vec!["phase-plan".to_string()],
        }];
        ConcernsManager::ingest_signals(&db, &signals, "test").expect("ingest");

        let context =
            ConcernsManager::build_priority_context(&db, 5, 200).expect("priority context");
        assert!(context.contains("Concern Priority Context"));
        assert!(context.contains("Loop integration"));
        assert!(context.contains("memory:phase-plan"));
    }
}
