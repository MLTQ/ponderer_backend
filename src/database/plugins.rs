use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::AgentDatabase;
use crate::plugin_contract::PluginStateMutation;

const MAX_EVENT_PAGE_SIZE: usize = 1_000;
pub const MAX_PLUGIN_EVENT_PAYLOAD_BYTES: usize = 256 * 1024;
pub const MAX_PLUGIN_STATE_VALUE_BYTES: usize = 256 * 1024;
pub const MAX_PLUGIN_STATE_TOTAL_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PLUGIN_STATE_KEYS: usize = 1_024;
const MAX_PLUGIN_IDENTIFIER_BYTES: usize = 256;
const MAX_DEAD_LETTER_REASON_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginStateRecord {
    pub plugin_id: String,
    pub key: String,
    pub schema_version: u32,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewPluginEvent {
    pub event_id: String,
    pub event_type: String,
    pub schema_version: u32,
    pub source: String,
    #[serde(default)]
    pub source_event_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub causation_id: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginEventRecord {
    pub sequence: i64,
    pub event_id: String,
    pub event_type: String,
    pub schema_version: u32,
    pub source: String,
    pub source_event_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginEventCursor {
    pub plugin_id: String,
    pub subscription: String,
    pub last_sequence: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginEventDeliveryReceipt {
    pub delivery_token: String,
    pub from_sequence: i64,
    pub through_sequence: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginEventPage {
    pub records: Vec<PluginEventRecord>,
    pub through_sequence: i64,
    pub quarantined_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginEventDeliveryBatch {
    pub records: Vec<PluginEventRecord>,
    pub receipt: Option<PluginEventDeliveryReceipt>,
    pub quarantined_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginEventDeadLetter {
    pub sequence: i64,
    pub event_id: String,
    pub event_type: String,
    pub schema_version: String,
    pub source: String,
    pub source_event_id: Option<String>,
    pub occurred_at: String,
    pub recorded_at: String,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub payload_json: String,
    pub payload_truncated: bool,
    pub reason: String,
    pub quarantined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginEventRetentionPolicy {
    pub acknowledged_event_age: chrono::Duration,
    pub unconsumed_event_age: chrono::Duration,
    pub dead_letter_age: chrono::Duration,
}

impl Default for PluginEventRetentionPolicy {
    fn default() -> Self {
        Self {
            acknowledged_event_age: chrono::Duration::days(7),
            unconsumed_event_age: chrono::Duration::days(90),
            dead_letter_age: chrono::Duration::days(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginEventCompactionReport {
    pub acknowledged_events_deleted: usize,
    pub expired_unconsumed_events_deleted: usize,
    pub dead_letters_deleted: usize,
}

impl AgentDatabase {
    pub fn apply_plugin_state_mutations(
        &self,
        plugin_id: &str,
        mutations: &[PluginStateMutation],
    ) -> Result<()> {
        validate_identifier("plugin id", plugin_id)?;
        if mutations.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        apply_plugin_state_mutations_in_transaction(
            &transaction,
            plugin_id,
            mutations,
            Utc::now(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn put_plugin_state(
        &self,
        plugin_id: &str,
        key: &str,
        schema_version: u32,
        value: &Value,
    ) -> Result<PluginStateRecord> {
        validate_identifier("plugin id", plugin_id)?;
        validate_identifier("plugin state key", key)?;
        if schema_version == 0 {
            anyhow::bail!("plugin state schema version must be at least 1");
        }
        let updated_at = Utc::now();
        let value_json = serde_json::to_string(value).context("serialize plugin state value")?;
        if value_json.len() > MAX_PLUGIN_STATE_VALUE_BYTES {
            anyhow::bail!(
                "plugin state value is {} bytes; maximum is {} bytes",
                value_json.len(),
                MAX_PLUGIN_STATE_VALUE_BYTES
            );
        }
        let conn = self.lock_conn()?;
        let (key_count, total_bytes, existing_bytes) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(value_json)), 0),
                    COALESCE(MAX(CASE WHEN key = ?2 THEN LENGTH(value_json) END), 0)
             FROM plugin_state WHERE plugin_id = ?1",
            params![plugin_id, key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        if existing_bytes == 0 && key_count >= MAX_PLUGIN_STATE_KEYS as i64 {
            anyhow::bail!(
                "plugin state namespace has reached its {} key limit",
                MAX_PLUGIN_STATE_KEYS
            );
        }
        let projected_bytes = total_bytes
            .saturating_sub(existing_bytes)
            .saturating_add(value_json.len() as i64);
        if projected_bytes > MAX_PLUGIN_STATE_TOTAL_BYTES as i64 {
            anyhow::bail!(
                "plugin state namespace would exceed its {} byte limit",
                MAX_PLUGIN_STATE_TOTAL_BYTES
            );
        }
        conn.execute(
            "INSERT INTO plugin_state (plugin_id, key, schema_version, value_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(plugin_id, key) DO UPDATE SET
                 schema_version = excluded.schema_version,
                 value_json = excluded.value_json,
                 updated_at = excluded.updated_at",
            params![
                plugin_id,
                key,
                i64::from(schema_version),
                value_json,
                updated_at.to_rfc3339()
            ],
        )?;
        Ok(PluginStateRecord {
            plugin_id: plugin_id.to_string(),
            key: key.to_string(),
            schema_version,
            value: value.clone(),
            updated_at,
        })
    }

    pub fn get_plugin_state(
        &self,
        plugin_id: &str,
        key: &str,
    ) -> Result<Option<PluginStateRecord>> {
        validate_identifier("plugin id", plugin_id)?;
        validate_identifier("plugin state key", key)?;
        let conn = self.lock_conn()?;
        let row = conn
            .query_row(
                "SELECT plugin_id, key, schema_version, value_json, updated_at
                 FROM plugin_state WHERE plugin_id = ?1 AND key = ?2",
                params![plugin_id, key],
                map_plugin_state_row,
            )
            .optional()?;
        row.map(decode_plugin_state).transpose()
    }

    pub fn list_plugin_state(&self, plugin_id: &str) -> Result<Vec<PluginStateRecord>> {
        validate_identifier("plugin id", plugin_id)?;
        let conn = self.lock_conn()?;
        let mut statement = conn.prepare(
            "SELECT plugin_id, key, schema_version, value_json, updated_at
             FROM plugin_state WHERE plugin_id = ?1 ORDER BY key ASC",
        )?;
        let encoded = statement
            .query_map([plugin_id], map_plugin_state_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        encoded.into_iter().map(decode_plugin_state).collect()
    }

    pub fn delete_plugin_state(&self, plugin_id: &str, key: &str) -> Result<bool> {
        validate_identifier("plugin id", plugin_id)?;
        validate_identifier("plugin state key", key)?;
        let conn = self.lock_conn()?;
        Ok(conn.execute(
            "DELETE FROM plugin_state WHERE plugin_id = ?1 AND key = ?2",
            params![plugin_id, key],
        )? > 0)
    }

    pub fn append_plugin_event(&self, event: &NewPluginEvent) -> Result<PluginEventRecord> {
        validate_identifier("plugin event id", &event.event_id)?;
        validate_identifier("plugin event type", &event.event_type)?;
        validate_identifier("plugin event source", &event.source)?;
        if event.schema_version == 0 {
            anyhow::bail!("plugin event schema version must be at least 1");
        }
        let source_event_id = normalize_optional_identifier(event.source_event_id.as_deref());
        let payload_json =
            serde_json::to_string(&event.payload).context("serialize plugin event payload")?;
        if payload_json.len() > MAX_PLUGIN_EVENT_PAYLOAD_BYTES {
            anyhow::bail!(
                "plugin event payload is {} bytes; maximum is {} bytes",
                payload_json.len(),
                MAX_PLUGIN_EVENT_PAYLOAD_BYTES
            );
        }
        let recorded_at = Utc::now();
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO plugin_events
             (event_id, event_type, schema_version, source, source_event_id, occurred_at,
              recorded_at, correlation_id, causation_id, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.event_id,
                event.event_type,
                i64::from(event.schema_version),
                event.source,
                source_event_id,
                event.occurred_at.to_rfc3339(),
                recorded_at.to_rfc3339(),
                normalize_optional_identifier(event.correlation_id.as_deref()),
                normalize_optional_identifier(event.causation_id.as_deref()),
                payload_json,
            ],
        )?;

        let encoded = if let Some(source_event_id) = source_event_id {
            transaction.query_row(
                "SELECT sequence, event_id, event_type, schema_version, source,
                        source_event_id, occurred_at, recorded_at, correlation_id,
                        causation_id, payload_json
                 FROM plugin_events WHERE source = ?1 AND source_event_id = ?2",
                params![event.source, source_event_id],
                map_plugin_event_row,
            )?
        } else {
            transaction.query_row(
                "SELECT sequence, event_id, event_type, schema_version, source,
                        source_event_id, occurred_at, recorded_at, correlation_id,
                        causation_id, payload_json
                 FROM plugin_events WHERE event_id = ?1",
                [event.event_id.as_str()],
                map_plugin_event_row,
            )?
        };
        transaction.commit()?;
        decode_plugin_event(encoded)
    }

    pub fn list_plugin_events_after(
        &self,
        sequence: i64,
        limit: usize,
    ) -> Result<Vec<PluginEventRecord>> {
        Ok(self.list_plugin_event_page_after(sequence, limit)?.records)
    }

    pub fn list_plugin_event_page_after(
        &self,
        sequence: i64,
        limit: usize,
    ) -> Result<PluginEventPage> {
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        let page = read_plugin_event_page_in_transaction(
            &transaction,
            sequence.max(0),
            None,
            None,
            limit,
        )?;
        transaction.commit()?;
        Ok(page)
    }

    pub fn prepare_plugin_event_delivery(
        &self,
        plugin_id: &str,
        subscription: &str,
        limit: usize,
    ) -> Result<PluginEventDeliveryBatch> {
        validate_identifier("plugin id", plugin_id)?;
        validate_identifier("plugin event subscription", subscription)?;
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        let existing_cursor = query_plugin_event_cursor(&transaction, plugin_id, subscription)?;
        let cursor = existing_cursor
            .as_ref()
            .map(|cursor| cursor.last_sequence)
            .unwrap_or(0);
        if existing_cursor.is_none() {
            upsert_plugin_event_cursor(&transaction, plugin_id, subscription, 0, Utc::now())?;
        }
        let existing_delivery = query_plugin_event_delivery(&transaction, plugin_id, subscription)?
            .filter(|(receipt, acknowledged_at)| {
                receipt.from_sequence == cursor && acknowledged_at.is_none()
            });
        let page = read_plugin_event_page_in_transaction(
            &transaction,
            cursor,
            Some(subscription),
            existing_delivery
                .as_ref()
                .map(|(receipt, _)| receipt.through_sequence),
            limit,
        )?;

        let receipt = if let Some((receipt, _)) = existing_delivery {
            Some(receipt)
        } else if page.through_sequence > cursor {
            let receipt = PluginEventDeliveryReceipt {
                delivery_token: Uuid::new_v4().to_string(),
                from_sequence: cursor,
                through_sequence: page.through_sequence,
                created_at: Utc::now(),
            };
            transaction.execute(
                "INSERT INTO plugin_event_deliveries
                     (plugin_id, subscription, delivery_token, from_sequence,
                      through_sequence, created_at, acknowledged_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
                     ON CONFLICT(plugin_id, subscription) DO UPDATE SET
                         delivery_token = excluded.delivery_token,
                         from_sequence = excluded.from_sequence,
                         through_sequence = excluded.through_sequence,
                         created_at = excluded.created_at,
                         acknowledged_at = NULL",
                params![
                    plugin_id,
                    subscription,
                    receipt.delivery_token,
                    receipt.from_sequence,
                    receipt.through_sequence,
                    receipt.created_at.to_rfc3339(),
                ],
            )?;
            Some(receipt)
        } else {
            None
        };

        transaction.commit()?;
        Ok(PluginEventDeliveryBatch {
            records: page.records,
            receipt,
            quarantined_count: page.quarantined_count,
        })
    }

    pub fn acknowledge_plugin_event_delivery(
        &self,
        plugin_id: &str,
        subscription: &str,
        delivery_token: &str,
        through_sequence: i64,
    ) -> Result<PluginEventCursor> {
        self.acknowledge_plugin_event_delivery_with_state(
            plugin_id,
            subscription,
            delivery_token,
            through_sequence,
            &[],
        )
    }

    pub fn acknowledge_plugin_event_delivery_with_state(
        &self,
        plugin_id: &str,
        subscription: &str,
        delivery_token: &str,
        through_sequence: i64,
        state_mutations: &[PluginStateMutation],
    ) -> Result<PluginEventCursor> {
        validate_identifier("plugin id", plugin_id)?;
        validate_identifier("plugin event subscription", subscription)?;
        validate_identifier("plugin event delivery token", delivery_token)?;
        if through_sequence < 0 {
            anyhow::bail!("plugin event delivery watermark cannot be negative");
        }

        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        let (receipt, acknowledged_at) =
            query_plugin_event_delivery(&transaction, plugin_id, subscription)?
                .context("no event batch has been delivered for this subscription")?;
        if receipt.delivery_token != delivery_token {
            anyhow::bail!("plugin event delivery token is not current for this subscription");
        }
        if receipt.through_sequence != through_sequence {
            anyhow::bail!(
                "plugin event delivery watermark {} does not match delivered watermark {}",
                through_sequence,
                receipt.through_sequence
            );
        }

        if acknowledged_at.is_none() {
            let updated_at = Utc::now();
            apply_plugin_state_mutations_in_transaction(
                &transaction,
                plugin_id,
                state_mutations,
                updated_at,
            )?;
            upsert_plugin_event_cursor(
                &transaction,
                plugin_id,
                subscription,
                receipt.through_sequence,
                updated_at,
            )?;
            transaction.execute(
                "UPDATE plugin_event_deliveries SET acknowledged_at = ?1
                 WHERE plugin_id = ?2 AND subscription = ?3 AND delivery_token = ?4",
                params![
                    updated_at.to_rfc3339(),
                    plugin_id,
                    subscription,
                    delivery_token
                ],
            )?;
        }

        let cursor = query_plugin_event_cursor(&transaction, plugin_id, subscription)?
            .context("plugin event cursor missing after delivery acknowledgement")?;
        transaction.commit()?;
        Ok(cursor)
    }

    pub fn quarantine_plugin_event(&self, sequence: i64, reason: &str) -> Result<bool> {
        if sequence <= 0 {
            anyhow::bail!("plugin event quarantine sequence must be positive");
        }
        validate_identifier("plugin event quarantine reason", reason)?;
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        let quarantined = quarantine_plugin_event_in_transaction(&transaction, sequence, reason)?;
        transaction.commit()?;
        Ok(quarantined)
    }

    pub fn list_plugin_event_dead_letters(
        &self,
        limit: usize,
    ) -> Result<Vec<PluginEventDeadLetter>> {
        let conn = self.lock_conn()?;
        let mut statement = conn.prepare(
            "SELECT sequence, event_id, event_type, schema_version, source,
                    source_event_id, occurred_at, recorded_at, correlation_id,
                    causation_id, payload_json, payload_truncated, reason, quarantined_at
             FROM plugin_event_dead_letters
             ORDER BY sequence ASC LIMIT ?1",
        )?;
        let dead_letters = statement
            .query_map([limit.clamp(1, MAX_EVENT_PAGE_SIZE) as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, bool>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                ))
            })?
            .map(|row| {
                let row = row?;
                Ok(PluginEventDeadLetter {
                    sequence: row.0,
                    event_id: row.1,
                    event_type: row.2,
                    schema_version: row.3,
                    source: row.4,
                    source_event_id: row.5,
                    occurred_at: row.6,
                    recorded_at: row.7,
                    correlation_id: row.8,
                    causation_id: row.9,
                    payload_json: row.10,
                    payload_truncated: row.11,
                    reason: row.12,
                    quarantined_at: parse_timestamp(&row.13, "plugin dead letter quarantined_at")?,
                })
            })
            .collect();
        dead_letters
    }

    pub fn compact_plugin_events(
        &self,
        policy: PluginEventRetentionPolicy,
        now: DateTime<Utc>,
    ) -> Result<PluginEventCompactionReport> {
        if policy.acknowledged_event_age < chrono::Duration::zero()
            || policy.unconsumed_event_age < chrono::Duration::zero()
            || policy.dead_letter_age < chrono::Duration::zero()
        {
            anyhow::bail!("plugin event retention ages cannot be negative");
        }
        if policy.unconsumed_event_age < policy.acknowledged_event_age {
            anyhow::bail!(
                "unconsumed plugin event retention cannot be shorter than acknowledged retention"
            );
        }
        let event_cutoff = now
            .checked_sub_signed(policy.acknowledged_event_age)
            .context("plugin event retention cutoff overflow")?;
        let dead_letter_cutoff = now
            .checked_sub_signed(policy.dead_letter_age)
            .context("plugin dead-letter retention cutoff overflow")?;
        let unconsumed_cutoff = now
            .checked_sub_signed(policy.unconsumed_event_age)
            .context("unconsumed plugin event retention cutoff overflow")?;
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        let acknowledged_events_deleted = transaction.execute(
            "DELETE FROM plugin_events
             WHERE julianday(recorded_at) < julianday(?1)
               AND sequence <= (
                   SELECT MIN(plugin_event_cursors.last_sequence)
                   FROM plugin_event_cursors
                   WHERE plugin_event_cursors.subscription = plugin_events.event_type
               )",
            [event_cutoff.to_rfc3339()],
        )?;
        let expired_unconsumed_events_deleted = transaction.execute(
            "DELETE FROM plugin_events WHERE julianday(recorded_at) < julianday(?1)",
            [unconsumed_cutoff.to_rfc3339()],
        )?;
        let dead_letters_deleted = transaction.execute(
            "DELETE FROM plugin_event_dead_letters
             WHERE julianday(quarantined_at) < julianday(?1)",
            [dead_letter_cutoff.to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(PluginEventCompactionReport {
            acknowledged_events_deleted,
            expired_unconsumed_events_deleted,
            dead_letters_deleted,
        })
    }

    pub fn get_plugin_event_cursor(
        &self,
        plugin_id: &str,
        subscription: &str,
    ) -> Result<Option<PluginEventCursor>> {
        validate_identifier("plugin id", plugin_id)?;
        validate_identifier("plugin event subscription", subscription)?;
        let conn = self.lock_conn()?;
        let encoded = conn
            .query_row(
                "SELECT plugin_id, subscription, last_sequence, updated_at
                 FROM plugin_event_cursors WHERE plugin_id = ?1 AND subscription = ?2",
                params![plugin_id, subscription],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        encoded.map(decode_plugin_cursor).transpose()
    }
}

struct PreparedPluginStateMutation {
    key: String,
    schema_version: u32,
    value_json: Option<String>,
    delete: bool,
}

fn apply_plugin_state_mutations_in_transaction(
    transaction: &Transaction<'_>,
    plugin_id: &str,
    mutations: &[PluginStateMutation],
    updated_at: DateTime<Utc>,
) -> Result<()> {
    if mutations.is_empty() {
        return Ok(());
    }

    let mut prepared = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        validate_identifier("plugin state key", &mutation.key)?;
        if mutation.schema_version == 0 {
            anyhow::bail!("plugin state schema version must be at least 1");
        }
        let value_json = if mutation.delete {
            None
        } else {
            let encoded = serde_json::to_string(&mutation.value)
                .context("serialize plugin state mutation value")?;
            if encoded.len() > MAX_PLUGIN_STATE_VALUE_BYTES {
                anyhow::bail!(
                    "plugin state value is {} bytes; maximum is {} bytes",
                    encoded.len(),
                    MAX_PLUGIN_STATE_VALUE_BYTES
                );
            }
            Some(encoded)
        };
        prepared.push(PreparedPluginStateMutation {
            key: mutation.key.clone(),
            schema_version: mutation.schema_version,
            value_json,
            delete: mutation.delete,
        });
    }

    let mut projected = HashMap::<String, usize>::new();
    {
        let mut statement = transaction
            .prepare("SELECT key, LENGTH(value_json) FROM plugin_state WHERE plugin_id = ?1")?;
        let rows = statement.query_map([plugin_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (key, bytes) = row?;
            projected.insert(
                key,
                usize::try_from(bytes).context("negative plugin state value length")?,
            );
        }
    }
    for mutation in &prepared {
        if mutation.delete {
            projected.remove(&mutation.key);
        } else if let Some(value_json) = &mutation.value_json {
            projected.insert(mutation.key.clone(), value_json.len());
        }
    }
    if projected.len() > MAX_PLUGIN_STATE_KEYS {
        anyhow::bail!(
            "plugin state namespace would exceed its {} key limit",
            MAX_PLUGIN_STATE_KEYS
        );
    }
    let projected_bytes = projected
        .values()
        .try_fold(0usize, |total, bytes| total.checked_add(*bytes))
        .context("plugin state namespace byte count overflow")?;
    if projected_bytes > MAX_PLUGIN_STATE_TOTAL_BYTES {
        anyhow::bail!(
            "plugin state namespace would exceed its {} byte limit",
            MAX_PLUGIN_STATE_TOTAL_BYTES
        );
    }

    let updated_at = updated_at.to_rfc3339();
    for mutation in prepared {
        if mutation.delete {
            transaction.execute(
                "DELETE FROM plugin_state WHERE plugin_id = ?1 AND key = ?2",
                params![plugin_id, mutation.key],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO plugin_state (plugin_id, key, schema_version, value_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(plugin_id, key) DO UPDATE SET
                     schema_version = excluded.schema_version,
                     value_json = excluded.value_json,
                     updated_at = excluded.updated_at",
                params![
                    plugin_id,
                    mutation.key,
                    i64::from(mutation.schema_version),
                    mutation
                        .value_json
                        .context("prepared state value missing")?,
                    updated_at,
                ],
            )?;
        }
    }
    Ok(())
}

type EncodedPluginState = (String, String, i64, String, String);
type EncodedPluginEvent = (
    i64,
    String,
    String,
    i64,
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

fn map_plugin_state_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EncodedPluginState> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn decode_plugin_state(encoded: EncodedPluginState) -> Result<PluginStateRecord> {
    let (plugin_id, key, schema_version, value_json, updated_at) = encoded;
    Ok(PluginStateRecord {
        plugin_id,
        key,
        schema_version: u32::try_from(schema_version)
            .context("invalid plugin state schema version")?,
        value: serde_json::from_str(&value_json).context("decode plugin state value")?,
        updated_at: parse_timestamp(&updated_at, "plugin state updated_at")?,
    })
}

fn map_plugin_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EncodedPluginEvent> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn read_plugin_event_page_in_transaction(
    transaction: &Transaction<'_>,
    sequence: i64,
    event_type: Option<&str>,
    maximum_sequence: Option<i64>,
    limit: usize,
) -> Result<PluginEventPage> {
    let limit = limit.clamp(1, MAX_EVENT_PAGE_SIZE) as i64;
    let (records, failures, through_sequence) =
        if let (Some(event_type), Some(maximum_sequence)) = (event_type, maximum_sequence) {
            let mut statement = transaction.prepare(
                "SELECT sequence, event_id, event_type, schema_version, source,
                    source_event_id, occurred_at, recorded_at, correlation_id,
                    causation_id, payload_json
             FROM plugin_events
             WHERE sequence > ?1 AND sequence <= ?2 AND event_type = ?3
             ORDER BY sequence ASC LIMIT ?4",
            )?;
            let mut rows = statement.query(params![
                sequence.max(0),
                maximum_sequence,
                event_type,
                limit
            ])?;
            collect_plugin_event_rows(&mut rows, sequence.max(0))?
        } else if let Some(event_type) = event_type {
            let mut statement = transaction.prepare(
                "SELECT sequence, event_id, event_type, schema_version, source,
                    source_event_id, occurred_at, recorded_at, correlation_id,
                    causation_id, payload_json
             FROM plugin_events
             WHERE sequence > ?1 AND event_type = ?2
             ORDER BY sequence ASC LIMIT ?3",
            )?;
            let mut rows = statement.query(params![sequence.max(0), event_type, limit])?;
            collect_plugin_event_rows(&mut rows, sequence.max(0))?
        } else if let Some(maximum_sequence) = maximum_sequence {
            let mut statement = transaction.prepare(
                "SELECT sequence, event_id, event_type, schema_version, source,
                    source_event_id, occurred_at, recorded_at, correlation_id,
                    causation_id, payload_json
             FROM plugin_events
             WHERE sequence > ?1 AND sequence <= ?2
             ORDER BY sequence ASC LIMIT ?3",
            )?;
            let mut rows = statement.query(params![sequence.max(0), maximum_sequence, limit])?;
            collect_plugin_event_rows(&mut rows, sequence.max(0))?
        } else {
            let mut statement = transaction.prepare(
                "SELECT sequence, event_id, event_type, schema_version, source,
                    source_event_id, occurred_at, recorded_at, correlation_id,
                    causation_id, payload_json
             FROM plugin_events
             WHERE sequence > ?1
             ORDER BY sequence ASC LIMIT ?2",
            )?;
            let mut rows = statement.query(params![sequence.max(0), limit])?;
            collect_plugin_event_rows(&mut rows, sequence.max(0))?
        };

    for (sequence, reason) in &failures {
        quarantine_plugin_event_in_transaction(transaction, *sequence, reason)?;
    }
    Ok(PluginEventPage {
        records,
        through_sequence,
        quarantined_count: failures.len(),
    })
}

fn collect_plugin_event_rows(
    rows: &mut rusqlite::Rows<'_>,
    starting_sequence: i64,
) -> Result<(Vec<PluginEventRecord>, Vec<(i64, String)>, i64)> {
    let mut records = Vec::new();
    let mut failures = Vec::new();
    let mut through_sequence = starting_sequence;
    while let Some(row) = rows.next()? {
        let sequence = row
            .get::<_, i64>(0)
            .context("decode plugin event sequence")?;
        through_sequence = sequence;
        match map_plugin_event_row(row) {
            Ok(encoded) => match decode_plugin_event(encoded) {
                Ok(record) => records.push(record),
                Err(error) => failures.push((sequence, format!("event decode failed: {error:#}"))),
            },
            Err(error) => failures.push((sequence, format!("event row decode failed: {error}"))),
        }
    }
    Ok((records, failures, through_sequence))
}

fn query_plugin_event_cursor(
    transaction: &Transaction<'_>,
    plugin_id: &str,
    subscription: &str,
) -> Result<Option<PluginEventCursor>> {
    let encoded = transaction
        .query_row(
            "SELECT plugin_id, subscription, last_sequence, updated_at
             FROM plugin_event_cursors WHERE plugin_id = ?1 AND subscription = ?2",
            params![plugin_id, subscription],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    encoded.map(decode_plugin_cursor).transpose()
}

fn upsert_plugin_event_cursor(
    transaction: &Transaction<'_>,
    plugin_id: &str,
    subscription: &str,
    sequence: i64,
    updated_at: DateTime<Utc>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO plugin_event_cursors
         (plugin_id, subscription, last_sequence, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(plugin_id, subscription) DO UPDATE SET
             last_sequence = MAX(plugin_event_cursors.last_sequence, excluded.last_sequence),
             updated_at = CASE
                 WHEN excluded.last_sequence >= plugin_event_cursors.last_sequence
                 THEN excluded.updated_at
                 ELSE plugin_event_cursors.updated_at
             END",
        params![plugin_id, subscription, sequence, updated_at.to_rfc3339()],
    )?;
    Ok(())
}

fn query_plugin_event_delivery(
    transaction: &Transaction<'_>,
    plugin_id: &str,
    subscription: &str,
) -> Result<Option<(PluginEventDeliveryReceipt, Option<DateTime<Utc>>)>> {
    let encoded = transaction
        .query_row(
            "SELECT delivery_token, from_sequence, through_sequence, created_at, acknowledged_at
             FROM plugin_event_deliveries
             WHERE plugin_id = ?1 AND subscription = ?2",
            params![plugin_id, subscription],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    encoded
        .map(
            |(delivery_token, from_sequence, through_sequence, created_at, acknowledged_at)| {
                Ok((
                    PluginEventDeliveryReceipt {
                        delivery_token,
                        from_sequence,
                        through_sequence,
                        created_at: parse_timestamp(
                            &created_at,
                            "plugin event delivery created_at",
                        )?,
                    },
                    acknowledged_at
                        .map(|value| {
                            parse_timestamp(&value, "plugin event delivery acknowledged_at")
                        })
                        .transpose()?,
                ))
            },
        )
        .transpose()
}

fn quarantine_plugin_event_in_transaction(
    transaction: &Transaction<'_>,
    sequence: i64,
    reason: &str,
) -> Result<bool> {
    type RawDeadLetter = (
        i64,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    );
    let raw: Option<RawDeadLetter> = transaction
        .query_row(
            "SELECT sequence, CAST(event_id AS TEXT), CAST(event_type AS TEXT),
                    CAST(schema_version AS TEXT), CAST(source AS TEXT),
                    CASE WHEN source_event_id IS NULL THEN NULL ELSE CAST(source_event_id AS TEXT) END,
                    CAST(occurred_at AS TEXT), CAST(recorded_at AS TEXT),
                    CASE WHEN correlation_id IS NULL THEN NULL ELSE CAST(correlation_id AS TEXT) END,
                    CASE WHEN causation_id IS NULL THEN NULL ELSE CAST(causation_id AS TEXT) END,
                    CAST(payload_json AS TEXT)
             FROM plugin_events WHERE sequence = ?1",
            [sequence],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(false);
    };
    let (payload_json, payload_truncated) =
        truncate_utf8_bytes(&raw.10, MAX_PLUGIN_EVENT_PAYLOAD_BYTES);
    let (reason, _) = truncate_utf8_bytes(reason, MAX_DEAD_LETTER_REASON_BYTES);
    transaction.execute(
        "INSERT OR IGNORE INTO plugin_event_dead_letters
         (sequence, event_id, event_type, schema_version, source, source_event_id,
          occurred_at, recorded_at, correlation_id, causation_id, payload_json,
          payload_truncated, reason, quarantined_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            raw.0,
            raw.1,
            raw.2,
            raw.3,
            raw.4,
            raw.5,
            raw.6,
            raw.7,
            raw.8,
            raw.9,
            payload_json,
            payload_truncated,
            reason,
            Utc::now().to_rfc3339(),
        ],
    )?;
    transaction.execute("DELETE FROM plugin_events WHERE sequence = ?1", [sequence])?;
    Ok(true)
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn decode_plugin_event(encoded: EncodedPluginEvent) -> Result<PluginEventRecord> {
    let (
        sequence,
        event_id,
        event_type,
        schema_version,
        source,
        source_event_id,
        occurred_at,
        recorded_at,
        correlation_id,
        causation_id,
        payload_json,
    ) = encoded;
    Ok(PluginEventRecord {
        sequence,
        event_id,
        event_type,
        schema_version: u32::try_from(schema_version)
            .context("invalid plugin event schema version")?,
        source,
        source_event_id,
        occurred_at: parse_timestamp(&occurred_at, "plugin event occurred_at")?,
        recorded_at: parse_timestamp(&recorded_at, "plugin event recorded_at")?,
        correlation_id,
        causation_id,
        payload: serde_json::from_str(&payload_json).context("decode plugin event payload")?,
    })
}

fn decode_plugin_cursor(encoded: (String, String, i64, String)) -> Result<PluginEventCursor> {
    Ok(PluginEventCursor {
        plugin_id: encoded.0,
        subscription: encoded.1,
        last_sequence: encoded.2,
        updated_at: parse_timestamp(&encoded.3, "plugin event cursor updated_at")?,
    })
}

fn parse_timestamp(raw: &str, field: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("decode {field}"))
        .map(|value| value.with_timezone(&Utc))
}

fn normalize_optional_identifier(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|value| !value.is_empty())
}

fn validate_identifier(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > MAX_PLUGIN_IDENTIFIER_BYTES
        || value.chars().any(char::is_control)
    {
        anyhow::bail!(
            "{label} must be a non-empty, control-free string of at most {} bytes",
            MAX_PLUGIN_IDENTIFIER_BYTES
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_database() -> (AgentDatabase, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "ponderer_plugin_storage_{}.db",
            uuid::Uuid::new_v4()
        ));
        (AgentDatabase::new(&path).expect("database"), path)
    }

    fn event(event_id: &str, event_type: &str) -> NewPluginEvent {
        NewPluginEvent {
            event_id: event_id.to_string(),
            event_type: event_type.to_string(),
            schema_version: 1,
            source: "dev.fixture".to_string(),
            source_event_id: Some(event_id.to_string()),
            occurred_at: Utc::now(),
            correlation_id: None,
            causation_id: None,
            payload: serde_json::json!({"body": event_id}),
        }
    }

    #[test]
    fn plugin_state_survives_reopen_and_is_namespaced() {
        let (db, path) = test_database();
        db.put_plugin_state("dev.one", "cursor", 2, &serde_json::json!({"n": 4}))
            .expect("put one");
        db.put_plugin_state("dev.two", "cursor", 1, &serde_json::json!({"n": 8}))
            .expect("put two");
        drop(db);

        let reopened = AgentDatabase::new(&path).expect("reopen");
        let one = reopened
            .get_plugin_state("dev.one", "cursor")
            .expect("get")
            .expect("state");
        assert_eq!(one.schema_version, 2);
        assert_eq!(one.value, serde_json::json!({"n": 4}));
        assert_eq!(reopened.list_plugin_state("dev.two").unwrap().len(), 1);
        assert!(reopened.delete_plugin_state("dev.one", "cursor").unwrap());
        assert!(reopened
            .get_plugin_state("dev.one", "cursor")
            .unwrap()
            .is_none());
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn plugin_state_rejects_invalid_versions_and_unbounded_values() {
        let (db, path) = test_database();
        assert!(db
            .put_plugin_state("dev.one", "cursor", 0, &serde_json::json!(1))
            .is_err());
        assert!(db
            .put_plugin_state(
                "dev.one",
                "oversized",
                1,
                &serde_json::json!("x".repeat(MAX_PLUGIN_STATE_VALUE_BYTES)),
            )
            .is_err());
        assert!(db
            .put_plugin_state(
                "dev.one",
                &"k".repeat(MAX_PLUGIN_IDENTIFIER_BYTES + 1),
                1,
                &serde_json::json!(1),
            )
            .is_err());
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn plugin_state_mutation_batches_are_atomic_and_validate_final_namespace() {
        let (db, path) = test_database();
        db.put_plugin_state("dev.one", "existing", 1, &serde_json::json!("kept"))
            .unwrap();
        let updates = vec![
            PluginStateMutation {
                key: "valid".to_string(),
                schema_version: 1,
                value: serde_json::json!({"n": 1}),
                delete: false,
            },
            PluginStateMutation {
                key: "oversized".to_string(),
                schema_version: 1,
                value: serde_json::json!("x".repeat(MAX_PLUGIN_STATE_VALUE_BYTES)),
                delete: false,
            },
        ];

        assert!(db
            .apply_plugin_state_mutations("dev.one", &updates)
            .is_err());
        assert!(db.get_plugin_state("dev.one", "valid").unwrap().is_none());
        assert_eq!(
            db.get_plugin_state("dev.one", "existing")
                .unwrap()
                .unwrap()
                .value,
            serde_json::json!("kept")
        );
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn plugin_events_are_ordered_and_deduplicated_per_source() {
        let (db, path) = test_database();
        let event = |event_id: &str, source: &str, body: &str| NewPluginEvent {
            event_id: event_id.into(),
            event_type: "social.post".into(),
            schema_version: 1,
            source: source.into(),
            source_event_id: Some("post-1".into()),
            occurred_at: Utc::now(),
            correlation_id: None,
            causation_id: None,
            payload: serde_json::json!({"body":body}),
        };
        let first = db
            .append_plugin_event(&event("host-1", "dev.social", "hello"))
            .unwrap();
        let duplicate = db
            .append_plugin_event(&event("host-duplicate", "dev.social", "ignored"))
            .unwrap();
        let other = db
            .append_plugin_event(&event("host-2", "dev.other", "other"))
            .unwrap();
        assert_eq!(duplicate.event_id, first.event_id);
        assert!(other.sequence > first.sequence);
        let events = db.list_plugin_events_after(0, 100).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_id, first.event_id);
        assert_eq!(events[1].event_id, other.event_id);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn plugin_event_payloads_are_bounded_by_serialized_bytes() {
        let (db, path) = test_database();
        let mut oversized = event("oversized", "social.post");
        oversized.payload = Value::String("x".repeat(MAX_PLUGIN_EVENT_PAYLOAD_BYTES));
        let error = db
            .append_plugin_event(&oversized)
            .expect_err("oversized payload must fail");
        assert!(error.to_string().contains("maximum"));
        assert!(db.list_plugin_events_after(0, 10).unwrap().is_empty());
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn corrupt_event_rows_are_quarantined_without_blocking_the_page() {
        let (db, path) = test_database();
        let corrupt = db
            .append_plugin_event(&event("corrupt", "social.post"))
            .expect("append corrupt fixture");
        let valid = db
            .append_plugin_event(&event("valid", "social.post"))
            .expect("append valid fixture");
        {
            let conn = db.lock_conn().expect("connection");
            conn.execute(
                "UPDATE plugin_events SET payload_json = '{' WHERE sequence = ?1",
                [corrupt.sequence],
            )
            .expect("corrupt row");
        }

        let page = db
            .list_plugin_event_page_after(0, 10)
            .expect("read hardened page");
        assert_eq!(page.through_sequence, valid.sequence);
        assert_eq!(page.quarantined_count, 1);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].event_id, "valid");
        let dead_letters = db.list_plugin_event_dead_letters(10).expect("dead letters");
        assert_eq!(dead_letters.len(), 1);
        assert_eq!(dead_letters[0].sequence, corrupt.sequence);
        assert!(dead_letters[0]
            .reason
            .contains("decode plugin event payload"));
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn delivery_acknowledgement_is_bound_to_issued_receipt() {
        let (db, path) = test_database();
        let record = db
            .append_plugin_event(&event("delivered", "social.post"))
            .expect("append");
        let batch = db
            .prepare_plugin_event_delivery("host.agent", "social.post", 10)
            .expect("prepare delivery");
        let receipt = batch.receipt.expect("receipt");
        assert_eq!(receipt.through_sequence, record.sequence);
        assert!(db
            .acknowledge_plugin_event_delivery(
                "host.agent",
                "social.post",
                &receipt.delivery_token,
                receipt.through_sequence + 1,
            )
            .is_err());
        assert_eq!(
            db.get_plugin_event_cursor("host.agent", "social.post")
                .expect("cursor")
                .expect("registered cursor")
                .last_sequence,
            0
        );
        assert!(db
            .acknowledge_plugin_event_delivery(
                "host.agent",
                "social.post",
                "not-the-issued-token",
                receipt.through_sequence,
            )
            .is_err());
        let cursor = db
            .acknowledge_plugin_event_delivery(
                "host.agent",
                "social.post",
                &receipt.delivery_token,
                receipt.through_sequence,
            )
            .expect("valid acknowledgement");
        assert_eq!(cursor.last_sequence, record.sequence);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn delivery_acknowledgement_commits_state_atomically_and_once() {
        let (db, path) = test_database();
        let first = db
            .append_plugin_event(&event("atomic-first", "host.lifecycle.tick"))
            .unwrap();
        let batch = db
            .prepare_plugin_event_delivery("dev.plugin", "host.lifecycle.tick", 10)
            .unwrap();
        let receipt = batch.receipt.unwrap();
        let first_update = PluginStateMutation {
            key: "count".to_string(),
            schema_version: 1,
            value: serde_json::json!(1),
            delete: false,
        };
        let cursor = db
            .acknowledge_plugin_event_delivery_with_state(
                "dev.plugin",
                "host.lifecycle.tick",
                &receipt.delivery_token,
                receipt.through_sequence,
                std::slice::from_ref(&first_update),
            )
            .unwrap();
        assert_eq!(cursor.last_sequence, first.sequence);
        assert_eq!(
            db.get_plugin_state("dev.plugin", "count")
                .unwrap()
                .unwrap()
                .value,
            serde_json::json!(1)
        );

        let duplicate_update = PluginStateMutation {
            value: serde_json::json!(2),
            ..first_update
        };
        db.acknowledge_plugin_event_delivery_with_state(
            "dev.plugin",
            "host.lifecycle.tick",
            &receipt.delivery_token,
            receipt.through_sequence,
            &[duplicate_update],
        )
        .unwrap();
        assert_eq!(
            db.get_plugin_state("dev.plugin", "count")
                .unwrap()
                .unwrap()
                .value,
            serde_json::json!(1)
        );

        db.append_plugin_event(&event("atomic-second", "host.lifecycle.tick"))
            .unwrap();
        let second = db
            .prepare_plugin_event_delivery("dev.plugin", "host.lifecycle.tick", 10)
            .unwrap();
        let second_receipt = second.receipt.unwrap();
        let invalid = PluginStateMutation {
            key: "invalid".to_string(),
            schema_version: 0,
            value: serde_json::json!(true),
            delete: false,
        };
        assert!(db
            .acknowledge_plugin_event_delivery_with_state(
                "dev.plugin",
                "host.lifecycle.tick",
                &second_receipt.delivery_token,
                second_receipt.through_sequence,
                &[invalid],
            )
            .is_err());
        assert_eq!(
            db.get_plugin_event_cursor("dev.plugin", "host.lifecycle.tick")
                .unwrap()
                .unwrap()
                .last_sequence,
            first.sequence
        );
        assert!(db
            .get_plugin_state("dev.plugin", "invalid")
            .unwrap()
            .is_none());
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unacknowledged_delivery_is_stable_while_new_events_arrive() {
        let (db, path) = test_database();
        let first = db
            .append_plugin_event(&event("first", "social.post"))
            .expect("first event");
        let initial = db
            .prepare_plugin_event_delivery("host.agent", "social.post", 10)
            .expect("initial delivery");
        let initial_receipt = initial.receipt.expect("initial receipt");
        assert_eq!(initial_receipt.through_sequence, first.sequence);

        let second = db
            .append_plugin_event(&event("second", "social.post"))
            .expect("second event");
        let repeated = db
            .prepare_plugin_event_delivery("host.agent", "social.post", 10)
            .expect("repeated delivery");
        let repeated_receipt = repeated.receipt.expect("repeated receipt");
        assert_eq!(
            repeated_receipt.delivery_token,
            initial_receipt.delivery_token
        );
        assert_eq!(repeated.records.len(), 1);
        assert_eq!(repeated.records[0].sequence, first.sequence);

        db.acknowledge_plugin_event_delivery(
            "host.agent",
            "social.post",
            &initial_receipt.delivery_token,
            initial_receipt.through_sequence,
        )
        .expect("acknowledge first delivery");
        let next = db
            .prepare_plugin_event_delivery("host.agent", "social.post", 10)
            .expect("next delivery");
        assert_eq!(next.records.len(), 1);
        assert_eq!(next.records[0].sequence, second.sequence);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn compaction_waits_for_every_cursor_and_preserves_unsubscribed_events() {
        let (db, path) = test_database();
        let now = Utc::now();
        let acknowledged = db
            .append_plugin_event(&event("acknowledged", "social.post"))
            .expect("append acknowledged");
        let orphan = db
            .append_plugin_event(&event("orphan", "host.lifecycle.tick"))
            .expect("append orphan");
        let expired = db
            .append_plugin_event(&event("expired", "host.lifecycle.removed"))
            .expect("append hard-retention fixture");
        {
            let conn = db.lock_conn().expect("connection");
            conn.execute(
                "UPDATE plugin_events SET recorded_at = ?1",
                [(now - chrono::Duration::days(8)).to_rfc3339()],
            )
            .expect("age fixtures");
            conn.execute(
                "UPDATE plugin_events SET recorded_at = ?1 WHERE sequence = ?2",
                params![
                    (now - chrono::Duration::days(91)).to_rfc3339(),
                    expired.sequence
                ],
            )
            .expect("age unconsumed fixture past hard retention");
        }
        let slow = db
            .prepare_plugin_event_delivery("consumer.slow", "social.post", 10)
            .expect("slow delivery");
        let fast = db
            .prepare_plugin_event_delivery("consumer.fast", "social.post", 10)
            .expect("fast delivery");
        let fast_receipt = fast.receipt.expect("fast receipt");
        db.acknowledge_plugin_event_delivery(
            "consumer.fast",
            "social.post",
            &fast_receipt.delivery_token,
            fast_receipt.through_sequence,
        )
        .expect("fast acknowledgement");
        let policy = PluginEventRetentionPolicy {
            acknowledged_event_age: chrono::Duration::days(7),
            unconsumed_event_age: chrono::Duration::days(90),
            dead_letter_age: chrono::Duration::days(30),
        };
        let blocked = db
            .compact_plugin_events(policy, now)
            .expect("blocked compaction");
        assert_eq!(blocked.acknowledged_events_deleted, 0);
        assert_eq!(blocked.expired_unconsumed_events_deleted, 1);

        let slow_receipt = slow.receipt.expect("slow receipt");
        assert_eq!(slow_receipt.through_sequence, acknowledged.sequence);
        db.acknowledge_plugin_event_delivery(
            "consumer.slow",
            "social.post",
            &slow_receipt.delivery_token,
            slow_receipt.through_sequence,
        )
        .expect("slow acknowledgement");
        let compacted = db.compact_plugin_events(policy, now).expect("compaction");
        assert_eq!(compacted.acknowledged_events_deleted, 1);
        let remaining = db.list_plugin_events_after(0, 10).expect("remaining");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].sequence, orphan.sequence);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn dead_letters_expire_on_their_separate_retention_window() {
        let (db, path) = test_database();
        let record = db
            .append_plugin_event(&event("dead", "social.post"))
            .expect("append");
        db.quarantine_plugin_event(record.sequence, "unsupported fixture")
            .expect("quarantine");
        let now = Utc::now();
        {
            let conn = db.lock_conn().expect("connection");
            conn.execute(
                "UPDATE plugin_event_dead_letters SET quarantined_at = ?1",
                [(now - chrono::Duration::days(31)).to_rfc3339()],
            )
            .expect("age dead letter");
        }
        let report = db
            .compact_plugin_events(PluginEventRetentionPolicy::default(), now)
            .expect("compact");
        assert_eq!(report.dead_letters_deleted, 1);
        assert!(db
            .list_plugin_event_dead_letters(10)
            .expect("dead letters")
            .is_empty());
        drop(db);
        let _ = std::fs::remove_file(path);
    }
}
