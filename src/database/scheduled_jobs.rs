use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::scheduled_jobs::ScheduledJob;

use super::chat::{ChatTurnPhase, DEFAULT_CHAT_SESSION_ID};
use super::AgentDatabase;

impl AgentDatabase {
    pub(super) fn parse_scheduled_job_row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<ScheduledJob> {
        let last_run_at_str: Option<String> = row.get(6)?;
        let next_run_at_str: String = row.get(7)?;
        let created_at_str: String = row.get(8)?;
        let updated_at_str: String = row.get(9)?;

        Ok(ScheduledJob {
            id: row.get(0)?,
            name: row.get(1)?,
            prompt: row.get(2)?,
            interval_minutes: row.get::<_, i64>(3)? as u64,
            conversation_id: row.get(4)?,
            enabled: row.get::<_, i64>(5)? != 0,
            last_run_at: match last_run_at_str {
                Some(value) => Some(value.parse().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?),
                None => None,
            },
            next_run_at: next_run_at_str.parse().map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            created_at: created_at_str.parse().map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            updated_at: updated_at_str.parse().map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        })
    }

    pub fn create_scheduled_job(
        &self,
        name: &str,
        prompt: &str,
        interval_minutes: u64,
    ) -> Result<ScheduledJob> {
        let now = Utc::now();
        let name = name.trim();
        let prompt = prompt.trim();
        let interval_minutes = ScheduledJob::normalized_interval_minutes(interval_minutes);
        let job_id = uuid::Uuid::new_v4().to_string();
        let conversation_id = format!("scheduled-job-{job_id}");
        let title = if name.is_empty() {
            "Schedule: Scheduled job".to_string()
        } else {
            format!("Schedule: {name}")
        };
        let job = ScheduledJob {
            id: job_id,
            name: if name.is_empty() {
                "Scheduled job".to_string()
            } else {
                name.to_string()
            },
            prompt: prompt.to_string(),
            interval_minutes,
            conversation_id: conversation_id.clone(),
            enabled: true,
            last_run_at: None,
            next_run_at: ScheduledJob::next_run_after(now, interval_minutes),
            created_at: now,
            updated_at: now,
        };
        let now_str = now.to_rfc3339();
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO chat_sessions (id, label, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                DEFAULT_CHAT_SESSION_ID,
                "Default session",
                now_str.clone(),
                now_str.clone()
            ],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO chat_conversations (id, session_id, title, created_at, updated_at, runtime_state, active_turn_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                conversation_id,
                DEFAULT_CHAT_SESSION_ID,
                title,
                now_str.clone(),
                now_str.clone(),
                ChatTurnPhase::Idle.as_db_str(),
            ],
        )?;
        conn.execute(
            "INSERT INTO scheduled_jobs
             (id, name, prompt, interval_minutes, conversation_id, enabled, last_run_at, next_run_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9)",
            params![
                &job.id,
                &job.name,
                &job.prompt,
                job.interval_minutes as i64,
                &job.conversation_id,
                1_i64,
                job.next_run_at.to_rfc3339(),
                job.created_at.to_rfc3339(),
                job.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(job)
    }

    pub fn list_scheduled_jobs(&self, limit: usize) -> Result<Vec<ScheduledJob>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt, interval_minutes, conversation_id, enabled, last_run_at, next_run_at, created_at, updated_at
             FROM scheduled_jobs
             ORDER BY enabled DESC, next_run_at ASC
             LIMIT ?1",
        )?;
        let jobs = stmt
            .query_map([limit as i64], Self::parse_scheduled_job_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(jobs)
    }

    pub fn get_scheduled_job(&self, job_id: &str) -> Result<Option<ScheduledJob>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt, interval_minutes, conversation_id, enabled, last_run_at, next_run_at, created_at, updated_at
             FROM scheduled_jobs
             WHERE id = ?1",
        )?;
        let mut rows = stmt.query([job_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(Self::parse_scheduled_job_row(row)?))
    }

    pub fn update_scheduled_job(
        &self,
        job_id: &str,
        name: Option<&str>,
        prompt: Option<&str>,
        interval_minutes: Option<u64>,
        enabled: Option<bool>,
    ) -> Result<Option<ScheduledJob>> {
        let Some(mut job) = self.get_scheduled_job(job_id)? else {
            return Ok(None);
        };

        let now = Utc::now();
        if let Some(name) = name.map(str::trim).filter(|value| !value.is_empty()) {
            job.name = name.to_string();
        }
        if let Some(prompt) = prompt.map(str::trim).filter(|value| !value.is_empty()) {
            job.prompt = prompt.to_string();
        }
        if let Some(interval_minutes) = interval_minutes {
            job.interval_minutes = ScheduledJob::normalized_interval_minutes(interval_minutes);
            job.next_run_at = ScheduledJob::next_run_after(now, job.interval_minutes);
        }
        if let Some(enabled) = enabled {
            job.enabled = enabled;
            if enabled && job.next_run_at <= now {
                job.next_run_at = ScheduledJob::next_run_after(now, job.interval_minutes);
            }
        }
        job.updated_at = now;

        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE scheduled_jobs
             SET name = ?1,
                 prompt = ?2,
                 interval_minutes = ?3,
                 enabled = ?4,
                 next_run_at = ?5,
                 updated_at = ?6
             WHERE id = ?7",
            params![
                &job.name,
                &job.prompt,
                job.interval_minutes as i64,
                if job.enabled { 1_i64 } else { 0_i64 },
                job.next_run_at.to_rfc3339(),
                job.updated_at.to_rfc3339(),
                &job.id,
            ],
        )?;
        conn.execute(
            "UPDATE chat_conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![
                format!("Schedule: {}", job.name),
                job.updated_at.to_rfc3339(),
                job.conversation_id,
            ],
        )?;

        Ok(Some(job))
    }

    pub fn delete_scheduled_job(&self, job_id: &str) -> Result<bool> {
        let conn = self.lock_conn()?;
        let rows = conn.execute("DELETE FROM scheduled_jobs WHERE id = ?1", [job_id])?;
        Ok(rows > 0)
    }

    pub fn next_scheduled_job_due_at(&self) -> Result<Option<DateTime<Utc>>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT next_run_at
             FROM scheduled_jobs
             WHERE enabled = 1
             ORDER BY next_run_at ASC
             LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let next_run_at_raw: String = row.get(0)?;
        let next_run_at = next_run_at_raw.parse::<DateTime<Utc>>()?;
        Ok(Some(next_run_at))
    }

    pub fn take_due_scheduled_jobs(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ScheduledJob>> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT id, name, prompt, interval_minutes, conversation_id, enabled, last_run_at, next_run_at, created_at, updated_at
             FROM scheduled_jobs
             WHERE enabled = 1 AND next_run_at <= ?1
             ORDER BY next_run_at ASC
             LIMIT ?2",
        )?;
        let mut jobs = stmt
            .query_map(
                params![now.to_rfc3339(), limit as i64],
                Self::parse_scheduled_job_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);

        let now_str = now.to_rfc3339();
        for job in &mut jobs {
            let message_id = uuid::Uuid::new_v4().to_string();
            job.last_run_at = Some(now);
            job.next_run_at = ScheduledJob::next_run_after(now, job.interval_minutes);
            job.updated_at = now;
            tx.execute(
                "INSERT OR IGNORE INTO chat_sessions (id, label, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    DEFAULT_CHAT_SESSION_ID,
                    "Default session",
                    now_str.clone(),
                    now_str.clone()
                ],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO chat_conversations
                 (id, session_id, title, created_at, updated_at, runtime_state, active_turn_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                params![
                    job.conversation_id,
                    DEFAULT_CHAT_SESSION_ID,
                    format!("Schedule: {}", job.name),
                    now_str.clone(),
                    now_str.clone(),
                    ChatTurnPhase::Idle.as_db_str(),
                ],
            )?;
            tx.execute(
                "INSERT INTO chat_messages
                 (id, conversation_id, role, content, created_at, processed, turn_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    message_id,
                    job.conversation_id,
                    "scheduled",
                    job.queue_message(),
                    now_str.clone(),
                    0_i64,
                    Option::<String>::None,
                ],
            )?;
            tx.execute(
                "UPDATE chat_conversations
                 SET runtime_state = ?2, active_turn_id = NULL, updated_at = ?3
                 WHERE id = ?1",
                params![
                    job.conversation_id,
                    ChatTurnPhase::Idle.as_db_str(),
                    now_str.clone(),
                ],
            )?;
            tx.execute(
                "UPDATE scheduled_jobs
                 SET last_run_at = ?1, next_run_at = ?2, updated_at = ?3
                 WHERE id = ?4",
                params![
                    now_str.clone(),
                    job.next_run_at.to_rfc3339(),
                    now_str.clone(),
                    &job.id,
                ],
            )?;
        }

        tx.commit()?;
        Ok(jobs)
    }
}
