use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension, Row};

use crate::agent::dream::DreamConsolidation;

use super::AgentDatabase;

impl AgentDatabase {
    pub fn save_dream_consolidation(&self, dream: &DreamConsolidation) -> Result<()> {
        let patterns =
            serde_json::to_string(&dream.patterns).context("failed to serialize Dream patterns")?;
        let tensions = serde_json::to_string(&dream.unresolved_tensions)
            .context("failed to serialize Dream tensions")?;
        let continuities = serde_json::to_string(&dream.continuities)
            .context("failed to serialize Dream continuities")?;
        let cues = serde_json::to_string(&dream.next_orientation_cues)
            .context("failed to serialize Dream orientation cues")?;
        let conn = self.lock_conn()?;
        conn.execute(
            r#"INSERT OR REPLACE INTO dream_consolidations
               (id, created_at, synthesis, patterns_json, unresolved_tensions_json,
                continuities_json, next_orientation_cues_json)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
            params![
                dream.id,
                dream.created_at.to_rfc3339(),
                dream.synthesis,
                patterns,
                tensions,
                continuities,
                cues,
            ],
        )?;
        Ok(())
    }

    pub fn get_latest_dream_consolidation(&self) -> Result<Option<DreamConsolidation>> {
        let conn = self.lock_conn()?;
        Ok(conn
            .query_row(
                r#"SELECT id, created_at, synthesis, patterns_json,
                          unresolved_tensions_json, continuities_json,
                          next_orientation_cues_json
                   FROM dream_consolidations
                   ORDER BY created_at DESC
                   LIMIT 1"#,
                [],
                parse_dream_row,
            )
            .optional()?)
    }

    pub fn get_recent_dream_consolidations(&self, limit: usize) -> Result<Vec<DreamConsolidation>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            r#"SELECT id, created_at, synthesis, patterns_json,
                      unresolved_tensions_json, continuities_json,
                      next_orientation_cues_json
               FROM dream_consolidations
               ORDER BY created_at DESC
               LIMIT ?1"#,
        )?;
        let rows = stmt.query_map([limit.clamp(1, 100)], parse_dream_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

fn parse_dream_row(row: &Row<'_>) -> rusqlite::Result<DreamConsolidation> {
    let created_at_raw: String = row.get(1)?;
    Ok(DreamConsolidation {
        id: row.get(0)?,
        created_at: created_at_raw.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        synthesis: row.get(2)?,
        patterns: parse_json_list(row, 3)?,
        unresolved_tensions: parse_json_list(row, 4)?,
        continuities: parse_json_list(row, 5)?,
        next_orientation_cues: parse_json_list(row, 6)?,
    })
}

fn parse_json_list(row: &Row<'_>, index: usize) -> rusqlite::Result<Vec<String>> {
    let raw: String = row.get(index)?;
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn sample(id: &str, created_at: chrono::DateTime<Utc>) -> DreamConsolidation {
        DreamConsolidation {
            id: id.to_string(),
            created_at,
            synthesis: format!("continuity {id}"),
            patterns: vec!["returning attention".to_string()],
            unresolved_tensions: vec!["care versus interruption".to_string()],
            continuities: vec!["curiosity".to_string()],
            next_orientation_cues: vec!["notice whether the repair holds".to_string()],
        }
    }

    #[test]
    fn persists_and_orders_dream_consolidations() {
        let db = AgentDatabase::new(":memory:").expect("database");
        let now = Utc::now();
        db.save_dream_consolidation(&sample("older", now - Duration::hours(1)))
            .expect("save older");
        db.save_dream_consolidation(&sample("newer", now))
            .expect("save newer");

        let latest = db
            .get_latest_dream_consolidation()
            .expect("latest")
            .expect("record");
        assert_eq!(latest.id, "newer");
        assert_eq!(latest.next_orientation_cues.len(), 1);

        let recent = db.get_recent_dream_consolidations(10).expect("recent");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "newer");
    }
}
