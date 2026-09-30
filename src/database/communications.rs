use anyhow::{ensure, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;

use super::{AgentDatabase, DEFAULT_CHAT_CONVERSATION_ID};
use crate::agent::continuity::{fingerprint, CommunicationChoice, CommunicationDecision};
use crate::config::OutreachConfig;

#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub id: String,
    pub message_id: String,
    pub intent_id: Option<String>,
    pub endpoint: String,
    pub content: String,
    pub part: i64,
    pub attempts: u32,
    pub lease_token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContactRecord {
    pub id: String,
    pub topic: String,
    pub status: String,
    pub reason: String,
    pub feedback: Option<String>,
}

pub fn telegram_conversation_id(chat_id: i64) -> String {
    format!("telegram:{chat_id}")
}

pub fn is_quiet_hour(policy: &OutreachConfig, hour: u8) -> bool {
    let start = policy.quiet_start_hour.min(23);
    let end = policy.quiet_end_hour.min(23);
    if start == end {
        false
    } else if start < end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

pub(super) fn insert_intent(
    conn: &Connection,
    source: &str,
    decision: &CommunicationDecision,
    now: DateTime<Utc>,
) -> Result<()> {
    let status = if decision.choice == CommunicationChoice::Discard {
        "discarded"
    } else {
        "pending"
    };
    conn.execute(
        "INSERT OR IGNORE INTO communication_intents
        (id,source_id,topic,decision_json,status,reason,created_at,expires_at,next_eligible_at)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            uuid::Uuid::new_v4().to_string(),
            source,
            fingerprint(&decision.topic),
            serde_json::to_string(decision)?,
            status,
            decision.reason,
            now.to_rfc3339(),
            (now + Duration::seconds(decision.expires_after_secs as i64)).to_rfc3339(),
            (now + Duration::seconds(if decision.choice == CommunicationChoice::Defer {
                decision.defer_secs.max(60)
            } else {
                0
            } as i64))
            .to_rfc3339()
        ],
    )?;
    Ok(())
}

impl AgentDatabase {
    pub fn configure_telegram_endpoint(
        &self,
        chat_id: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(id) = chat_id.filter(|id| *id > 0) {
            tx.execute("INSERT INTO contact_endpoints VALUES ('telegram',?1,'operator',1,?2)
                ON CONFLICT(channel) DO UPDATE SET endpoint=excluded.endpoint,enabled=1,
                activated_at=CASE WHEN endpoint=excluded.endpoint AND enabled=1 THEN activated_at ELSE excluded.activated_at END",
                params![id.to_string(),now.to_rfc3339()])?;
            tx.execute("UPDATE delivery_outbox SET state='cancelled',last_error='Owner endpoint changed' WHERE endpoint<>?1 AND state IN ('pending','sending')",[id.to_string()])?;
        } else {
            tx.execute(
                "UPDATE contact_endpoints SET enabled=0 WHERE channel='telegram'",
                [],
            )?;
            tx.execute("UPDATE delivery_outbox SET state='cancelled',last_error='Telegram disabled' WHERE state IN ('pending','sending')",[])?;
        }
        tx.execute("UPDATE communication_intents SET status='cancelled',reason='Telegram endpoint disabled or changed'
            WHERE status='queued' AND id IN (SELECT intent_id FROM delivery_outbox WHERE state='cancelled')",[])?;
        tx.commit()?;
        Ok(())
    }

    pub fn telegram_offset(&self, source: &str) -> Result<i64> {
        let conn = self.lock_conn()?;
        Ok(conn.query_row("SELECT COALESCE(MAX(CAST(event_id AS INTEGER))+1,0) FROM connector_receipts WHERE source=?1",[source],|r|r.get(0))?)
    }

    /// Receipt and authenticated inbound message commit together, before Telegram
    /// is allowed to advance the update cursor. Replays do not create new turns.
    pub fn ingest_telegram_update(
        &self,
        source: &str,
        update_id: i64,
        chat_id: i64,
        text: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authorized: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM contact_endpoints WHERE channel='telegram' AND enabled=1 AND endpoint=?1)",[chat_id.to_string()],|r|r.get(0))?;
        let id = uuid::Uuid::new_v4().to_string();
        let insert = tx.execute(
            "INSERT OR IGNORE INTO connector_receipts VALUES (?1,?2,?3)",
            params![
                source,
                update_id.to_string(),
                if authorized && text.is_some() {
                    Some(&id)
                } else {
                    None
                }
            ],
        )?;
        if insert == 0 {
            return Ok(false);
        }
        if let Some(text) = text.filter(|_| authorized) {
            super::chat::insert_chat_message(
                &tx,
                &id,
                &telegram_conversation_id(chat_id),
                "operator",
                text,
                None,
                now,
            )?;
        }
        tx.commit()?;
        Ok(authorized && text.is_some())
    }

    pub fn telegram_message_is_authorized(
        &self,
        message_id: &str,
        chat_id: Option<i64>,
    ) -> Result<bool> {
        let Some(chat_id) = chat_id.filter(|id| *id > 0) else {
            return Ok(false);
        };
        let conn = self.lock_conn()?;
        Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM connector_receipts r JOIN chat_messages m ON m.id=r.message_id
            JOIN contact_endpoints e ON e.channel='telegram' AND e.enabled=1 AND e.endpoint=?2
            WHERE r.message_id=?1 AND m.conversation_id=?3)",params![message_id,chat_id.to_string(),telegram_conversation_id(chat_id)],|r|r.get(0))?)
    }

    /// One transaction checks durable budgets, records the visible message and
    /// reserves delivery. All spontaneous channels go through this boundary.
    pub fn release_communication_intents(
        &self,
        policy: &OutreachConfig,
        busy: bool,
        local_hour: u8,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE communication_intents SET status='expired',reason='Expired before contact was appropriate' WHERE status='pending' AND expires_at<=?1",[now.to_rfc3339()])?;
        let operator_waiting: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE role='operator' AND processed=0)",
            [],
            |r| r.get(0),
        )?;
        if !policy.enabled || !policy.min_confidence.is_finite() || operator_waiting {
            tx.commit()?;
            return Ok(Vec::new());
        }
        let rows = {
            let mut stmt = tx.prepare("SELECT id,topic,decision_json,expires_at FROM communication_intents WHERE status='pending' AND next_eligible_at<=?1 ORDER BY created_at LIMIT 16")?;
            let rows = stmt
                .query_map([now.to_rfc3339()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut conversations = Vec::new();
        for (id, topic, raw, expiry) in rows {
            let decision: CommunicationDecision = serde_json::from_str(&raw)?;
            if decision.confidence < policy.min_confidence || !decision.confidence.is_finite() {
                tx.execute("UPDATE communication_intents SET status='discarded',reason='Insufficient confidence' WHERE id=?1",[&id])?;
                continue;
            }
            let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM communication_intents WHERE topic=?1 AND reserved_at IS NOT NULL AND reserved_at>=?2)",params![topic,(now-Duration::seconds(policy.topic_cooldown_secs.min(2_592_000) as i64)).to_rfc3339()],|r|r.get(0))?;
            if duplicate {
                tx.execute("UPDATE communication_intents SET status='discarded',reason='Topic already surfaced' WHERE id=?1",[&id])?;
                continue;
            }
            let recent: i64 = tx.query_row("SELECT count(*) FROM communication_intents WHERE reserved_at IS NOT NULL AND reserved_at>=?1",[(now-Duration::days(1)).to_rfc3339()],|r|r.get(0))?;
            let cooldown: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM communication_intents WHERE reserved_at IS NOT NULL AND reserved_at>=?1)",[(now-Duration::seconds(policy.min_interval_secs.min(604_800) as i64)).to_rfc3339()],|r|r.get(0))?;
            let quiet = is_quiet_hour(policy, local_hour)
                && !(decision.urgent && policy.allow_urgent_during_quiet);
            // Dismissed contact reduces contact frequency for the next 24 hours.
            let dismissed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM communication_intents WHERE feedback='dismissed' AND created_at>=?1)",[(now-Duration::days(1)).to_rfc3339()],|r|r.get(0))?;
            let limit = if dismissed {
                policy.max_per_day.min(1)
            } else {
                policy.max_per_day
            };
            if quiet || cooldown || recent >= i64::from(limit) || (busy && !decision.urgent) {
                tx.execute(
                    "UPDATE communication_intents SET reason=?2,next_eligible_at=?3 WHERE id=?1",
                    params![
                        id,
                        if quiet {
                            "Deferred for quiet hours"
                        } else if busy {
                            "Deferred while operator is busy"
                        } else {
                            "Deferred by contact budget"
                        },
                        (now + Duration::minutes(5)).to_rfc3339()
                    ],
                )?;
                continue;
            }
            let use_telegram =
                policy.telegram_enabled && decision.choice != CommunicationChoice::Desktop;
            let endpoint: Option<String> = if use_telegram {
                tx.query_row(
                    "SELECT endpoint FROM contact_endpoints WHERE channel='telegram' AND enabled=1",
                    [],
                    |r| r.get(0),
                )
                .optional()?
            } else {
                None
            };
            let conversation = endpoint
                .as_ref()
                .map(|s| format!("telegram:{s}"))
                .unwrap_or_else(|| DEFAULT_CHAT_CONVERSATION_ID.to_string());
            let message = uuid::Uuid::new_v4().to_string();
            super::chat::insert_chat_message(
                &tx,
                &message,
                &conversation,
                "agent",
                &decision.message,
                None,
                now,
            )?;
            // insert_chat_message enqueues a reply; attach the spontaneous policy
            // reservation and expiry before committing the same transaction.
            tx.execute(
                "UPDATE delivery_outbox SET intent_id=?2,expires_at=?3 WHERE message_id=?1",
                params![message, id, expiry],
            )?;
            tx.execute("UPDATE communication_intents SET status=?2,conversation_id=?3,message_id=?4,reason=?5,reserved_at=?6 WHERE id=?1",params![id,if endpoint.is_some(){"queued"}else{"delivered"},conversation,message,decision.reason,now.to_rfc3339()])?;
            super::continuity::record_outcome(
                &tx,
                &format!("contact-queued:{id}"),
                "communication",
                &format!(
                    "Chose to contact operator about {}: {}",
                    decision.topic, decision.reason
                ),
                now,
            )?;
            conversations.push(conversation);
        }
        tx.commit()?;
        Ok(conversations)
    }

    pub fn recent_contact_records(&self) -> Result<Vec<ContactRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare("SELECT id,json_extract(decision_json,'$.topic'),status,reason,feedback FROM communication_intents ORDER BY created_at DESC LIMIT 20")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ContactRecord {
                    id: r.get(0)?,
                    topic: r.get(1)?,
                    status: r.get(2)?,
                    reason: r.get(3)?,
                    feedback: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn record_contact_feedback(
        &self,
        id: &str,
        feedback: &str,
        endpoint: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        ensure!(
            matches!(feedback, "welcomed" | "dismissed"),
            "unknown feedback"
        );
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE communication_intents SET feedback=?2 WHERE id=?1 AND status IN ('queued','delivered') AND feedback IS NULL
            AND (?3 IS NULL OR conversation_id=?3)",params![id,feedback,endpoint.map(telegram_conversation_id)])?;
        if changed > 0 {
            super::continuity::record_outcome(
                &tx,
                &format!("feedback:{id}"),
                "contact_feedback",
                &format!("Operator explicitly {feedback} contact {id}"),
                now,
            )?;
            tx.execute("UPDATE agent_drives SET pressure=MAX(0,pressure-0.4),reason=?1,updated_at=?2 WHERE kind='connection'",params![format!("Contact {feedback}; allow space"),now.to_rfc3339()])?;
            tx.execute(
                "INSERT OR REPLACE INTO agent_state VALUES ('continuity.next_wake_at',?1)",
                [now.to_rfc3339()],
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn claim_delivery(
        &self,
        endpoint: i64,
        policy: &OutreachConfig,
        hour: u8,
        paused: bool,
        now: DateTime<Utc>,
    ) -> Result<Option<Delivery>> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A process may have died after Telegram accepted the request. Never
        // blindly resend an ambiguous delivery (Telegram has no idempotency key).
        tx.execute("UPDATE delivery_outbox SET state='uncertain',last_error='Delivery interrupted; receipt unknown' WHERE state='sending' AND lease_until<=?1",[now.to_rfc3339()])?;
        tx.execute(
            "UPDATE delivery_outbox SET state='expired' WHERE state='pending' AND expires_at<=?1",
            [now.to_rfc3339()],
        )?;
        tx.execute("UPDATE communication_intents SET status='uncertain' WHERE id IN (SELECT intent_id FROM delivery_outbox WHERE state='uncertain') AND status='queued'",[])?;
        tx.execute("UPDATE communication_intents SET status='expired' WHERE id IN (SELECT intent_id FROM delivery_outbox WHERE state='expired') AND status='queued'",[])?;
        if paused {
            tx.commit()?;
            return Ok(None);
        }
        let allow_spontaneous = policy.enabled
            && policy.telegram_enabled
            && policy.max_per_day > 0
            && policy.min_confidence.is_finite();
        let row = tx.query_row("SELECT o.id,o.message_id,o.intent_id,o.endpoint,o.content,o.part,o.attempts
            FROM delivery_outbox o JOIN contact_endpoints e ON e.channel='telegram' AND e.enabled=1 AND e.endpoint=o.endpoint
            LEFT JOIN communication_intents c ON c.id=o.intent_id
            WHERE o.state='pending' AND o.endpoint=?1 AND o.next_attempt_at<=?2 AND o.expires_at>?2
            AND (o.intent_id IS NULL OR (?3 AND json_extract(c.decision_json,'$.confidence')>=?6 AND (?4=0 OR (?5 AND json_extract(c.decision_json,'$.urgent')=1))))
            AND (o.intent_id IS NULL OR NOT EXISTS(SELECT 1 FROM chat_messages WHERE role='operator' AND processed=0))
            AND NOT EXISTS(SELECT 1 FROM delivery_outbox p WHERE p.message_id=o.message_id AND p.part<o.part AND p.state<>'sent')
            ORDER BY o.created_at,o.part LIMIT 1",params![endpoint.to_string(),now.to_rfc3339(),allow_spontaneous,is_quiet_hour(policy,hour),policy.allow_urgent_during_quiet,policy.min_confidence],|r|Ok(Delivery{
                id:r.get(0)?,message_id:r.get(1)?,intent_id:r.get(2)?,endpoint:r.get(3)?,content:r.get(4)?,part:r.get(5)?,attempts:r.get(6)?,lease_token:uuid::Uuid::new_v4().to_string()})).optional()?;
        if let Some(d) = &row {
            tx.execute("UPDATE delivery_outbox SET state='sending',attempts=attempts+1,lease_token=?2,lease_until=?3 WHERE id=?1",params![d.id,d.lease_token,(now+Duration::seconds(120)).to_rfc3339()])?;
        }
        tx.commit()?;
        Ok(row)
    }

    pub fn settle_delivery(
        &self,
        delivery: &Delivery,
        state: &str,
        provider_id: Option<i64>,
        error: Option<&str>,
        retry_secs: u64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        ensure!(
            matches!(state, "sent" | "pending" | "failed" | "uncertain"),
            "invalid delivery state"
        );
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE delivery_outbox SET state=?3,provider_message_id=?4,last_error=?5,next_attempt_at=?6,lease_until=NULL,lease_token=NULL
            WHERE id=?1 AND state='sending' AND lease_token=?2",params![delivery.id,delivery.lease_token,state,provider_id.map(|id|id.to_string()),error,(now+Duration::seconds(retry_secs.clamp(1,86_400) as i64)).to_rfc3339()])?;
        if changed > 0 {
            if let Some(id) = &delivery.intent_id {
                let remaining: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM delivery_outbox WHERE intent_id=?1 AND state<>'sent')",[id],|r|r.get(0))?;
                tx.execute(
                    "UPDATE communication_intents SET status=?2 WHERE id=?1",
                    params![
                        id,
                        match state {
                            "sent" if !remaining => "delivered",
                            "sent" | "pending" => "queued",
                            "uncertain" => "uncertain",
                            _ => "failed",
                        }
                    ],
                )?;
                super::continuity::record_outcome(
                    &tx,
                    &format!("delivery:{}:{}", delivery.id, delivery.attempts),
                    "delivery",
                    &format!(
                        "Contact {id}: {state}; {}",
                        error.unwrap_or("confirmed by Telegram")
                    ),
                    now,
                )?;
            }
        }
        tx.commit()?;
        Ok(changed > 0)
    }
}

/// Called inside the chat transaction, for both foreground and background replies.
pub(super) fn enqueue_reply(
    conn: &Connection,
    message_id: &str,
    conversation_id: &str,
    content: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let Some(endpoint) = conversation_id.strip_prefix("telegram:") else {
        return Ok(());
    };
    let active: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM contact_endpoints WHERE channel='telegram' AND enabled=1 AND endpoint=?1)",[endpoint],|r|r.get(0))?;
    if !active {
        return Ok(());
    }
    let mut visible = content.to_string();
    for (start, end) in [
        ("[thinking]", "[/thinking]"),
        ("[tool_calls]", "[/tool_calls]"),
        ("[media]", "[/media]"),
        ("[turn_control]", "[/turn_control]"),
        ("[concerns]", "[/concerns]"),
        ("[intention_status]", "[/intention_status]"),
        ("<think>", "</think>"),
        ("<thinking>", "</thinking>"),
    ] {
        visible = super::helpers::extract_tagged_blocks(&visible, start, end).0;
    }
    for (part, chunk) in split_telegram_text(visible.trim()).iter().enumerate() {
        conn.execute("INSERT OR IGNORE INTO delivery_outbox (id,message_id,endpoint,part,content,next_attempt_at,expires_at,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?6)",params![uuid::Uuid::new_v4().to_string(),message_id,endpoint,part as i64,chunk,now.to_rfc3339(),(now+Duration::days(1)).to_rfc3339()])?;
    }
    Ok(())
}

pub fn split_telegram_text(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut units = 0;
    for ch in text.chars() {
        // Count UTF-16 units conservatively so emoji never exceed Telegram limits.
        if units + ch.len_utf16() > 4096 {
            parts.push(std::mem::take(&mut current));
            units = 0;
        }
        current.push(ch);
        units += ch.len_utf16();
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}
