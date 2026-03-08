use anyhow::Result;
use rusqlite::params;

use crate::agent::journal::{JournalContext, JournalEntry, JournalEntryType, JournalMood};

use super::AgentDatabase;

impl AgentDatabase {
    pub fn add_journal_entry(&self, entry: &JournalEntry) -> Result<()> {
        let related_concerns_json = serde_json::to_string(&entry.related_concerns)
            .map_err(|e| anyhow::anyhow!("Failed to serialize journal related concerns: {}", e))?;
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO journal_entries
             (id, timestamp, entry_type, content, trigger, user_state_at_time, time_of_day,
              related_concerns, mood_valence, mood_arousal, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.id,
                entry.timestamp.to_rfc3339(),
                entry.entry_type.as_db_str(),
                entry.content,
                entry.context.trigger,
                entry.context.user_state_at_time,
                entry.context.time_of_day,
                related_concerns_json,
                entry.mood_at_time.as_ref().map(|m| m.valence),
                entry.mood_at_time.as_ref().map(|m| m.arousal),
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_recent_journal(&self, limit: usize) -> Result<Vec<JournalEntry>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, entry_type, content, trigger, user_state_at_time, time_of_day,
                    related_concerns, mood_valence, mood_arousal
             FROM journal_entries
             ORDER BY timestamp DESC
             LIMIT ?1",
        )?;

        let entries = stmt
            .query_map([limit.max(1)], |row| {
                let timestamp_raw: String = row.get(1)?;
                let related_raw: Option<String> = row.get(7)?;
                let related_concerns = related_raw
                    .as_deref()
                    .map(serde_json::from_str::<Vec<String>>)
                    .transpose()
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
                    .unwrap_or_default();
                let mood_valence: Option<f32> = row.get(8)?;
                let mood_arousal: Option<f32> = row.get(9)?;

                Ok(JournalEntry {
                    id: row.get(0)?,
                    timestamp: timestamp_raw.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    entry_type: JournalEntryType::from_db(&row.get::<_, String>(2)?),
                    content: row.get(3)?,
                    context: JournalContext {
                        trigger: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        user_state_at_time: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        time_of_day: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    },
                    related_concerns,
                    mood_at_time: match (mood_valence, mood_arousal) {
                        (Some(valence), Some(arousal)) => Some(JournalMood { valence, arousal }),
                        _ => None,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    pub fn get_journal_for_context(&self, max_tokens: usize) -> Result<String> {
        if max_tokens == 0 {
            return Ok(String::new());
        }

        let entries = self.get_recent_journal(64)?;
        if entries.is_empty() {
            return Ok(String::new());
        }

        let mut token_budget = 0usize;
        let mut out = String::from("## Recent Journal Notes\n\n");
        for entry in entries {
            let line = format!(
                "- [{}] ({}) {}\n",
                entry.timestamp.format("%Y-%m-%d %H:%M"),
                entry.entry_type.as_db_str(),
                entry.content.trim()
            );
            let est_tokens = line.split_whitespace().count();
            if token_budget + est_tokens > max_tokens {
                break;
            }
            token_budget += est_tokens;
            out.push_str(&line);
        }

        if token_budget == 0 {
            Ok(String::new())
        } else {
            Ok(out)
        }
    }

    pub fn search_journal(&self, query: &str, limit: usize) -> Result<Vec<JournalEntry>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.get_recent_journal(limit.max(1));
        }

        let like_pattern = format!("%{}%", trimmed.to_ascii_lowercase());
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, entry_type, content, trigger, user_state_at_time, time_of_day,
                    related_concerns, mood_valence, mood_arousal
             FROM journal_entries
             WHERE LOWER(content) LIKE ?1 OR LOWER(COALESCE(trigger, '')) LIKE ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        )?;

        let entries = stmt
            .query_map(params![like_pattern, limit.max(1)], |row| {
                let timestamp_raw: String = row.get(1)?;
                let related_raw: Option<String> = row.get(7)?;
                let related_concerns = related_raw
                    .as_deref()
                    .map(serde_json::from_str::<Vec<String>>)
                    .transpose()
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
                    .unwrap_or_default();
                let mood_valence: Option<f32> = row.get(8)?;
                let mood_arousal: Option<f32> = row.get(9)?;

                Ok(JournalEntry {
                    id: row.get(0)?,
                    timestamp: timestamp_raw.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    entry_type: JournalEntryType::from_db(&row.get::<_, String>(2)?),
                    content: row.get(3)?,
                    context: JournalContext {
                        trigger: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        user_state_at_time: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        time_of_day: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    },
                    related_concerns,
                    mood_at_time: match (mood_valence, mood_arousal) {
                        (Some(valence), Some(arousal)) => Some(JournalMood { valence, arousal }),
                        _ => None,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }
}
