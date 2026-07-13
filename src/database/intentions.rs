use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};

use crate::intentions::{
    AgentIntention, AgentIntentionPatch, IntentionAttemptOutcome, IntentionListFilter,
    IntentionOrigin, IntentionStatus, NewAgentIntention,
};

use super::AgentDatabase;

impl AgentDatabase {
    /// Insert a fully formed intention. Most callers should prefer `create_intention`.
    pub fn insert_intention(&self, intention: &AgentIntention) -> Result<()> {
        intention.validate()?;
        let conn = self.lock_conn()?;
        insert_intention_record(&conn, intention)?;
        Ok(())
    }

    /// Create a pending intention with a fresh id.
    pub fn create_intention(
        &self,
        draft: NewAgentIntention,
        now: DateTime<Utc>,
    ) -> Result<AgentIntention> {
        let intention = draft.into_record(now)?;
        self.insert_intention(&intention)?;
        Ok(intention)
    }

    /// Atomically create a source-backed intention or return the existing record.
    ///
    /// The boolean is true only when this call inserted the record. Drafts without a
    /// source reference always create a new intention.
    pub fn create_intention_if_absent(
        &self,
        draft: NewAgentIntention,
        now: DateTime<Utc>,
    ) -> Result<(AgentIntention, bool)> {
        let intention = draft.into_record(now)?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(source_reference) = intention.source_reference.as_deref() {
            if let Some(existing) =
                query_intention_by_source(&tx, intention.origin, source_reference)?
            {
                tx.commit()?;
                return Ok((existing, false));
            }
        }

        insert_intention_record(&tx, &intention)?;
        tx.commit()?;
        Ok((intention, true))
    }

    pub fn get_intention(&self, id: &str) -> Result<Option<AgentIntention>> {
        let conn = self.lock_conn()?;
        query_intention_by_id(&conn, id)
    }

    pub fn get_intention_by_source(
        &self,
        origin: IntentionOrigin,
        source_reference: &str,
    ) -> Result<Option<AgentIntention>> {
        let source_reference = source_reference.trim();
        if source_reference.is_empty() {
            return Ok(None);
        }
        let conn = self.lock_conn()?;
        query_intention_by_source(&conn, origin, source_reference)
    }

    pub fn list_intentions(&self, filter: &IntentionListFilter) -> Result<Vec<AgentIntention>> {
        let conn = self.lock_conn()?;
        let status = filter.status.map(|value| value.as_db_str().to_string());
        let origin = filter.origin.map(|value| value.as_db_str().to_string());
        let actionable_at = filter.actionable_at.map(|value| value.to_rfc3339());
        let limit = filter.limit.unwrap_or(100).clamp(1, 500) as i64;
        let mut stmt = conn.prepare(
            r#"SELECT id, origin, status, summary, motivation, priority, created_at, updated_at,
                      due_at, next_eligible_at, attempt_count, last_attempt_at, last_outcome,
                      last_outcome_at, related_concern_ids_json, source_reference, claimed_by,
                      claim_expires_at, completed_at
               FROM agent_intentions
               WHERE (?1 IS NULL OR status = ?1)
                 AND (?2 IS NULL OR origin = ?2)
                 AND (?3 = 0 OR status NOT IN ('completed', 'abandoned'))
                 AND (
                    ?4 IS NULL
                    OR (
                        (status = 'pending'
                         OR (status = 'blocked' AND next_eligible_at IS NOT NULL))
                        AND (due_at IS NULL OR due_at <= ?4)
                        AND (next_eligible_at IS NULL OR next_eligible_at <= ?4)
                    )
                 )
               ORDER BY priority DESC, created_at ASC
               LIMIT ?5"#,
        )?;
        let rows = stmt.query_map(
            params![
                status,
                origin,
                i64::from(filter.open_only),
                actionable_at,
                limit
            ],
            parse_intention_row,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// List pending, claimed, and blocked work in one consistent query snapshot.
    pub fn list_open_intentions(
        &self,
        origin: Option<IntentionOrigin>,
        limit: usize,
    ) -> Result<Vec<AgentIntention>> {
        self.list_intentions(&IntentionListFilter {
            origin,
            open_only: true,
            limit: Some(limit),
            ..Default::default()
        })
    }

    /// Update descriptive/provenance/scheduling fields without bypassing lifecycle APIs.
    pub fn update_intention(
        &self,
        id: &str,
        patch: AgentIntentionPatch,
        now: DateTime<Utc>,
    ) -> Result<Option<AgentIntention>> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut intention) = query_intention_by_id(&tx, id)? else {
            tx.commit()?;
            return Ok(None);
        };

        patch.apply(&mut intention);
        intention.updated_at = now;
        intention.validate()?;
        let related_concern_ids_json = serde_json::to_string(&intention.related_concern_ids)
            .context("failed to serialize intention concern ids")?;
        tx.execute(
            r#"UPDATE agent_intentions
               SET summary = ?2,
                   motivation = ?3,
                   priority = ?4,
                   updated_at = ?5,
                   due_at = ?6,
                   next_eligible_at = ?7,
                   related_concern_ids_json = ?8,
                   source_reference = ?9
               WHERE id = ?1"#,
            params![
                intention.id,
                intention.summary,
                intention.motivation,
                intention.priority,
                intention.updated_at.to_rfc3339(),
                intention.due_at.map(|value| value.to_rfc3339()),
                intention.next_eligible_at.map(|value| value.to_rfc3339()),
                related_concern_ids_json,
                intention.source_reference,
            ],
        )?;
        tx.commit()?;
        Ok(Some(intention))
    }

    pub fn delete_intention(&self, id: &str) -> Result<bool> {
        let conn = self.lock_conn()?;
        Ok(conn.execute("DELETE FROM agent_intentions WHERE id = ?1", [id])? > 0)
    }

    /// Claim the highest-priority eligible intention under a bounded worker lease.
    pub fn claim_next_intention(
        &self,
        now: DateTime<Utc>,
        claimed_by: &str,
        lease_duration: Duration,
    ) -> Result<Option<AgentIntention>> {
        let claimed_by = validate_claim_request(claimed_by, lease_duration)?;

        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_expired_claims_in(&tx, now)?;
        let now_raw = now.to_rfc3339();
        let id = tx
            .query_row(
                r#"SELECT id
                   FROM agent_intentions
                   WHERE (status = 'pending'
                          OR (status = 'blocked' AND next_eligible_at IS NOT NULL))
                     AND (due_at IS NULL OR due_at <= ?1)
                     AND (next_eligible_at IS NULL OR next_eligible_at <= ?1)
                   ORDER BY priority DESC, created_at ASC
                   LIMIT 1"#,
                [&now_raw],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        let Some(id) = id else {
            tx.commit()?;
            return Ok(None);
        };
        let claimed = claim_intention_in(&tx, &id, now, claimed_by, lease_duration)?
            .context("selected eligible intention could not be claimed in the same transaction")?;
        tx.commit()?;
        Ok(Some(claimed))
    }

    /// Claim one exact eligible intention without racing a separate queue selection.
    pub fn claim_intention(
        &self,
        id: &str,
        now: DateTime<Utc>,
        claimed_by: &str,
        lease_duration: Duration,
    ) -> Result<Option<AgentIntention>> {
        let claimed_by = validate_claim_request(claimed_by, lease_duration)?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_expired_claims_in(&tx, now)?;
        let claimed = claim_intention_in(&tx, id, now, claimed_by, lease_duration)?;
        tx.commit()?;
        Ok(claimed)
    }

    /// Record an outcome and release a claim only when the same worker still owns it.
    pub fn transition_claimed_intention(
        &self,
        id: &str,
        claimed_by: &str,
        outcome: IntentionAttemptOutcome,
        now: DateTime<Utc>,
    ) -> Result<Option<AgentIntention>> {
        let claimed_by = claimed_by.trim();
        ensure!(!claimed_by.is_empty(), "claim owner must not be empty");

        let (status, outcome_text, next_eligible_at, completed_at) = match outcome {
            IntentionAttemptOutcome::Completed { outcome } => {
                (IntentionStatus::Completed, outcome, None, Some(now))
            }
            IntentionAttemptOutcome::Retry {
                outcome,
                next_eligible_at,
            } => (IntentionStatus::Pending, outcome, next_eligible_at, None),
            IntentionAttemptOutcome::Blocked {
                outcome,
                next_eligible_at,
            } => (IntentionStatus::Blocked, outcome, next_eligible_at, None),
            IntentionAttemptOutcome::Abandoned { outcome } => {
                (IntentionStatus::Abandoned, outcome, None, Some(now))
            }
        };
        let outcome_text = outcome_text.trim();
        ensure!(
            !outcome_text.is_empty(),
            "intention outcome must not be empty"
        );

        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            r#"UPDATE agent_intentions
               SET status = ?3,
                   updated_at = ?4,
                   last_outcome = ?5,
                   last_outcome_at = ?6,
                   next_eligible_at = ?7,
                   claimed_by = NULL,
                   claim_expires_at = NULL,
                   completed_at = ?8
               WHERE id = ?1 AND status = 'claimed' AND claimed_by = ?2"#,
            params![
                id,
                claimed_by,
                status.as_db_str(),
                now.to_rfc3339(),
                outcome_text,
                now.to_rfc3339(),
                next_eligible_at.map(|value| value.to_rfc3339()),
                completed_at.map(|value| value.to_rfc3339()),
            ],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(None);
        }
        let transitioned = query_intention_by_id(&tx, id)?
            .context("transitioned intention disappeared before transaction commit")?;
        tx.commit()?;
        Ok(Some(transitioned))
    }

    /// Release claims whose leases elapsed, making interrupted work available after restart.
    pub fn recover_expired_intention_claims(&self, now: DateTime<Utc>) -> Result<usize> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recovered = recover_expired_claims_in(&tx, now)?;
        tx.commit()?;
        Ok(recovered)
    }
}

fn insert_intention_record(conn: &Connection, intention: &AgentIntention) -> Result<usize> {
    intention.validate()?;
    let related_concern_ids_json = serde_json::to_string(&intention.related_concern_ids)
        .context("failed to serialize intention concern ids")?;
    Ok(conn.execute(
        r#"INSERT INTO agent_intentions
           (id, origin, status, summary, motivation, priority, created_at, updated_at, due_at,
            next_eligible_at, attempt_count, last_attempt_at, last_outcome,
            last_outcome_at, related_concern_ids_json, source_reference, claimed_by,
            claim_expires_at, completed_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                   ?16, ?17, ?18, ?19)"#,
        params![
            intention.id,
            intention.origin.as_db_str(),
            intention.status.as_db_str(),
            intention.summary,
            intention.motivation,
            intention.priority,
            intention.created_at.to_rfc3339(),
            intention.updated_at.to_rfc3339(),
            intention.due_at.map(|value| value.to_rfc3339()),
            intention.next_eligible_at.map(|value| value.to_rfc3339()),
            intention.attempt_count,
            intention.last_attempt_at.map(|value| value.to_rfc3339()),
            intention.last_outcome,
            intention.last_outcome_at.map(|value| value.to_rfc3339()),
            related_concern_ids_json,
            intention.source_reference,
            intention.claimed_by,
            intention.claim_expires_at.map(|value| value.to_rfc3339()),
            intention.completed_at.map(|value| value.to_rfc3339()),
        ],
    )?)
}

fn validate_claim_request(claimed_by: &str, lease_duration: Duration) -> Result<&str> {
    let claimed_by = claimed_by.trim();
    ensure!(!claimed_by.is_empty(), "claim owner must not be empty");
    ensure!(
        lease_duration > Duration::zero(),
        "claim lease duration must be positive"
    );
    Ok(claimed_by)
}

fn claim_intention_in(
    conn: &Connection,
    id: &str,
    now: DateTime<Utc>,
    claimed_by: &str,
    lease_duration: Duration,
) -> Result<Option<AgentIntention>> {
    let now_raw = now.to_rfc3339();
    let changed = conn.execute(
        r#"UPDATE agent_intentions
           SET status = 'claimed',
               claimed_by = ?2,
               claim_expires_at = ?3,
               attempt_count = attempt_count + 1,
               last_attempt_at = ?4,
               updated_at = ?4
           WHERE id = ?1
             AND (status = 'pending'
                  OR (status = 'blocked' AND next_eligible_at IS NOT NULL))
             AND (due_at IS NULL OR due_at <= ?4)
             AND (next_eligible_at IS NULL OR next_eligible_at <= ?4)"#,
        params![id, claimed_by, (now + lease_duration).to_rfc3339(), now_raw,],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    query_intention_by_id(conn, id)
}

fn query_intention_by_id(conn: &Connection, id: &str) -> Result<Option<AgentIntention>> {
    Ok(conn
        .query_row(
            r#"SELECT id, origin, status, summary, motivation, priority, created_at, updated_at,
                      due_at, next_eligible_at, attempt_count, last_attempt_at, last_outcome,
                      last_outcome_at, related_concern_ids_json, source_reference, claimed_by,
                      claim_expires_at, completed_at
               FROM agent_intentions
               WHERE id = ?1"#,
            [id],
            parse_intention_row,
        )
        .optional()?)
}

fn query_intention_by_source(
    conn: &Connection,
    origin: IntentionOrigin,
    source_reference: &str,
) -> Result<Option<AgentIntention>> {
    Ok(conn
        .query_row(
            r#"SELECT id, origin, status, summary, motivation, priority, created_at, updated_at,
                      due_at, next_eligible_at, attempt_count, last_attempt_at, last_outcome,
                      last_outcome_at, related_concern_ids_json, source_reference, claimed_by,
                      claim_expires_at, completed_at
               FROM agent_intentions
               WHERE origin = ?1 AND source_reference = ?2"#,
            params![origin.as_db_str(), source_reference],
            parse_intention_row,
        )
        .optional()?)
}

fn recover_expired_claims_in(conn: &Connection, now: DateTime<Utc>) -> Result<usize> {
    Ok(conn.execute(
        r#"UPDATE agent_intentions
           SET status = 'pending',
               updated_at = ?1,
               last_outcome = 'claim lease expired before an outcome was recorded',
               last_outcome_at = ?1,
               claimed_by = NULL,
               claim_expires_at = NULL,
               completed_at = NULL
           WHERE status = 'claimed'
             AND claim_expires_at IS NOT NULL
             AND claim_expires_at <= ?1"#,
        [now.to_rfc3339()],
    )?)
}

fn parse_intention_row(row: &Row<'_>) -> rusqlite::Result<AgentIntention> {
    let origin_raw: String = row.get(1)?;
    let status_raw: String = row.get(2)?;
    let related_concern_ids_raw: String = row.get(14)?;
    let attempt_count_raw: i64 = row.get(10)?;
    Ok(AgentIntention {
        id: row.get(0)?,
        origin: IntentionOrigin::from_db(&origin_raw)
            .ok_or_else(|| invalid_text_value(1, "unknown intention origin", &origin_raw))?,
        status: IntentionStatus::from_db(&status_raw)
            .ok_or_else(|| invalid_text_value(2, "unknown intention status", &status_raw))?,
        summary: row.get(3)?,
        motivation: row.get(4)?,
        priority: row.get(5)?,
        created_at: parse_required_datetime(row, 6)?,
        updated_at: parse_required_datetime(row, 7)?,
        due_at: parse_optional_datetime(row, 8)?,
        next_eligible_at: parse_optional_datetime(row, 9)?,
        attempt_count: u32::try_from(attempt_count_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        last_attempt_at: parse_optional_datetime(row, 11)?,
        last_outcome: row.get(12)?,
        last_outcome_at: parse_optional_datetime(row, 13)?,
        related_concern_ids: serde_json::from_str(&related_concern_ids_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                14,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        source_reference: row.get(15)?,
        claimed_by: row.get(16)?,
        claim_expires_at: parse_optional_datetime(row, 17)?,
        completed_at: parse_optional_datetime(row, 18)?,
    })
}

fn parse_required_datetime(row: &Row<'_>, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    let raw: String = row.get(index)?;
    raw.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn parse_optional_datetime(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.get(index)?;
    raw.map(|raw| {
        raw.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    })
    .transpose()
}

fn invalid_text_value(index: usize, message: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{message}: {value}"),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ponderer_intentions_{name}_{}.db",
            uuid::Uuid::new_v4()
        ))
    }

    fn draft(summary: &str, priority: f32) -> NewAgentIntention {
        let mut draft = NewAgentIntention::new(
            IntentionOrigin::OrientationThought,
            summary,
            "continue a line of self-directed inquiry",
        );
        draft.priority = priority;
        draft
    }

    #[test]
    fn crud_and_source_idempotency_survive_reopen() {
        let path = temp_db_path("crud");
        let now = Utc::now();
        let id = {
            let db = AgentDatabase::new(&path).unwrap();
            let mut first = draft("trace a recurring pattern", 0.7);
            first.source_reference = Some("pending-thought:42".into());
            first.related_concern_ids = vec!["identity".into()];

            let (created, inserted) = db.create_intention_if_absent(first.clone(), now).unwrap();
            assert!(inserted);
            let (duplicate, inserted) = db
                .create_intention_if_absent(first, now + Duration::seconds(1))
                .unwrap();
            assert!(!inserted);
            assert_eq!(duplicate.id, created.id);

            let updated = db
                .update_intention(
                    &created.id,
                    AgentIntentionPatch {
                        summary: Some("trace the recurring pattern carefully".into()),
                        priority: Some(0.9),
                        due_at: Some(Some(now + Duration::minutes(5))),
                        ..Default::default()
                    },
                    now + Duration::seconds(2),
                )
                .unwrap()
                .unwrap();
            assert_eq!(updated.priority, 0.9);

            let listed = db
                .list_intentions(&IntentionListFilter {
                    origin: Some(IntentionOrigin::OrientationThought),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(listed.len(), 1);
            created.id
        };

        let reopened = AgentDatabase::new(&path).unwrap();
        let persisted = reopened.get_intention(&id).unwrap().unwrap();
        assert_eq!(persisted.summary, "trace the recurring pattern carefully");
        assert!(reopened.delete_intention(&id).unwrap());
        assert!(reopened.get_intention(&id).unwrap().is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn claims_are_atomic_and_respect_priority_and_eligibility() {
        let path = temp_db_path("claim_order");
        let db = AgentDatabase::new(&path).unwrap();
        let now = Utc::now();

        let mut future = draft("future high priority", 1.0);
        future.due_at = Some(now + Duration::hours(1));
        db.create_intention(future, now).unwrap();
        let eligible = db.create_intention(draft("eligible", 0.4), now).unwrap();

        let claimed = db
            .claim_next_intention(now, "orientation-loop", Duration::minutes(5))
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, eligible.id);
        assert_eq!(claimed.status, IntentionStatus::Claimed);
        assert_eq!(claimed.attempt_count, 1);
        assert_eq!(claimed.claimed_by.as_deref(), Some("orientation-loop"));
        assert!(db
            .claim_next_intention(now, "second-worker", Duration::minutes(5))
            .unwrap()
            .is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn exact_claim_owns_the_requested_eligible_intention_only() {
        let path = temp_db_path("exact_claim");
        let db = AgentDatabase::new(&path).unwrap();
        let now = Utc::now();
        let higher_priority = db
            .create_intention(draft("unrelated higher priority", 0.9), now)
            .unwrap();
        let requested = db
            .create_intention(draft("producer-owned work", 0.2), now)
            .unwrap();

        let claimed = db
            .claim_intention(
                &requested.id,
                now,
                "foreground-producer",
                Duration::minutes(5),
            )
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, requested.id);
        assert_eq!(claimed.status, IntentionStatus::Claimed);
        assert_eq!(claimed.attempt_count, 1);
        assert_eq!(claimed.last_attempt_at, Some(now));
        assert_eq!(claimed.claimed_by.as_deref(), Some("foreground-producer"));
        assert_eq!(
            db.get_intention(&higher_priority.id)
                .unwrap()
                .unwrap()
                .status,
            IntentionStatus::Pending
        );
        assert!(db
            .claim_intention(
                &requested.id,
                now,
                "competing-producer",
                Duration::minutes(5),
            )
            .unwrap()
            .is_none());

        let mut future = draft("not due", 1.0);
        future.due_at = Some(now + Duration::minutes(1));
        let future = db.create_intention(future, now).unwrap();
        assert!(db
            .claim_intention(&future.id, now, "foreground-producer", Duration::minutes(5),)
            .unwrap()
            .is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_filter_returns_all_nonterminal_states_in_one_snapshot() {
        let path = temp_db_path("open_filter");
        let db = AgentDatabase::new(&path).unwrap();
        let now = Utc::now();

        let pending = draft("pending", 0.4).into_record(now).unwrap();
        db.insert_intention(&pending).unwrap();

        let mut claimed = draft("claimed", 0.5).into_record(now).unwrap();
        claimed.status = IntentionStatus::Claimed;
        claimed.claimed_by = Some("worker".into());
        claimed.claim_expires_at = Some(now + Duration::minutes(5));
        db.insert_intention(&claimed).unwrap();

        let mut blocked = draft("blocked", 0.6).into_record(now).unwrap();
        blocked.status = IntentionStatus::Blocked;
        db.insert_intention(&blocked).unwrap();

        let mut completed = draft("completed", 0.7).into_record(now).unwrap();
        completed.status = IntentionStatus::Completed;
        completed.completed_at = Some(now);
        db.insert_intention(&completed).unwrap();

        let open = db
            .list_open_intentions(Some(IntentionOrigin::OrientationThought), 10)
            .unwrap();
        assert_eq!(open.len(), 3);
        assert!(open
            .iter()
            .any(|item| item.status == IntentionStatus::Pending));
        assert!(open
            .iter()
            .any(|item| item.status == IntentionStatus::Claimed));
        assert!(open
            .iter()
            .any(|item| item.status == IntentionStatus::Blocked));
        assert!(open.iter().all(|item| !item.status.is_terminal()));

        let actionable = db
            .list_intentions(&IntentionListFilter {
                open_only: true,
                actionable_at: Some(now),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(actionable.len(), 1);
        assert_eq!(actionable[0].id, pending.id);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn transition_requires_owner_and_retry_controls_reeligibility() {
        let path = temp_db_path("transition");
        let db = AgentDatabase::new(&path).unwrap();
        let now = Utc::now();
        db.create_intention(draft("retry me", 0.5), now).unwrap();
        let claimed = db
            .claim_next_intention(now, "worker-a", Duration::minutes(5))
            .unwrap()
            .unwrap();

        assert!(db
            .transition_claimed_intention(
                &claimed.id,
                "worker-b",
                IntentionAttemptOutcome::Completed {
                    outcome: "wrong worker".into(),
                },
                now,
            )
            .unwrap()
            .is_none());

        let eligible_again = now + Duration::minutes(10);
        let retried = db
            .transition_claimed_intention(
                &claimed.id,
                "worker-a",
                IntentionAttemptOutcome::Retry {
                    outcome: "need more context".into(),
                    next_eligible_at: Some(eligible_again),
                },
                now + Duration::minutes(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(retried.status, IntentionStatus::Pending);
        assert!(db
            .claim_next_intention(
                eligible_again - Duration::seconds(1),
                "worker-a",
                Duration::minutes(5),
            )
            .unwrap()
            .is_none());

        let claimed_again = db
            .claim_next_intention(eligible_again, "worker-a", Duration::minutes(5))
            .unwrap()
            .unwrap();
        assert_eq!(claimed_again.attempt_count, 2);
        let completed = db
            .transition_claimed_intention(
                &claimed_again.id,
                "worker-a",
                IntentionAttemptOutcome::Completed {
                    outcome: "integrated the context".into(),
                },
                eligible_again + Duration::minutes(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, IntentionStatus::Completed);
        assert_eq!(
            completed.last_outcome.as_deref(),
            Some("integrated the context")
        );
        assert!(completed.completed_at.is_some());
        assert_eq!(completed.last_outcome_at, completed.completed_at);
        assert!(db
            .list_intentions(&IntentionListFilter {
                origin: Some(IntentionOrigin::OrientationThought),
                open_only: true,
                ..Default::default()
            })
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn expired_claims_are_recovered_after_interruption() {
        let path = temp_db_path("recovery");
        let db = AgentDatabase::new(&path).unwrap();
        let now = Utc::now();
        let created = db
            .create_intention(draft("survive restart", 0.8), now)
            .unwrap();
        db.claim_next_intention(now, "crashed-worker", Duration::minutes(1))
            .unwrap()
            .unwrap();

        assert_eq!(
            db.recover_expired_intention_claims(now + Duration::seconds(59))
                .unwrap(),
            0
        );
        assert_eq!(
            db.recover_expired_intention_claims(now + Duration::minutes(1))
                .unwrap(),
            1
        );
        let recovered = db.get_intention(&created.id).unwrap().unwrap();
        assert_eq!(recovered.status, IntentionStatus::Pending);
        assert!(recovered.claimed_by.is_none());
        assert_eq!(recovered.attempt_count, 1);

        let reclaimed = db
            .claim_next_intention(
                now + Duration::minutes(1),
                "restart-worker",
                Duration::minutes(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(reclaimed.id, created.id);
        assert_eq!(reclaimed.attempt_count, 2);
        let _ = std::fs::remove_file(path);
    }
}
