use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use crate::agent::concerns::{Concern, ConcernContext, ConcernType, Salience};

use super::AgentDatabase;

impl AgentDatabase {
    pub fn save_concern(&self, concern: &Concern) -> Result<()> {
        let concern_type_json = serde_json::to_string(&concern.concern_type)
            .map_err(|e| anyhow::anyhow!("Failed to serialize concern type: {}", e))?;
        let related_keys_json =
            serde_json::to_string(&concern.related_memory_keys).map_err(|e| {
                anyhow::anyhow!("Failed to serialize concern related memory keys: {}", e)
            })?;
        let context_json = serde_json::to_string(&concern.context)
            .map_err(|e| anyhow::anyhow!("Failed to serialize concern context: {}", e))?;

        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO concerns
             (id, created_at, last_touched, summary, concern_type, salience, my_thoughts,
              related_memory_keys, context, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                concern.id,
                concern.created_at.to_rfc3339(),
                concern.last_touched.to_rfc3339(),
                concern.summary,
                concern_type_json,
                concern.salience.as_db_str(),
                concern.my_thoughts,
                related_keys_json,
                context_json,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_concern(&self, id: &str) -> Result<Option<Concern>> {
        let conn = self.lock_conn()?;
        let result = conn.query_row(
            "SELECT id, created_at, last_touched, summary, concern_type, salience, my_thoughts,
                    related_memory_keys, context
             FROM concerns
             WHERE id = ?1",
            [id],
            |row| {
                let created_raw: String = row.get(1)?;
                let touched_raw: String = row.get(2)?;
                let concern_type_raw: String = row.get(4)?;
                let salience_raw: String = row.get(5)?;
                let related_raw: Option<String> = row.get(7)?;
                let context_raw: Option<String> = row.get(8)?;

                let concern_type: ConcernType =
                    serde_json::from_str(&concern_type_raw).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                let related_memory_keys = related_raw
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
                let context: ConcernContext = context_raw
                    .as_deref()
                    .map(serde_json::from_str::<ConcernContext>)
                    .transpose()
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
                    .unwrap_or_default();

                Ok(Concern {
                    id: row.get(0)?,
                    created_at: created_raw.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    last_touched: touched_raw.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    summary: row.get(3)?,
                    concern_type,
                    salience: Salience::from_db(&salience_raw),
                    my_thoughts: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    related_memory_keys,
                    context,
                })
            },
        );

        match result {
            Ok(concern) => Ok(Some(concern)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_active_concerns(&self) -> Result<Vec<Concern>> {
        let ids = {
            let conn = self.lock_conn()?;
            let mut stmt = conn.prepare(
                "SELECT id
                 FROM concerns
                 WHERE salience IN (?1, ?2)
                 ORDER BY last_touched DESC",
            )?;
            let rows = stmt.query_map(
                params![
                    Salience::Active.as_db_str(),
                    Salience::Monitoring.as_db_str()
                ],
                |row| row.get::<_, String>(0),
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut concerns = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(concern) = self.get_concern(&id)? {
                concerns.push(concern);
            }
        }
        Ok(concerns)
    }

    pub fn get_all_concerns(&self) -> Result<Vec<Concern>> {
        let ids = {
            let conn = self.lock_conn()?;
            let mut stmt = conn.prepare("SELECT id FROM concerns ORDER BY last_touched DESC")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut concerns = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(concern) = self.get_concern(&id)? {
                concerns.push(concern);
            }
        }
        Ok(concerns)
    }

    pub fn update_concern_salience(&self, id: &str, salience: Salience) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE concerns
             SET salience = ?2, updated_at = ?3
             WHERE id = ?1",
            params![id, salience.as_db_str(), Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn touch_concern(&self, id: &str, reason: &str) -> Result<()> {
        let Some(mut concern) = self.get_concern(id)? else {
            return Ok(());
        };
        concern.last_touched = Utc::now();
        concern.context.last_update_reason = reason.to_string();
        self.save_concern(&concern)
    }
}
