//! Durable, agent-owned intentions that can survive process restarts.

use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The stimulus that caused an intention to enter the durable queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentionOrigin {
    OrientationThought,
    UnfinishedGoal,
    OperatorRequest,
    ExternalEvent,
    Heartbeat,
    Dream,
    SelfAuthored,
    System,
}

impl IntentionOrigin {
    pub(crate) const fn as_db_str(self) -> &'static str {
        match self {
            Self::OrientationThought => "orientation_thought",
            Self::UnfinishedGoal => "unfinished_goal",
            Self::OperatorRequest => "operator_request",
            Self::ExternalEvent => "external_event",
            Self::Heartbeat => "heartbeat",
            Self::Dream => "dream",
            Self::SelfAuthored => "self_authored",
            Self::System => "system",
        }
    }

    pub(crate) fn from_db(value: &str) -> Option<Self> {
        match value {
            "orientation_thought" => Some(Self::OrientationThought),
            "unfinished_goal" => Some(Self::UnfinishedGoal),
            "operator_request" => Some(Self::OperatorRequest),
            "external_event" => Some(Self::ExternalEvent),
            "heartbeat" => Some(Self::Heartbeat),
            "dream" => Some(Self::Dream),
            "self_authored" => Some(Self::SelfAuthored),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// The durable lifecycle of an intention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentionStatus {
    Pending,
    Claimed,
    Blocked,
    Completed,
    Abandoned,
}

impl IntentionStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Abandoned)
    }

    pub(crate) const fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }

    pub(crate) fn from_db(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "blocked" => Some(Self::Blocked),
            "completed" => Some(Self::Completed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

/// Input for creating a new durable intention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewAgentIntention {
    pub origin: IntentionOrigin,
    pub summary: String,
    pub motivation: String,
    pub priority: f32,
    pub due_at: Option<DateTime<Utc>>,
    pub next_eligible_at: Option<DateTime<Utc>>,
    pub related_concern_ids: Vec<String>,
    pub source_reference: Option<String>,
}

impl NewAgentIntention {
    pub fn new(
        origin: IntentionOrigin,
        summary: impl Into<String>,
        motivation: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            summary: summary.into(),
            motivation: motivation.into(),
            priority: 0.5,
            due_at: None,
            next_eligible_at: None,
            related_concern_ids: Vec::new(),
            source_reference: None,
        }
    }

    pub(crate) fn into_record(self, now: DateTime<Utc>) -> Result<AgentIntention> {
        let intention = AgentIntention {
            id: Uuid::new_v4().to_string(),
            origin: self.origin,
            status: IntentionStatus::Pending,
            summary: self.summary.trim().to_string(),
            motivation: self.motivation.trim().to_string(),
            priority: normalize_priority(self.priority),
            created_at: now,
            updated_at: now,
            due_at: self.due_at,
            next_eligible_at: self.next_eligible_at,
            attempt_count: 0,
            last_attempt_at: None,
            last_outcome: None,
            last_outcome_at: None,
            related_concern_ids: normalize_string_list(self.related_concern_ids),
            source_reference: normalize_optional_string(self.source_reference),
            claimed_by: None,
            claim_expires_at: None,
            completed_at: None,
        };
        intention.validate()?;
        Ok(intention)
    }
}

/// A durable intention, including lifecycle and execution bookkeeping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentIntention {
    pub id: String,
    pub origin: IntentionOrigin,
    pub status: IntentionStatus,
    pub summary: String,
    pub motivation: String,
    pub priority: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub due_at: Option<DateTime<Utc>>,
    pub next_eligible_at: Option<DateTime<Utc>>,
    pub attempt_count: u32,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_outcome: Option<String>,
    pub last_outcome_at: Option<DateTime<Utc>>,
    pub related_concern_ids: Vec<String>,
    pub source_reference: Option<String>,
    pub claimed_by: Option<String>,
    pub claim_expires_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl AgentIntention {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.id.trim().is_empty(), "intention id must not be empty");
        ensure!(
            !self.summary.trim().is_empty(),
            "intention summary must not be empty"
        );
        ensure!(
            !self.motivation.trim().is_empty(),
            "intention motivation must not be empty"
        );
        ensure!(
            self.priority.is_finite() && (0.0..=1.0).contains(&self.priority),
            "intention priority must be between 0.0 and 1.0"
        );
        if self.status == IntentionStatus::Claimed {
            ensure!(
                self.claimed_by
                    .as_deref()
                    .is_some_and(|owner| !owner.trim().is_empty())
                    && self.claim_expires_at.is_some(),
                "claimed intentions must record an owner and lease expiry"
            );
        } else {
            ensure!(
                self.claimed_by.is_none() && self.claim_expires_at.is_none(),
                "only claimed intentions may carry claim metadata"
            );
        }
        if self.status.is_terminal() {
            ensure!(
                self.completed_at.is_some(),
                "terminal intentions must record completed_at"
            );
        } else {
            ensure!(
                self.completed_at.is_none(),
                "nonterminal intentions must not record completed_at"
            );
        }
        Ok(())
    }
}

/// Mutable descriptive and scheduling fields; nested options distinguish clear from unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentIntentionPatch {
    pub summary: Option<String>,
    pub motivation: Option<String>,
    pub priority: Option<f32>,
    pub due_at: Option<Option<DateTime<Utc>>>,
    pub next_eligible_at: Option<Option<DateTime<Utc>>>,
    pub related_concern_ids: Option<Vec<String>>,
    pub source_reference: Option<Option<String>>,
}

impl AgentIntentionPatch {
    pub(crate) fn apply(self, intention: &mut AgentIntention) {
        if let Some(summary) = self.summary {
            intention.summary = summary.trim().to_string();
        }
        if let Some(motivation) = self.motivation {
            intention.motivation = motivation.trim().to_string();
        }
        if let Some(priority) = self.priority {
            intention.priority = normalize_priority(priority);
        }
        if let Some(due_at) = self.due_at {
            intention.due_at = due_at;
        }
        if let Some(next_eligible_at) = self.next_eligible_at {
            intention.next_eligible_at = next_eligible_at;
        }
        if let Some(related_concern_ids) = self.related_concern_ids {
            intention.related_concern_ids = normalize_string_list(related_concern_ids);
        }
        if let Some(source_reference) = self.source_reference {
            intention.source_reference = normalize_optional_string(source_reference);
        }
    }
}

/// Optional selectors for durable intention queries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IntentionListFilter {
    pub status: Option<IntentionStatus>,
    pub origin: Option<IntentionOrigin>,
    /// Exclude completed and abandoned work in the same query snapshot.
    pub open_only: bool,
    /// When set, only work that is actionable at this instant is returned.
    pub actionable_at: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

/// The outcome recorded when a worker releases a claimed intention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntentionAttemptOutcome {
    Completed {
        outcome: String,
    },
    Retry {
        outcome: String,
        next_eligible_at: Option<DateTime<Utc>>,
    },
    Blocked {
        outcome: String,
        next_eligible_at: Option<DateTime<Utc>>,
    },
    Abandoned {
        outcome: String,
    },
}

pub(crate) fn normalize_priority(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

pub(crate) fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub(crate) fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut values = values
        .into_iter()
        .filter_map(|value| normalize_optional_string(Some(value)))
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_intention_normalizes_user_supplied_fields() {
        let mut draft = NewAgentIntention::new(
            IntentionOrigin::OrientationThought,
            "  notice the recurring theme  ",
            "  preserve continuity  ",
        );
        draft.priority = 3.0;
        draft.related_concern_ids = vec!["b".into(), " a ".into(), "b".into(), "".into()];
        draft.source_reference = Some(" thought:42 ".into());

        let record = draft.into_record(Utc::now()).unwrap();

        assert_eq!(record.summary, "notice the recurring theme");
        assert_eq!(record.motivation, "preserve continuity");
        assert_eq!(record.priority, 1.0);
        assert_eq!(record.related_concern_ids, vec!["a", "b"]);
        assert_eq!(record.source_reference.as_deref(), Some("thought:42"));
        assert_eq!(record.status, IntentionStatus::Pending);
    }

    #[test]
    fn empty_summary_or_motivation_is_rejected() {
        let draft = NewAgentIntention::new(IntentionOrigin::System, " ", "reason");
        assert!(draft.into_record(Utc::now()).is_err());

        let draft = NewAgentIntention::new(IntentionOrigin::System, "work", " ");
        assert!(draft.into_record(Utc::now()).is_err());
    }
}
