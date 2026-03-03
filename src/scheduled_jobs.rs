use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

pub const MIN_SCHEDULE_INTERVAL_MINUTES: u64 = 1;
pub const MAX_SCHEDULE_INTERVAL_MINUTES: u64 = 7 * 24 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub interval_minutes: u64,
    pub conversation_id: String,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ScheduledJob {
    pub fn normalized_interval_minutes(interval_minutes: u64) -> u64 {
        interval_minutes.clamp(
            MIN_SCHEDULE_INTERVAL_MINUTES,
            MAX_SCHEDULE_INTERVAL_MINUTES,
        )
    }

    pub fn next_run_after(from: DateTime<Utc>, interval_minutes: u64) -> DateTime<Utc> {
        from + ChronoDuration::minutes(Self::normalized_interval_minutes(interval_minutes) as i64)
    }

    pub fn queue_message(&self) -> String {
        format!("Scheduled job \"{}\":\n{}", self.name.trim(), self.prompt.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_interval_into_supported_range() {
        assert_eq!(ScheduledJob::normalized_interval_minutes(0), 1);
        assert_eq!(
            ScheduledJob::normalized_interval_minutes(99_999),
            MAX_SCHEDULE_INTERVAL_MINUTES
        );
    }
}
