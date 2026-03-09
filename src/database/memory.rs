use anyhow::{Context, Result};
use rusqlite::params;

use crate::memory::archive::{
    evaluate_promotion_policy, MemoryDesignArchiveEntry, MemoryEvalRunRecord,
    MemoryPromotionDecisionRecord, MemoryPromotionPolicy, PromotionMetricsSnapshot,
    PromotionOutcome,
};
use crate::memory::{
    MemoryDesignVersion, WorkingMemoryEntry, MEMORY_DESIGN_STATE_KEY,
    MEMORY_SCHEMA_VERSION_STATE_KEY,
};

use super::helpers::{
    filter_activity_log_for_conversation, outcome_to_db, short_conversation_tag,
    truncate_for_db_digest,
};
use super::AgentDatabase;

impl AgentDatabase {
    /// Get persisted memory design metadata.
    pub fn get_memory_design_version(&self) -> Result<Option<MemoryDesignVersion>> {
        let design = self.get_state(MEMORY_DESIGN_STATE_KEY)?;
        let version = self.get_state(MEMORY_SCHEMA_VERSION_STATE_KEY)?;

        match (design, version) {
            (None, None) => Ok(None),
            (Some(design_id), Some(version_str)) => {
                let schema_version = version_str.parse::<u32>().with_context(|| {
                    format!(
                        "Invalid {} value: {}",
                        MEMORY_SCHEMA_VERSION_STATE_KEY, version_str
                    )
                })?;
                Ok(Some(MemoryDesignVersion {
                    design_id,
                    schema_version,
                }))
            }
            _ => anyhow::bail!(
                "Incomplete memory design metadata in agent_state: expected both '{}' and '{}'",
                MEMORY_DESIGN_STATE_KEY,
                MEMORY_SCHEMA_VERSION_STATE_KEY
            ),
        }
    }

    /// Persist memory design metadata.
    pub fn set_memory_design_version(&self, design: &MemoryDesignVersion) -> Result<()> {
        self.set_state(MEMORY_DESIGN_STATE_KEY, &design.design_id)?;
        self.set_state(
            MEMORY_SCHEMA_VERSION_STATE_KEY,
            &design.schema_version.to_string(),
        )
    }

    /// Apply a direct memory migration and update persisted version metadata.
    pub fn apply_memory_migration(&self, target: &MemoryDesignVersion) -> Result<()> {
        let current = self
            .get_memory_design_version()?
            .unwrap_or_else(|| self.memory_backend.design_version());

        if current == *target {
            return Ok(());
        }

        let conn = self.lock_conn()?;
        self.migration_registry
            .apply_direct(&conn, &current, target)?;
        drop(conn);

        self.set_memory_design_version(target)
    }

    /// Save a versioned memory design into archive storage.
    pub fn save_memory_design_archive_entry(&self, entry: &MemoryDesignArchiveEntry) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO memory_design_archive
             (id, design_id, schema_version, description, metadata_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.id,
                entry.design_version.design_id,
                entry.design_version.schema_version as i64,
                entry.description,
                entry.metadata_json,
                entry.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// List recent archived memory designs.
    pub fn list_memory_design_archive_entries(
        &self,
        limit: usize,
    ) -> Result<Vec<MemoryDesignArchiveEntry>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, design_id, schema_version, description, metadata_json, created_at
             FROM memory_design_archive
             ORDER BY created_at DESC
             LIMIT ?1",
        )?;

        let rows = stmt
            .query_map([limit as i64], |row| {
                Ok(MemoryDesignArchiveEntry {
                    id: row.get(0)?,
                    design_version: MemoryDesignVersion {
                        design_id: row.get(1)?,
                        schema_version: row.get::<_, i64>(2)? as u32,
                    },
                    description: row.get(3)?,
                    metadata_json: row.get(4)?,
                    created_at: row.get::<_, String>(5)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Persist a memory evaluation run report.
    pub fn save_memory_eval_run(&self, run: &MemoryEvalRunRecord) -> Result<()> {
        let conn = self.lock_conn()?;
        let report_json =
            serde_json::to_string(&run.report).context("Failed to serialize memory eval report")?;
        conn.execute(
            "INSERT OR REPLACE INTO memory_eval_runs (id, trace_set_name, report_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                run.id,
                run.trace_set_name,
                report_json,
                run.created_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Load a memory evaluation run report by ID.
    pub fn get_memory_eval_run(&self, id: &str) -> Result<Option<MemoryEvalRunRecord>> {
        let conn = self.lock_conn()?;
        let result = conn.query_row(
            "SELECT id, trace_set_name, report_json, created_at
             FROM memory_eval_runs
             WHERE id = ?1",
            [id],
            |row| {
                let report_json: String = row.get(2)?;
                let report = serde_json::from_str(&report_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(MemoryEvalRunRecord {
                    id: row.get(0)?,
                    trace_set_name: row.get(1)?,
                    report,
                    created_at: row.get::<_, String>(3)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            },
        );

        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Save a memory promotion decision record.
    ///
    /// Rollback fields are always persisted as NOT NULL columns, ensuring
    /// every decision includes an explicit fallback target.
    pub fn save_memory_promotion_decision(
        &self,
        decision: &MemoryPromotionDecisionRecord,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        let policy_json = serde_json::to_string(&decision.policy)
            .context("Failed to serialize promotion policy")?;
        let metrics_snapshot_json = serde_json::to_string(&decision.metrics_snapshot)
            .context("Failed to serialize promotion metrics snapshot")?;
        conn.execute(
            "INSERT OR REPLACE INTO memory_promotion_decisions
             (id, eval_run_id, candidate_design_id, candidate_schema_version, outcome, rationale,
              policy_json, metrics_snapshot_json, rollback_design_id, rollback_schema_version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                decision.id,
                decision.eval_run_id,
                decision.candidate_design.design_id,
                decision.candidate_design.schema_version as i64,
                outcome_to_db(&decision.outcome),
                decision.rationale,
                policy_json,
                metrics_snapshot_json,
                decision.rollback_target.design_id,
                decision.rollback_target.schema_version as i64,
                decision.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Load a promotion decision by ID.
    pub fn get_memory_promotion_decision(
        &self,
        id: &str,
    ) -> Result<Option<MemoryPromotionDecisionRecord>> {
        let conn = self.lock_conn()?;
        let result = conn.query_row(
            "SELECT id, eval_run_id, candidate_design_id, candidate_schema_version, outcome, rationale,
                    policy_json, metrics_snapshot_json, rollback_design_id, rollback_schema_version, created_at
             FROM memory_promotion_decisions
             WHERE id = ?1",
            [id],
            |row| {
                let policy_json: String = row.get(6)?;
                let metrics_json: String = row.get(7)?;
                let policy: MemoryPromotionPolicy = serde_json::from_str(&policy_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                let metrics_snapshot: PromotionMetricsSnapshot =
                    serde_json::from_str(&metrics_json).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                let outcome_raw: String = row.get(4)?;
                let outcome = match outcome_raw.as_str() {
                    "promote" => PromotionOutcome::Promote,
                    "hold" => PromotionOutcome::Hold,
                    _ => {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("Unknown promotion outcome '{}'", outcome_raw),
                            )),
                        ));
                    }
                };

                Ok(MemoryPromotionDecisionRecord {
                    id: row.get(0)?,
                    eval_run_id: row.get(1)?,
                    candidate_design: MemoryDesignVersion {
                        design_id: row.get(2)?,
                        schema_version: row.get::<_, i64>(3)? as u32,
                    },
                    outcome,
                    rationale: row.get(5)?,
                    policy,
                    metrics_snapshot,
                    rollback_target: MemoryDesignVersion {
                        design_id: row.get(8)?,
                        schema_version: row.get::<_, i64>(9)? as u32,
                    },
                    created_at: row.get::<_, String>(10)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            10,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Evaluate promotion policy against a stored eval run, then persist the decision.
    pub fn evaluate_and_record_memory_promotion(
        &self,
        eval_run_id: &str,
        baseline_backend_id: &str,
        candidate_backend_id: &str,
        policy: &MemoryPromotionPolicy,
    ) -> Result<MemoryPromotionDecisionRecord> {
        let run = self
            .get_memory_eval_run(eval_run_id)?
            .with_context(|| format!("Missing memory eval run '{}'", eval_run_id))?;

        let current_design = self
            .get_memory_design_version()?
            .unwrap_or_else(|| self.memory_backend.design_version());

        let decision = evaluate_promotion_policy(
            &run.id,
            &run.report,
            baseline_backend_id,
            candidate_backend_id,
            &current_design,
            policy,
        )?;
        self.save_memory_promotion_decision(&decision)?;
        Ok(decision)
    }

    /// Recompute a stored promotion decision from archived metrics and policy.
    ///
    /// This is used to verify decision reproducibility from persisted artifacts.
    pub fn recompute_memory_promotion_decision(
        &self,
        decision_id: &str,
    ) -> Result<MemoryPromotionDecisionRecord> {
        let decision = self
            .get_memory_promotion_decision(decision_id)?
            .with_context(|| format!("Missing memory promotion decision '{}'", decision_id))?;
        let run = self
            .get_memory_eval_run(&decision.eval_run_id)?
            .with_context(|| format!("Missing memory eval run '{}'", decision.eval_run_id))?;

        evaluate_promotion_policy(
            &run.id,
            &run.report,
            &decision.metrics_snapshot.baseline_backend_id,
            &decision.metrics_snapshot.candidate_backend_id,
            &decision.rollback_target,
            &decision.policy,
        )
    }

    // ========================================================================
    // Working Memory - Agent's Persistent Scratchpad
    // ========================================================================

    /// Set a working memory entry (creates or updates)
    pub fn set_working_memory(&self, key: &str, content: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        self.memory_backend.set_entry(&conn, key, content)
    }

    /// Get a working memory entry by key
    pub fn get_working_memory(&self, key: &str) -> Result<Option<WorkingMemoryEntry>> {
        let conn = self.lock_conn()?;
        self.memory_backend.get_entry(&conn, key)
    }

    /// Get all working memory entries
    pub fn get_all_working_memory(&self) -> Result<Vec<WorkingMemoryEntry>> {
        let conn = self.lock_conn()?;
        self.memory_backend.list_entries(&conn)
    }

    /// Search working memory entries using a simple relevance rank over key/content text.
    pub fn search_working_memory(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<WorkingMemoryEntry>> {
        let max_results = limit.max(1);
        let trimmed_query = query.trim();
        let mut entries = self.get_all_working_memory()?;
        if trimmed_query.is_empty() {
            entries.truncate(max_results);
            return Ok(entries);
        }

        let query_lower = trimmed_query.to_ascii_lowercase();
        let terms: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .collect();

        let mut ranked = entries
            .into_iter()
            .filter_map(|entry| {
                let key_lower = entry.key.to_ascii_lowercase();
                let content_lower = entry.content.to_ascii_lowercase();

                let mut score: i32 = 0;
                if key_lower.contains(&query_lower) {
                    score += 6;
                }
                if content_lower.contains(&query_lower) {
                    score += 5;
                }
                for term in &terms {
                    if key_lower.contains(term) {
                        score += 3;
                    }
                    if content_lower.contains(term) {
                        score += 1;
                    }
                }

                if score > 0 {
                    Some((score, entry))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        ranked.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.updated_at.cmp(&a.1.updated_at))
        });
        ranked.truncate(max_results);
        Ok(ranked.into_iter().map(|(_, entry)| entry).collect())
    }

    /// Append one line to today's activity log in working memory.
    pub fn append_daily_activity_log(&self, entry: &str) -> Result<()> {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        use chrono::Utc;
        let now = Utc::now();
        let day_key = format!("activity-log-{}", now.format("%Y-%m-%d"));
        let line = format!("- [{} UTC] {}", now.format("%H:%M:%S"), trimmed);
        let existing = self
            .get_working_memory(&day_key)?
            .map(|item| item.content)
            .unwrap_or_default();

        let merged = if existing.trim().is_empty() {
            format!(
                "Daily activity log for {}\n\n{}",
                now.format("%Y-%m-%d"),
                line
            )
        } else {
            format!("{}\n{}", existing.trim_end(), line)
        };

        self.set_working_memory(&day_key, &merged)
    }

    /// Delete a working memory entry
    pub fn delete_working_memory(&self, key: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        self.memory_backend.delete_entry(&conn, key)
    }

    /// Get working memory as a formatted string for inclusion in context
    pub fn get_working_memory_context(&self) -> Result<String> {
        let entries = self.get_all_working_memory()?;
        if entries.is_empty() {
            return Ok(String::new());
        }

        let mut context = String::from("## Your Working Memory (Notes to Self)\n\n");
        for entry in entries {
            context.push_str(&format!("### {}\n{}\n\n", entry.key, entry.content));
        }
        Ok(context)
    }

    /// Get conversation-scoped working memory context to reduce cross-thread noise.
    pub fn get_working_memory_context_for_conversation(
        &self,
        conversation_id: &str,
        max_chars: usize,
    ) -> Result<String> {
        let entries = self.get_all_working_memory()?;
        if entries.is_empty() {
            return Ok(String::new());
        }

        let conversation_tag = short_conversation_tag(conversation_id);
        let mut context = String::from("## Your Working Memory (Notes to Self)\n\n");
        let mut appended = 0usize;

        for entry in entries {
            if entry.content.trim().is_empty() {
                continue;
            }

            if entry.key.starts_with("activity-log-") {
                if let Some(filtered) =
                    filter_activity_log_for_conversation(&entry.content, &conversation_tag, 14)
                {
                    context.push_str(&format!("### {}\n{}\n\n", entry.key, filtered));
                    appended += 1;
                }
            } else {
                context.push_str(&format!(
                    "### {}\n{}\n\n",
                    entry.key,
                    truncate_for_db_digest(entry.content.trim(), 900)
                ));
                appended += 1;
            }

            if context.chars().count() >= max_chars.max(320) {
                context = truncate_for_db_digest(&context, max_chars.max(320));
                break;
            }
        }

        if appended == 0 {
            return Ok(String::new());
        }

        Ok(context)
    }
}
