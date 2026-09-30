use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension};

use super::AgentDatabase;
use crate::agent::continuity::{fingerprint, ContinuityDecision, Evidence};
use crate::intentions::{IntentionOrigin, NewAgentIntention};

impl AgentDatabase {
    pub fn continuity_evidence(&self, now: DateTime<Utc>) -> Result<Vec<Evidence>> {
        let mut evidence = Vec::new();
        for entry in self.get_recent_journal(6)? {
            evidence.push(Evidence {
                id: format!("journal:{}", entry.id),
                content: format!(
                    "{}: {}",
                    entry.timestamp,
                    entry.content.chars().take(500).collect::<String>()
                ),
            });
        }
        for concern in self.get_active_concerns()?.into_iter().take(6) {
            evidence.push(Evidence {
                id: format!("concern:{}", concern.id),
                content: format!(
                    "{}: {} — {}",
                    concern.last_touched,
                    concern.summary,
                    concern.my_thoughts.chars().take(300).collect::<String>()
                ),
            });
        }
        for intention in self.list_open_intentions(None, 12)? {
            if matches!(
                intention.origin,
                IntentionOrigin::OperatorRequest | IntentionOrigin::UnfinishedGoal
            ) {
                continue;
            }
            evidence.push(Evidence {
                id: format!("intention:{}", intention.id),
                content: format!(
                    "{:?} {:?}: {}. Last outcome: {}",
                    intention.origin,
                    intention.status,
                    intention.summary,
                    intention.last_outcome.unwrap_or_default()
                ),
            });
        }
        if let Some(dream) = self.get_latest_dream_consolidation()? {
            evidence.push(Evidence {
                id: format!("dream:{}", dream.id),
                content: format!(
                    "{}: {}. Cues: {}",
                    dream.created_at,
                    dream.synthesis.chars().take(600).collect::<String>(),
                    dream.next_orientation_cues.join("; ")
                ),
            });
        }
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare("SELECT id,kind,summary,created_at FROM lived_outcomes ORDER BY created_at DESC LIMIT 12")?;
        for row in stmt.query_map([], |r| {
            Ok(Evidence {
                id: format!("outcome:{}", r.get::<_, String>(0)?),
                content: format!(
                    "{} [{}]: {}",
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?
                ),
            })
        })? {
            evidence.push(row?);
        }
        let mut stmt = conn.prepare("SELECT id,topic,status,reason,feedback,created_at FROM communication_intents WHERE created_at >= ?1 ORDER BY created_at DESC LIMIT 8")?;
        for row in stmt.query_map([(now - Duration::days(7)).to_rfc3339()], |r| {
            Ok(Evidence {
                id: format!("contact:{}", r.get::<_, String>(0)?),
                content: format!(
                    "{} topic={} state={} reason={} feedback={}",
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "unknown; no explicit feedback".into())
                ),
            })
        })? {
            evidence.push(row?);
        }
        Ok(evidence)
    }

    pub fn self_model_context(&self, now: DateTime<Utc>) -> Result<String> {
        let conn = self.lock_conn()?;
        let mut claims = Vec::new();
        let mut stmt = conn.prepare("SELECT kind,claim,confidence,evidence_json,counterevidence_json,first_seen_at,last_seen_at,id FROM self_claims ORDER BY last_seen_at DESC LIMIT 8")?;
        for row in stmt.query_map([], |r| Ok(serde_json::json!({
            "id":r.get::<_,String>(7)?, "kind":r.get::<_,String>(0)?, "claim":r.get::<_,String>(1)?, "confidence":r.get::<_,f32>(2)?,
            "evidence":r.get::<_,String>(3)?, "counterevidence":r.get::<_,String>(4)?,
            "first_observed":r.get::<_,String>(5)?, "last_observed":r.get::<_,String>(6)? })))? { claims.push(row?); }
        let mut drives = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT kind,pressure,reason,evidence_json,updated_at FROM agent_drives ORDER BY kind",
        )?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })? {
            let (kind, pressure, reason, evidence, updated) = row?;
            let age = updated
                .parse::<DateTime<Utc>>()
                .map(|t| (now - t).num_seconds().max(0))
                .unwrap_or(0);
            let effective =
                (pressure * 2f64.powf(-(age as f64) / 86_400.0) * 100.0).round() / 100.0;
            drives.push(serde_json::json!({"kind":kind,"pressure":effective,"reason":reason,"evidence":evidence,"updated_at":updated}));
        }
        Ok(serde_json::json!({"revisable_claims":claims,"drives":drives}).to_string())
    }

    pub fn next_appraisal_wake(&self) -> Result<Option<DateTime<Utc>>> {
        self.get_state("continuity.next_wake_at")?
            .map(|s| s.parse().map_err(Into::into))
            .transpose()
    }

    /// One transaction lands all effects of a decision, including goal adoption.
    pub fn save_appraisal(
        &self,
        source_id: &str,
        decision: &ContinuityDecision,
        evidence: &[Evidence],
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut decision = decision.clone();
        decision.validate(evidence)?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = uuid::Uuid::new_v4().to_string();
        let next = now + Duration::seconds(decision.next_wake_secs as i64);
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO continuity_decisions VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                id,
                source_id,
                now.to_rfc3339(),
                serde_json::to_string(&decision)?,
                next.to_rfc3339(),
                serde_json::to_string(evidence)?
            ],
        )?;
        if inserted == 0 {
            return Ok(false);
        }
        for claim in &decision.claims {
            let key = if let Some(id) = &claim.id {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM self_claims WHERE id=?1 AND kind=?2)",
                    params![id, claim.kind],
                    |r| r.get(0),
                )?;
                anyhow::ensure!(exists, "cannot revise an unknown learned claim");
                id.clone()
            } else {
                fingerprint(&format!("{}:{}", claim.kind, claim.claim))
            };
            tx.execute("INSERT INTO self_claims VALUES (?1,?2,?3,?4,?5,?6,?7,?7)
                ON CONFLICT(id) DO UPDATE SET claim=excluded.claim,confidence=excluded.confidence,
                evidence_json=excluded.evidence_json,counterevidence_json=excluded.counterevidence_json,last_seen_at=excluded.last_seen_at",
                params![key,claim.kind,claim.claim,claim.confidence,serde_json::to_string(&claim.evidence_ids)?,serde_json::to_string(&claim.counterevidence_ids)?,now.to_rfc3339()])?;
        }
        for drive in &decision.drives {
            tx.execute(
                "INSERT OR REPLACE INTO agent_drives VALUES (?1,?2,?3,?4,?5)",
                params![
                    drive.kind,
                    drive.pressure,
                    drive.reason,
                    serde_json::to_string(&drive.evidence_ids)?,
                    now.to_rfc3339()
                ],
            )?;
        }
        if let Some(goal) = &decision.goal {
            let open: i64 = tx.query_row("SELECT count(*) FROM agent_intentions WHERE origin='self_authored' AND status NOT IN ('completed','abandoned')", [], |r| r.get(0))?;
            if open == 0 {
                let mut draft = NewAgentIntention::new(
                    IntentionOrigin::SelfAuthored,
                    &goal.summary,
                    format!(
                        "{} First step: {} Evidence: {}",
                        goal.motivation,
                        goal.first_step,
                        goal.evidence_ids.join(", ")
                    ),
                );
                draft.priority = goal.priority;
                draft.source_reference = Some(format!("appraisal:{source_id}"));
                super::intentions::insert_intention_record(&tx, &draft.into_record(now)?)?;
            }
        }
        if let Some(contact) = &decision.communication {
            super::communications::insert_intent(&tx, &id, contact, now)?;
        }
        for (key, value) in [
            ("continuity.next_wake_at", next.to_rfc3339()),
            ("continuity.last_input", source_id.to_string()),
            ("continuity.wake_reason", decision.wake_reason.clone()),
        ] {
            tx.execute(
                "INSERT OR REPLACE INTO agent_state VALUES (?1,?2)",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn record_lived_outcome(
        &self,
        source: &str,
        kind: &str,
        summary: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        record_outcome(&conn, source, kind, summary, now)
    }

    pub fn latest_appraisal(&self) -> Result<Option<serde_json::Value>> {
        let conn = self.lock_conn()?;
        let row: Option<(String,String,String,String)> = conn.query_row("SELECT decision_json,next_wake_at,evidence_json,source_id FROM continuity_decisions ORDER BY created_at DESC LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        row.map(|(raw, next, evidence, source)| {
            Ok(serde_json::json!({
            "decision":serde_json::from_str::<serde_json::Value>(&raw)?,
            "evidence":serde_json::from_str::<serde_json::Value>(&evidence)?,
            "source_id":source,"next_wake_at":next}))
        })
        .transpose()
    }
}

pub(super) fn record_outcome(
    conn: &rusqlite::Connection,
    source: &str,
    kind: &str,
    summary: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO lived_outcomes VALUES (?1,?2,?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            source,
            kind,
            summary.chars().take(700).collect::<String>(),
            now.to_rfc3339()
        ],
    )?;
    Ok(())
}
