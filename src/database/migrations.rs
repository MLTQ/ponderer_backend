use anyhow::Result;

use super::AgentDatabase;

impl AgentDatabase {
    /// Transactional and repeatable for both fresh databases and pre-Loose installs.
    pub(super) fn migrate_continuity_schema(&self) -> Result<()> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);",
        )?;
        let sql: String = tx.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='agent_intentions'",
            [],
            |r| r.get(0),
        )?;
        if !sql.contains("'self_authored'") {
            let upgraded = sql
                .replacen("agent_intentions", "agent_intentions_v2", 1)
                .replace("'system'", "'system', 'self_authored'");
            tx.execute_batch(&upgraded)?;
            tx.execute_batch("INSERT INTO agent_intentions_v2 SELECT * FROM agent_intentions;
                DROP TABLE agent_intentions;
                ALTER TABLE agent_intentions_v2 RENAME TO agent_intentions;
                CREATE INDEX idx_agent_intentions_eligible ON agent_intentions(status,next_eligible_at,due_at,priority DESC,created_at ASC);
                CREATE INDEX idx_agent_intentions_origin_updated ON agent_intentions(origin,updated_at DESC);
                CREATE UNIQUE INDEX idx_agent_intentions_source ON agent_intentions(origin,source_reference) WHERE source_reference IS NOT NULL AND TRIM(source_reference) <> '';")?;
        }
        tx.execute_batch("INSERT OR IGNORE INTO schema_migrations VALUES (1);
            CREATE TABLE IF NOT EXISTS contact_endpoints (
                channel TEXT PRIMARY KEY, endpoint TEXT NOT NULL, owner TEXT NOT NULL,
                enabled INTEGER NOT NULL, activated_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS connector_receipts (
                source TEXT NOT NULL, event_id TEXT NOT NULL, message_id TEXT,
                PRIMARY KEY(source,event_id));
            CREATE TABLE IF NOT EXISTS communication_intents (
                id TEXT PRIMARY KEY, source_id TEXT NOT NULL UNIQUE, topic TEXT NOT NULL,
                decision_json TEXT NOT NULL, status TEXT NOT NULL, reason TEXT NOT NULL,
                created_at TEXT NOT NULL, expires_at TEXT NOT NULL, next_eligible_at TEXT NOT NULL,
                conversation_id TEXT, message_id TEXT, feedback TEXT, reserved_at TEXT);
            CREATE TABLE IF NOT EXISTS delivery_outbox (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL, intent_id TEXT,
                endpoint TEXT NOT NULL, part INTEGER NOT NULL, content TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TEXT NOT NULL, lease_until TEXT, lease_token TEXT,
                provider_message_id TEXT, last_error TEXT, expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL, UNIQUE(message_id,endpoint,part));
            CREATE INDEX IF NOT EXISTS idx_outbox_due ON delivery_outbox(state,next_attempt_at);
            CREATE TABLE IF NOT EXISTS continuity_decisions (
                id TEXT PRIMARY KEY, source_id TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL,
                decision_json TEXT NOT NULL, next_wake_at TEXT NOT NULL, evidence_json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS self_claims (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, claim TEXT NOT NULL,
                confidence REAL NOT NULL, evidence_json TEXT NOT NULL, counterevidence_json TEXT NOT NULL,
                first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS agent_drives (
                kind TEXT PRIMARY KEY, pressure REAL NOT NULL, reason TEXT NOT NULL,
                evidence_json TEXT NOT NULL, updated_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS lived_outcomes (
                id TEXT PRIMARY KEY, source_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
                summary TEXT NOT NULL, created_at TEXT NOT NULL);
            INSERT OR IGNORE INTO schema_migrations VALUES (2);")?;
        tx.commit()?;
        Ok(())
    }
}
