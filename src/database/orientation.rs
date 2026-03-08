use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::AgentDatabase;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrientationSnapshotRecord {
    pub id: String,
    pub timestamp: chrono::DateTime<Utc>,
    pub user_state: serde_json::Value,
    pub disposition: String,
    pub synthesis: String,
    pub salience_map: serde_json::Value,
    pub anomalies: serde_json::Value,
    pub pending_thoughts: serde_json::Value,
    pub mood_valence: Option<f32>,
    pub mood_arousal: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingThoughtRecord {
    pub id: String,
    pub content: String,
    pub context: Option<String>,
    pub priority: f32,
    pub relates_to: Vec<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub surfaced_at: Option<chrono::DateTime<Utc>>,
    pub dismissed_at: Option<chrono::DateTime<Utc>>,
}

impl AgentDatabase {
    pub fn save_orientation_snapshot(&self, orientation: &OrientationSnapshotRecord) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO orientation_snapshots
             (id, timestamp, user_state, disposition, synthesis, salience_map, anomalies,
              pending_thoughts, mood_valence, mood_arousal, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                orientation.id,
                orientation.timestamp.to_rfc3339(),
                serde_json::to_string(&orientation.user_state)
                    .context("Failed to serialize orientation user_state")?,
                orientation.disposition,
                orientation.synthesis,
                serde_json::to_string(&orientation.salience_map)
                    .context("Failed to serialize orientation salience_map")?,
                serde_json::to_string(&orientation.anomalies)
                    .context("Failed to serialize orientation anomalies")?,
                serde_json::to_string(&orientation.pending_thoughts)
                    .context("Failed to serialize orientation pending_thoughts")?,
                orientation.mood_valence,
                orientation.mood_arousal,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_recent_orientations(&self, limit: usize) -> Result<Vec<OrientationSnapshotRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, user_state, disposition, synthesis, salience_map, anomalies,
                    pending_thoughts, mood_valence, mood_arousal
             FROM orientation_snapshots
             ORDER BY timestamp DESC
             LIMIT ?1",
        )?;
        let snapshots = stmt
            .query_map([limit.max(1)], |row| {
                let timestamp_raw: String = row.get(1)?;
                let user_state_raw: String = row.get(2)?;
                let salience_map_raw: Option<String> = row.get(5)?;
                let anomalies_raw: Option<String> = row.get(6)?;
                let pending_thoughts_raw: Option<String> = row.get(7)?;
                Ok(OrientationSnapshotRecord {
                    id: row.get(0)?,
                    timestamp: timestamp_raw.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    user_state: serde_json::from_str(&user_state_raw).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    disposition: row.get(3)?,
                    synthesis: row.get(4)?,
                    salience_map: match salience_map_raw {
                        Some(raw) => serde_json::from_str(&raw).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        None => serde_json::json!([]),
                    },
                    anomalies: match anomalies_raw {
                        Some(raw) => serde_json::from_str(&raw).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        None => serde_json::json!([]),
                    },
                    pending_thoughts: match pending_thoughts_raw {
                        Some(raw) => serde_json::from_str(&raw).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                7,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        None => serde_json::json!([]),
                    },
                    mood_valence: row.get(8)?,
                    mood_arousal: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(snapshots)
    }

    // ========================================================================
    // Living Loop Foundation - Pending Thought Queue
    // ========================================================================

    pub fn queue_pending_thought(&self, thought: &PendingThoughtRecord) -> Result<()> {
        let conn = self.lock_conn()?;
        let relates_to_json = serde_json::to_string(&thought.relates_to)
            .context("Failed to serialize pending_thought relates_to")?;
        conn.execute(
            "INSERT OR REPLACE INTO pending_thoughts_queue
             (id, content, context, priority, relates_to, created_at, surfaced_at, dismissed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                thought.id,
                thought.content,
                thought.context,
                thought.priority,
                relates_to_json,
                thought.created_at.to_rfc3339(),
                thought.surfaced_at.map(|v| v.to_rfc3339()),
                thought.dismissed_at.map(|v| v.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn get_unsurfaced_thoughts(&self) -> Result<Vec<PendingThoughtRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, content, context, priority, relates_to, created_at, surfaced_at, dismissed_at
             FROM pending_thoughts_queue
             WHERE surfaced_at IS NULL AND dismissed_at IS NULL
             ORDER BY priority DESC, created_at ASC",
        )?;
        let thoughts = stmt
            .query_map([], |row| {
                let relates_to_raw: Option<String> = row.get(4)?;
                let created_at_raw: String = row.get(5)?;
                let surfaced_at_raw: Option<String> = row.get(6)?;
                let dismissed_at_raw: Option<String> = row.get(7)?;
                Ok(PendingThoughtRecord {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    context: row.get(2)?,
                    priority: row.get(3)?,
                    relates_to: relates_to_raw
                        .as_deref()
                        .map(serde_json::from_str::<Vec<String>>)
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                4,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?
                        .unwrap_or_default(),
                    created_at: created_at_raw.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    surfaced_at: match surfaced_at_raw {
                        Some(raw) => Some(raw.parse().map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?),
                        None => None,
                    },
                    dismissed_at: match dismissed_at_raw {
                        Some(raw) => Some(raw.parse().map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                7,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?),
                        None => None,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(thoughts)
    }

    pub fn mark_thought_surfaced(&self, id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE pending_thoughts_queue
             SET surfaced_at = ?2
             WHERE id = ?1",
            params![id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn dismiss_thought(&self, id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE pending_thoughts_queue
             SET dismissed_at = ?2
             WHERE id = ?1",
            params![id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}
