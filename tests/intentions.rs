use chrono::{Duration, Utc};
use ponderer_backend::database::AgentDatabase;
use ponderer_backend::intentions::{
    IntentionAttemptOutcome, IntentionOrigin, IntentionStatus, NewAgentIntention,
};

fn temp_db_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ponderer_intentions_integration_{}.db",
        uuid::Uuid::new_v4()
    ))
}

#[test]
fn durable_intention_round_trip_deduplicates_and_recovers_work() {
    let path = temp_db_path();
    let now = Utc::now();
    let intention_id = {
        let db = AgentDatabase::new(&path).unwrap();
        let mut draft = NewAgentIntention::new(
            IntentionOrigin::OrientationThought,
            "return to an unfinished reflection",
            "maintain continuity across process lifetimes",
        );
        draft.priority = 0.8;
        draft.source_reference = Some("pending-thought:integration-1".into());

        let (created, inserted) = db.create_intention_if_absent(draft.clone(), now).unwrap();
        assert!(inserted);
        let (same, inserted) = db
            .create_intention_if_absent(draft, now + Duration::seconds(1))
            .unwrap();
        assert!(!inserted);
        assert_eq!(same.id, created.id);

        let claimed = db
            .claim_intention(&created.id, now, "orientation-loop", Duration::minutes(1))
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, created.id);
        assert_eq!(claimed.status, IntentionStatus::Claimed);
        created.id
    };

    let reopened = AgentDatabase::new(&path).unwrap();
    assert_eq!(
        reopened
            .recover_expired_intention_claims(now + Duration::minutes(1))
            .unwrap(),
        1
    );
    let reclaimed = reopened
        .claim_next_intention(
            now + Duration::minutes(1),
            "restart-loop",
            Duration::minutes(1),
        )
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.id, intention_id);
    assert_eq!(reclaimed.attempt_count, 2);

    let completed = reopened
        .transition_claimed_intention(
            &intention_id,
            "restart-loop",
            IntentionAttemptOutcome::Completed {
                outcome: "reflection was resumed".into(),
            },
            now + Duration::minutes(2),
        )
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, IntentionStatus::Completed);
    assert_eq!(
        completed.last_outcome.as_deref(),
        Some("reflection was resumed")
    );
    assert_eq!(completed.last_outcome_at, completed.completed_at);
    assert!(reopened
        .list_open_intentions(Some(IntentionOrigin::OrientationThought), 10)
        .unwrap()
        .is_empty());
    assert!(reopened
        .claim_next_intention(
            now + Duration::minutes(3),
            "orientation-loop",
            Duration::minutes(1),
        )
        .unwrap()
        .is_none());

    drop(reopened);
    let _ = std::fs::remove_file(path);
}
