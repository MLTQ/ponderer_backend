use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::database::{
    AgentDatabase, NewPluginEvent, PluginEventDeliveryReceipt, PluginEventRecord,
};
use crate::plugin_contract::{RuntimePluginLifecycleEvent, RuntimePluginPollEvent};
use crate::skills::SkillEvent;

pub const AGENT_EVENT_CONSUMER_ID: &str = "host.agent";
pub const SKILL_EVENT_SUBSCRIPTION: &str = "plugin.poll.new_content";

#[derive(Debug, Clone, PartialEq)]
pub struct RecordedPluginEvent {
    pub record: PluginEventRecord,
    pub inserted: bool,
}

#[derive(Debug, Clone)]
pub struct PluginSkillEventBatch {
    pub events: Vec<SkillEvent>,
    pub through_sequence: i64,
    pub receipt: Option<PluginEventDeliveryReceipt>,
    pub quarantined_count: usize,
}

#[derive(Clone)]
pub struct PluginEventLedger {
    database: Arc<AgentDatabase>,
}

impl PluginEventLedger {
    pub fn new(database: Arc<AgentDatabase>) -> Self {
        Self { database }
    }

    pub fn record_lifecycle_event(
        &self,
        event: &RuntimePluginLifecycleEvent,
    ) -> Result<PluginEventRecord> {
        self.database.append_plugin_event(&NewPluginEvent {
            event_id: Uuid::new_v4().to_string(),
            event_type: format!("host.lifecycle.{}", event.wire_name()),
            schema_version: 1,
            source: "host.agent".to_string(),
            source_event_id: None,
            occurred_at: Utc::now(),
            correlation_id: None,
            causation_id: None,
            payload: serde_json::to_value(event).context("serialize plugin lifecycle event")?,
        })
    }

    pub fn record_polled_event(
        &self,
        plugin_id: &str,
        event: &RuntimePluginPollEvent,
    ) -> Result<RecordedPluginEvent> {
        let event_id = Uuid::new_v4().to_string();
        let record = self.database.append_plugin_event(&NewPluginEvent {
            event_id: event_id.clone(),
            event_type: SKILL_EVENT_SUBSCRIPTION.to_string(),
            schema_version: 1,
            source: plugin_id.to_string(),
            source_event_id: Some(event.id.clone()),
            occurred_at: Utc::now(),
            correlation_id: None,
            causation_id: None,
            payload: serde_json::to_value(event).context("serialize polled plugin event")?,
        })?;
        Ok(RecordedPluginEvent {
            inserted: record.event_id == event_id,
            record,
        })
    }

    pub fn pending_skill_events(&self) -> Result<PluginSkillEventBatch> {
        let delivery = self.database.prepare_plugin_event_delivery(
            AGENT_EVENT_CONSUMER_ID,
            SKILL_EVENT_SUBSCRIPTION,
            1_000,
        )?;
        let through_sequence = if let Some(receipt) = &delivery.receipt {
            receipt.through_sequence
        } else {
            self.database
                .get_plugin_event_cursor(AGENT_EVENT_CONSUMER_ID, SKILL_EVENT_SUBSCRIPTION)?
                .map(|cursor| cursor.last_sequence)
                .unwrap_or(0)
        };
        let mut events = Vec::new();
        let mut quarantined_count = delivery.quarantined_count;
        for record in delivery.records {
            if record.schema_version != 1 {
                self.database.quarantine_plugin_event(
                    record.sequence,
                    &format!(
                        "unsupported {} schema version {}; host supports version 1",
                        SKILL_EVENT_SUBSCRIPTION, record.schema_version
                    ),
                )?;
                quarantined_count += 1;
                continue;
            }
            match decode_skill_event(record.clone()) {
                Ok(event) => events.push(event),
                Err(error) => {
                    self.database.quarantine_plugin_event(
                        record.sequence,
                        &format!("skill event decode failed: {error:#}"),
                    )?;
                    quarantined_count += 1;
                }
            }
        }
        Ok(PluginSkillEventBatch {
            events,
            through_sequence,
            receipt: delivery.receipt,
            quarantined_count,
        })
    }

    pub fn acknowledge_skill_events(&self, batch: &PluginSkillEventBatch) -> Result<()> {
        let Some(receipt) = &batch.receipt else {
            if batch.events.is_empty() {
                return Ok(());
            }
            anyhow::bail!("cannot acknowledge plugin events without a delivery receipt");
        };
        if batch.through_sequence != receipt.through_sequence {
            anyhow::bail!("plugin event batch watermark does not match its delivery receipt");
        }
        self.database.acknowledge_plugin_event_delivery(
            AGENT_EVENT_CONSUMER_ID,
            SKILL_EVENT_SUBSCRIPTION,
            &receipt.delivery_token,
            receipt.through_sequence,
        )?;
        Ok(())
    }

    pub fn database(&self) -> &Arc<AgentDatabase> {
        &self.database
    }
}

fn decode_skill_event(record: PluginEventRecord) -> Result<SkillEvent> {
    let event: RuntimePluginPollEvent =
        serde_json::from_value::<RuntimePluginPollEvent>(record.payload)
            .with_context(|| format!("decode plugin event at sequence {}", record.sequence))?;
    Ok(SkillEvent::NewContent {
        id: format!("{}:{}", record.source, event.id),
        source: event.source,
        author: event.author,
        body: event.body,
        parent_ids: event.parent_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> (PluginEventLedger, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "ponderer_plugin_ledger_{}.db",
            uuid::Uuid::new_v4()
        ));
        let database = Arc::new(AgentDatabase::new(&path).expect("database"));
        (PluginEventLedger::new(database), path)
    }

    fn polled(id: &str, body: &str) -> RuntimePluginPollEvent {
        RuntimePluginPollEvent {
            id: id.to_string(),
            source: "social-feed".to_string(),
            author: "Arlecchino".to_string(),
            body: body.to_string(),
            parent_ids: vec!["thread-1".to_string()],
        }
    }

    #[test]
    fn polled_events_are_recorded_before_delivery_and_deduplicated() {
        let (ledger, path) = ledger();
        let first = ledger
            .record_polled_event("dev.social", &polled("post-1", "hello"))
            .expect("first");
        let duplicate = ledger
            .record_polled_event("dev.social", &polled("post-1", "changed"))
            .expect("duplicate");
        assert!(first.inserted);
        assert!(!duplicate.inserted);
        assert_eq!(duplicate.record.sequence, first.record.sequence);

        let batch = ledger.pending_skill_events().expect("batch");
        assert_eq!(batch.events.len(), 1);
        assert!(matches!(
            &batch.events[0],
            SkillEvent::NewContent { id, body, .. }
                if id == "dev.social:post-1" && body == "hello"
        ));
        ledger
            .acknowledge_skill_events(&batch)
            .expect("acknowledge");
        assert!(ledger
            .pending_skill_events()
            .expect("empty batch")
            .events
            .is_empty());
        drop(ledger);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn lifecycle_events_are_durable_and_typed() {
        let (ledger, path) = ledger();
        let record = ledger
            .record_lifecycle_event(&RuntimePluginLifecycleEvent::ReflectionCompleted {
                summary: "noticed a pattern".to_string(),
            })
            .expect("record");
        assert_eq!(record.event_type, "host.lifecycle.reflection_completed");
        assert_eq!(record.source, "host.agent");
        assert_eq!(record.payload["event"], "reflection_completed");
        drop(ledger);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn acknowledgement_requires_the_delivered_token_and_watermark() {
        let (ledger, path) = ledger();
        ledger
            .record_polled_event("dev.social", &polled("post-1", "hello"))
            .expect("record");
        let batch = ledger.pending_skill_events().expect("batch");
        let receipt = batch.receipt.as_ref().expect("receipt");

        assert!(ledger
            .database()
            .acknowledge_plugin_event_delivery(
                AGENT_EVENT_CONSUMER_ID,
                SKILL_EVENT_SUBSCRIPTION,
                &receipt.delivery_token,
                receipt.through_sequence + 100,
            )
            .is_err());
        assert_eq!(
            ledger
                .database()
                .get_plugin_event_cursor(AGENT_EVENT_CONSUMER_ID, SKILL_EVENT_SUBSCRIPTION)
                .expect("cursor query")
                .expect("registered cursor")
                .last_sequence,
            0
        );

        let mut forged = batch.clone();
        forged.receipt.as_mut().expect("receipt").delivery_token = "forged".to_string();
        assert!(ledger.acknowledge_skill_events(&forged).is_err());
        assert_eq!(
            ledger
                .database()
                .get_plugin_event_cursor(AGENT_EVENT_CONSUMER_ID, SKILL_EVENT_SUBSCRIPTION)
                .expect("cursor query")
                .expect("registered cursor")
                .last_sequence,
            0
        );

        ledger
            .acknowledge_skill_events(&batch)
            .expect("valid acknowledgement");
        drop(ledger);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unsupported_rows_are_dead_lettered_and_do_not_block_later_events() {
        let (ledger, path) = ledger();
        ledger
            .database()
            .append_plugin_event(&NewPluginEvent {
                event_id: "future-event".to_string(),
                event_type: SKILL_EVENT_SUBSCRIPTION.to_string(),
                schema_version: 2,
                source: "dev.future".to_string(),
                source_event_id: Some("future-1".to_string()),
                occurred_at: Utc::now(),
                correlation_id: None,
                causation_id: None,
                payload: serde_json::to_value(polled("future-1", "future")).expect("serialize"),
            })
            .expect("append future event");
        ledger
            .record_polled_event("dev.social", &polled("post-2", "later"))
            .expect("append supported event");

        let batch = ledger.pending_skill_events().expect("batch");
        assert_eq!(batch.quarantined_count, 1);
        assert_eq!(batch.events.len(), 1);
        assert!(matches!(
            &batch.events[0],
            SkillEvent::NewContent { body, .. } if body == "later"
        ));
        ledger
            .acknowledge_skill_events(&batch)
            .expect("acknowledge");
        assert_eq!(
            ledger
                .database()
                .list_plugin_event_dead_letters(10)
                .expect("dead letters")
                .len(),
            1
        );
        assert!(ledger
            .pending_skill_events()
            .expect("empty")
            .events
            .is_empty());
        drop(ledger);
        let _ = std::fs::remove_file(path);
    }
}
