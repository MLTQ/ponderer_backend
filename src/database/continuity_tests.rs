use super::*;
use crate::agent::continuity::{ContinuityDecision, Evidence};
use crate::config::OutreachConfig;
use crate::intentions::{IntentionOrigin, NewAgentIntention};
use chrono::{Duration, Utc};

fn evidence() -> Vec<Evidence> {
    vec![Evidence {
        id: "journal:real".into(),
        content: "A small prototype is ready to discuss.".into(),
    }]
}
fn decision(topic: &str) -> ContinuityDecision {
    serde_json::from_value(serde_json::json!({
        "reflection":"There is a concrete result worth sharing.","next_wake_secs":900,"wake_reason":"Review outcome",
        "claims":[{"kind":"curiosity","claim":"I return to small working prototypes.","confidence":0.7,"evidence_ids":["journal:real"]}],
        "drives":[{"kind":"connection","pressure":0.3,"reason":"Share useful progress","evidence_ids":["journal:real"]}],
        "communication":{"choice":"send","topic":topic,"message":"The prototype is ready; want to take a look?",
            "reason":"A completed result is available now","confidence":0.9,"evidence_ids":["journal:real"],"expires_after_secs":7200},
        "goal":{"summary":"Evaluate the prototype","motivation":"Check that it is useful","first_step":"Review the result",
            "priority":0.5,"evidence_ids":["journal:real"]}
    })).unwrap()
}
fn database() -> (tempfile::TempDir, AgentDatabase) {
    let dir = tempfile::tempdir().unwrap();
    let db = AgentDatabase::new(dir.path().join("test.db")).unwrap();
    (dir, db)
}
fn count(db: &AgentDatabase, table: &str) -> i64 {
    db.lock_conn()
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn reflection_to_outreach_is_atomic_restart_safe_and_grounded() {
    let (dir, db) = database();
    let now = Utc::now();
    db.configure_telegram_endpoint(Some(42), now).unwrap();
    let decision = decision("prototype-ready");
    assert!(db
        .save_appraisal("orientation:one", &decision, &evidence(), now)
        .unwrap());
    assert!(!db
        .save_appraisal("orientation:one", &decision, &evidence(), now)
        .unwrap());
    assert_eq!(count(&db, "agent_intentions"), 1);
    assert_eq!(count(&db, "communication_intents"), 1);
    let policy = OutreachConfig::default();
    assert_eq!(
        db.release_communication_intents(&policy, false, 14, now)
            .unwrap(),
        vec!["telegram:42"]
    );
    let delivery = db
        .claim_delivery(42, &policy, 14, false, now)
        .unwrap()
        .unwrap();
    assert!(delivery.content.contains("prototype"));
    assert!(delivery.intent_id.is_some());
    assert!(db
        .settle_delivery(&delivery, "sent", Some(100), None, 0, now)
        .unwrap());
    assert!(!db
        .settle_delivery(&delivery, "sent", Some(100), None, 0, now)
        .unwrap());
    drop(db);
    let db = AgentDatabase::new(dir.path().join("test.db")).unwrap();
    assert!(db
        .claim_delivery(42, &policy, 14, false, now + Duration::minutes(2))
        .unwrap()
        .is_none());
    assert!(db
        .release_communication_intents(&policy, false, 14, now)
        .unwrap()
        .is_empty());
    assert_eq!(db.recent_contact_records().unwrap()[0].status, "delivered");
    assert!(db
        .continuity_evidence(now)
        .unwrap()
        .iter()
        .any(|e| e.content.contains("confirmed by Telegram")));
    // A later reflection about the same topic cannot publish it again.
    db.save_appraisal(
        "orientation:two",
        &decision,
        &evidence(),
        now + Duration::hours(1),
    )
    .unwrap();
    assert!(db
        .release_communication_intents(&policy, false, 14, now + Duration::hours(1))
        .unwrap()
        .is_empty());
}

#[test]
fn quiet_hours_pause_expiry_and_feedback_are_host_enforced() {
    let (_dir, db) = database();
    let now = Utc::now();
    let mut policy = OutreachConfig::default();
    db.configure_telegram_endpoint(Some(42), now).unwrap();
    db.save_appraisal("a", &decision("a"), &evidence(), now)
        .unwrap();
    assert!(db
        .release_communication_intents(&policy, false, 23, now)
        .unwrap()
        .is_empty());
    assert!(db
        .release_communication_intents(&policy, true, 14, now + Duration::minutes(6))
        .unwrap()
        .is_empty());
    assert_eq!(
        db.release_communication_intents(&policy, false, 14, now + Duration::minutes(12))
            .unwrap()
            .len(),
        1
    );
    assert!(db
        .claim_delivery(42, &policy, 14, true, now + Duration::minutes(12))
        .unwrap()
        .is_none());
    assert!(db
        .claim_delivery(42, &policy, 23, false, now + Duration::minutes(12))
        .unwrap()
        .is_none());
    let contact = &db.recent_contact_records().unwrap()[0];
    assert!(!db
        .record_contact_feedback(&contact.id, "dismissed", Some(99), now)
        .unwrap());
    assert!(db
        .record_contact_feedback(&contact.id, "dismissed", Some(42), now)
        .unwrap());
    assert!(!db
        .record_contact_feedback(&contact.id, "dismissed", Some(42), now)
        .unwrap());
    db.save_appraisal(
        "b",
        &decision("b"),
        &evidence(),
        now + Duration::minutes(13),
    )
    .unwrap();
    policy.min_interval_secs = 0;
    assert!(db
        .release_communication_intents(&policy, false, 14, now + Duration::hours(1))
        .unwrap()
        .is_empty());
    assert!(db
        .claim_delivery(42, &policy, 14, false, now + Duration::hours(3))
        .unwrap()
        .is_none());
    assert!(db
        .continuity_evidence(now)
        .unwrap()
        .iter()
        .any(|e| e.content.contains("explicitly dismissed")));
}

#[test]
fn telegram_receipts_deduplicate_and_unverified_operator_turns_have_no_authority() {
    let (_dir, db) = database();
    let now = Utc::now();
    assert!(!db
        .ingest_telegram_update("telegram:bot", 1, 42, Some("not configured"), now)
        .unwrap());
    db.configure_telegram_endpoint(Some(42), now).unwrap();
    assert!(!db
        .ingest_telegram_update("telegram:bot", 2, 99, Some("wrong owner"), now)
        .unwrap());
    assert!(db
        .ingest_telegram_update("telegram:bot", 3, 42, Some("hello"), now)
        .unwrap());
    assert!(!db
        .ingest_telegram_update("telegram:bot", 3, 42, Some("hello"), now)
        .unwrap());
    assert_eq!(db.telegram_offset("telegram:bot").unwrap(), 4);
    let messages = db.get_unprocessed_operator_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(db
        .telegram_message_is_authorized(&messages[0].id, Some(42))
        .unwrap());
    let forged = db
        .add_chat_message_in_conversation("telegram:42", "operator", "no receipt")
        .unwrap();
    assert!(!db
        .telegram_message_is_authorized(&forged, Some(42))
        .unwrap());
    db.configure_telegram_endpoint(Some(43), now).unwrap();
    assert!(!db
        .telegram_message_is_authorized(&messages[0].id, Some(42))
        .unwrap());
}

#[test]
fn replies_are_durable_complete_and_private_metadata_stays_private() {
    let (_dir, db) = database();
    db.configure_telegram_endpoint(Some(42), Utc::now())
        .unwrap();
    let visible = "🌱".repeat(5000);
    db.add_chat_message_in_conversation(
        "telegram:42",
        "agent",
        &format!("[thinking]private[/thinking]{visible}[tool_calls]secret[/tool_calls]"),
    )
    .unwrap();
    let now = Utc::now();
    let policy = OutreachConfig {
        enabled: false,
        ..OutreachConfig::default()
    };
    let mut actual = String::new();
    while let Some(delivery) = db.claim_delivery(42, &policy, 23, false, now).unwrap() {
        assert!(delivery.intent_id.is_none());
        assert!(delivery.content.encode_utf16().count() <= 4096);
        actual.push_str(&delivery.content);
        db.settle_delivery(&delivery, "sent", Some(1), None, 0, now)
            .unwrap();
    }
    assert_eq!(actual, visible);
}

#[test]
fn interrupted_sends_become_uncertain_and_keep_contact_reservations() {
    let (dir, db) = database();
    let now = Utc::now();
    db.configure_telegram_endpoint(Some(42), now).unwrap();
    db.save_appraisal("a", &decision("a"), &evidence(), now)
        .unwrap();
    let policy = OutreachConfig::default();
    db.release_communication_intents(&policy, false, 14, now)
        .unwrap();
    let claimed = db
        .claim_delivery(42, &policy, 14, false, now)
        .unwrap()
        .unwrap();
    drop(db);
    let db = AgentDatabase::new(dir.path().join("test.db")).unwrap();
    assert!(db
        .claim_delivery(42, &policy, 14, false, now + Duration::minutes(3))
        .unwrap()
        .is_none());
    assert_eq!(db.recent_contact_records().unwrap()[0].status, "uncertain");
    assert!(!db
        .settle_delivery(&claimed, "sent", Some(1), None, 0, now)
        .unwrap());
    db.save_appraisal("b", &decision("b"), &evidence(), now + Duration::minutes(4))
        .unwrap();
    assert!(db
        .release_communication_intents(&policy, false, 14, now + Duration::minutes(4))
        .unwrap()
        .is_empty());
}

#[test]
fn invalid_appraisals_cannot_commit_partial_effects_and_claims_can_change() {
    let (_dir, db) = database();
    let now = Utc::now();
    let mut d = decision("one");
    d.communication.as_mut().unwrap().evidence_ids = vec!["invented".into()];
    assert!(db.save_appraisal("bad", &d, &evidence(), now).is_err());
    assert_eq!(count(&db, "continuity_decisions"), 0);
    assert_eq!(count(&db, "self_claims"), 0);
    d = decision("one");
    db.save_appraisal("good", &d, &evidence(), now).unwrap();
    let model: serde_json::Value =
        serde_json::from_str(&db.self_model_context(now).unwrap()).unwrap();
    d.claims[0].id = Some(model["revisable_claims"][0]["id"].as_str().unwrap().into());
    d.claims[0].claim = "I prefer testing prototypes before sharing them.".into();
    d.claims[0].confidence = 0.5;
    d.claims[0].counterevidence_ids = vec!["journal:real".into()];
    d.communication = None;
    db.save_appraisal("revision", &d, &evidence(), now + Duration::minutes(2))
        .unwrap();
    assert_eq!(count(&db, "self_claims"), 1);
    assert!(db
        .self_model_context(now)
        .unwrap()
        .contains("testing prototypes"));
    assert_eq!(count(&db, "agent_intentions"), 1);
    let fresh: serde_json::Value =
        serde_json::from_str(&db.self_model_context(now + Duration::days(3)).unwrap()).unwrap();
    assert!(fresh["drives"][0]["pressure"].as_f64().unwrap() < 0.1);
}

#[test]
fn old_origin_constraint_is_migrated_without_losing_intentions() {
    let (dir, db) = database();
    let now = Utc::now();
    let original = db
        .create_intention(
            NewAgentIntention::new(IntentionOrigin::System, "Keep me", "Test migration"),
            now,
        )
        .unwrap();
    let conn = db.lock_conn().unwrap();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='agent_intentions'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let old = sql
        .replace("agent_intentions", "old_intentions")
        .replace(", 'self_authored'", "");
    // Original CHECK lists self_authored before system on fresh installations.
    let old = old.replace("'self_authored', ", "");
    assert!(!old.contains("'self_authored'"));
    conn.execute_batch(&format!(
        "{old}; INSERT INTO old_intentions SELECT * FROM agent_intentions;
        DROP TABLE agent_intentions; ALTER TABLE old_intentions RENAME TO agent_intentions;"
    ))
    .unwrap();
    drop(conn);
    drop(db);
    let db = AgentDatabase::new(dir.path().join("test.db")).unwrap();
    assert!(db.get_intention(&original.id).unwrap().is_some());
    db.create_intention(
        NewAgentIntention::new(IntentionOrigin::SelfAuthored, "New goal", "Chosen"),
        now,
    )
    .unwrap();
    drop(db);
    let db = AgentDatabase::new(dir.path().join("test.db")).unwrap();
    assert_eq!(count(&db, "agent_intentions"), 2);
}

#[test]
fn waiting_operator_preempts_spontaneous_contact_even_after_reservation() {
    let (_dir, db) = database();
    let now = Utc::now();
    let policy = OutreachConfig::default();
    db.configure_telegram_endpoint(Some(42), now).unwrap();
    db.save_appraisal("a", &decision("a"), &evidence(), now)
        .unwrap();
    let message = db
        .add_chat_message("operator", "Please help with this first")
        .unwrap();
    assert!(db
        .release_communication_intents(&policy, false, 14, now)
        .unwrap()
        .is_empty());
    db.mark_message_processed(&message).unwrap();
    assert_eq!(
        db.release_communication_intents(&policy, false, 14, now)
            .unwrap()
            .len(),
        1
    );
    db.add_chat_message("operator", "One more question")
        .unwrap();
    assert!(db
        .claim_delivery(42, &policy, 14, false, now)
        .unwrap()
        .is_none());
}
