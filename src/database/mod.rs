use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

use crate::memory::{KvMemoryBackend, MemoryBackend, MemoryMigrationRegistry};

mod helpers;

pub mod chat;
pub mod concerns;
pub mod journal;
pub mod memory;
pub mod orientation;
pub mod persona;
pub mod posts;
pub mod scheduled_jobs;

// Re-export public types
pub use chat::{
    ChatConversation, ChatConversationSummary, ChatMessage, ChatSession, ChatTurn, ChatTurnPhase,
    ChatTurnToolCall, OodaTurnPacketRecord, DEFAULT_CHAT_CONVERSATION_ID, DEFAULT_CHAT_SESSION_ID,
    TELEGRAM_CONVERSATION_ID,
};
pub use orientation::{OrientationSnapshotRecord, PendingThoughtRecord};
pub use persona::{CharacterCard, PersonaSnapshot, PersonaTraits, ReflectionRecord};
pub use posts::ImportantPost;

pub struct AgentDatabase {
    pub(super) conn: Mutex<Connection>,
    pub(super) memory_backend: Box<dyn MemoryBackend>,
    pub(super) migration_registry: MemoryMigrationRegistry,
}

impl AgentDatabase {
    /// Helper to lock the connection
    pub(super) fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Database lock poisoned: {}", e))
    }

    /// Create or open the database
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self {
            conn: Mutex::new(conn),
            memory_backend: Box::new(KvMemoryBackend::new()),
            migration_registry: MemoryMigrationRegistry::default(),
        };
        db.ensure_schema()?;
        db.ensure_memory_design_state()?;
        Ok(db)
    }

    fn ensure_memory_design_state(&self) -> Result<()> {
        if self.get_memory_design_version()?.is_none() {
            self.set_memory_design_version(&self.memory_backend.design_version())?;
        }
        Ok(())
    }

    fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(columns.iter().any(|name| name == column))
    }

    fn ensure_chat_messages_conversation_column(&self, conn: &Connection) -> Result<()> {
        if !Self::table_has_column(conn, "chat_messages", "conversation_id")? {
            conn.execute(
                "ALTER TABLE chat_messages ADD COLUMN conversation_id TEXT NOT NULL DEFAULT 'default'",
                [],
            )?;
        }

        if !Self::table_has_column(conn, "chat_messages", "turn_id")? {
            conn.execute("ALTER TABLE chat_messages ADD COLUMN turn_id TEXT", [])?;
        }

        conn.execute(
            "UPDATE chat_messages SET conversation_id = ?1 WHERE conversation_id IS NULL OR TRIM(conversation_id) = ''",
            [DEFAULT_CHAT_CONVERSATION_ID],
        )?;

        Ok(())
    }

    fn ensure_chat_conversations_runtime_columns(&self, conn: &Connection) -> Result<()> {
        if !Self::table_has_column(conn, "chat_conversations", "session_id")? {
            conn.execute(
                "ALTER TABLE chat_conversations ADD COLUMN session_id TEXT NOT NULL DEFAULT 'default_session'",
                [],
            )?;
        }
        if !Self::table_has_column(conn, "chat_conversations", "runtime_state")? {
            conn.execute(
                "ALTER TABLE chat_conversations ADD COLUMN runtime_state TEXT NOT NULL DEFAULT 'idle'",
                [],
            )?;
        }
        if !Self::table_has_column(conn, "chat_conversations", "active_turn_id")? {
            conn.execute(
                "ALTER TABLE chat_conversations ADD COLUMN active_turn_id TEXT",
                [],
            )?;
        }

        conn.execute(
            "UPDATE chat_conversations
             SET session_id = ?1
             WHERE session_id IS NULL OR TRIM(session_id) = ''",
            [DEFAULT_CHAT_SESSION_ID],
        )?;
        conn.execute(
            "UPDATE chat_conversations
             SET runtime_state = ?1
             WHERE runtime_state IS NULL OR TRIM(runtime_state) = ''",
            [ChatTurnPhase::Idle.as_db_str()],
        )?;

        Ok(())
    }

    fn ensure_chat_turns_prompt_columns(&self, conn: &Connection) -> Result<()> {
        if !Self::table_has_column(conn, "chat_turns", "prompt_text")? {
            conn.execute("ALTER TABLE chat_turns ADD COLUMN prompt_text TEXT", [])?;
        }
        if !Self::table_has_column(conn, "chat_turns", "system_prompt_text")? {
            conn.execute(
                "ALTER TABLE chat_turns ADD COLUMN system_prompt_text TEXT",
                [],
            )?;
        }
        Ok(())
    }

    fn ensure_default_chat_session(&self, conn: &Connection) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO chat_sessions (id, label, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![DEFAULT_CHAT_SESSION_ID, "Default session", now.clone(), now],
        )?;
        Ok(())
    }

    fn ensure_default_chat_conversation(&self, conn: &Connection) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO chat_conversations (id, session_id, title, created_at, updated_at, runtime_state, active_turn_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                DEFAULT_CHAT_CONVERSATION_ID,
                DEFAULT_CHAT_SESSION_ID,
                "Default chat",
                now.clone(),
                now,
                ChatTurnPhase::Idle.as_db_str(),
            ],
        )?;
        Ok(())
    }

    /// Create the database schema
    fn ensure_schema(&self) -> Result<()> {
        let conn = self.lock_conn()?;

        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS important_posts (
                id TEXT PRIMARY KEY,
                post_id TEXT NOT NULL,
                thread_id TEXT NOT NULL,
                post_body TEXT NOT NULL,
                why_important TEXT NOT NULL,
                importance_score REAL NOT NULL,
                marked_at TEXT NOT NULL
            )"#,
            [],
        )?;

        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS reflection_history (
                id TEXT PRIMARY KEY,
                reflected_at TEXT NOT NULL,
                old_prompt TEXT NOT NULL,
                new_prompt TEXT NOT NULL,
                reasoning TEXT NOT NULL,
                guiding_principles TEXT NOT NULL
            )"#,
            [],
        )?;

        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS agent_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )"#,
            [],
        )?;

        // Create index for faster lookups
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_important_posts_marked_at ON important_posts(marked_at DESC)",
            [],
        )?;

        // Character card storage
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS character_cards (
                id TEXT PRIMARY KEY,
                imported_at TEXT NOT NULL,
                format TEXT NOT NULL,
                original_data TEXT NOT NULL,
                derived_prompt TEXT NOT NULL,
                name TEXT,
                description TEXT
            )"#,
            [],
        )?;

        // Persona history for Ludonarrative Assonantic Tracing
        // Tracks personality evolution over time
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS persona_history (
                id TEXT PRIMARY KEY,
                captured_at TEXT NOT NULL,
                traits_json TEXT NOT NULL,
                system_prompt TEXT NOT NULL,
                trigger TEXT NOT NULL,
                self_description TEXT NOT NULL,
                inferred_trajectory TEXT,
                formative_experiences_json TEXT NOT NULL
            )"#,
            [],
        )?;

        // Index for chronological persona queries
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_persona_history_captured_at ON persona_history(captured_at DESC)",
            [],
        )?;

        // Working memory - agent's persistent scratchpad
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS working_memory (
                key TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"#,
            [],
        )?;

        // Memory design archive
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS memory_design_archive (
                id TEXT PRIMARY KEY,
                design_id TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                description TEXT,
                metadata_json TEXT,
                created_at TEXT NOT NULL,
                UNIQUE(design_id, schema_version)
            )"#,
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_design_archive_created_at ON memory_design_archive(created_at DESC)",
            [],
        )?;

        // Stored memory eval runs (raw report JSON)
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS memory_eval_runs (
                id TEXT PRIMARY KEY,
                trace_set_name TEXT NOT NULL,
                report_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            )"#,
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_eval_runs_created_at ON memory_eval_runs(created_at DESC)",
            [],
        )?;

        // Promotion decisions derived from eval runs and policies
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS memory_promotion_decisions (
                id TEXT PRIMARY KEY,
                eval_run_id TEXT NOT NULL,
                candidate_design_id TEXT NOT NULL,
                candidate_schema_version INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                rationale TEXT NOT NULL,
                policy_json TEXT NOT NULL,
                metrics_snapshot_json TEXT NOT NULL,
                rollback_design_id TEXT NOT NULL,
                rollback_schema_version INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY(eval_run_id) REFERENCES memory_eval_runs(id)
            )"#,
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_promotion_decisions_created_at ON memory_promotion_decisions(created_at DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_promotion_decisions_eval_run_id ON memory_promotion_decisions(eval_run_id)",
            [],
        )?;

        // Session metadata for grouping chat threads over long-running use.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS chat_sessions (
                id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"#,
            [],
        )?;

        // Conversation metadata for private operator chat.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS chat_conversations (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL DEFAULT 'default_session',
                title TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                runtime_state TEXT NOT NULL DEFAULT 'idle',
                active_turn_id TEXT
            )"#,
            [],
        )?;

        // Compacted long-context snapshot for one conversation.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS chat_conversation_summaries (
                conversation_id TEXT PRIMARY KEY,
                summary_text TEXT NOT NULL,
                summarized_message_count INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            )"#,
            [],
        )?;

        // Private chat between operator and agent.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS chat_messages (
                id TEXT PRIMARY KEY,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL,
                processed INTEGER NOT NULL DEFAULT 0,
                turn_id TEXT
            )"#,
            [],
        )?;

        // Conversation turns with explicit lifecycle.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS chat_turns (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                iteration INTEGER NOT NULL,
                phase_state TEXT NOT NULL,
                decision TEXT,
                status TEXT,
                trigger_message_ids_json TEXT NOT NULL,
                operator_message TEXT,
                reason TEXT,
                error TEXT,
                tool_call_count INTEGER NOT NULL DEFAULT 0,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                agent_message_id TEXT,
                prompt_text TEXT,
                system_prompt_text TEXT
            )"#,
            [],
        )?;

        // Tool output lineage for each turn.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS chat_turn_tool_calls (
                id TEXT PRIMARY KEY,
                turn_id TEXT NOT NULL,
                call_index INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                arguments_json TEXT NOT NULL,
                output_text TEXT NOT NULL,
                created_at TEXT NOT NULL
            )"#,
            [],
        )?;

        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS scheduled_jobs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                prompt TEXT NOT NULL,
                interval_minutes INTEGER NOT NULL,
                conversation_id TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                last_run_at TEXT,
                next_run_at TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"#,
            [],
        )?;

        // Living Loop foundation: private journal entries.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS journal_entries (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                entry_type TEXT NOT NULL,
                content TEXT NOT NULL,
                trigger TEXT,
                user_state_at_time TEXT,
                time_of_day TEXT,
                related_concerns TEXT,
                mood_valence REAL,
                mood_arousal REAL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )"#,
            [],
        )?;

        // Living Loop foundation: ongoing concerns.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS concerns (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                last_touched TEXT NOT NULL,
                summary TEXT NOT NULL,
                concern_type TEXT NOT NULL,
                salience TEXT NOT NULL,
                my_thoughts TEXT,
                related_memory_keys TEXT,
                context TEXT,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )"#,
            [],
        )?;

        // Living Loop foundation: orientation snapshots for debugging/analysis.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS orientation_snapshots (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                user_state TEXT NOT NULL,
                disposition TEXT NOT NULL,
                synthesis TEXT NOT NULL,
                salience_map TEXT,
                anomalies TEXT,
                pending_thoughts TEXT,
                mood_valence REAL,
                mood_arousal REAL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )"#,
            [],
        )?;

        // Living Loop foundation: queued thoughts to surface later.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS pending_thoughts_queue (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                context TEXT,
                priority REAL NOT NULL DEFAULT 0.5,
                relates_to TEXT,
                created_at TEXT NOT NULL,
                surfaced_at TEXT,
                dismissed_at TEXT
            )"#,
            [],
        )?;

        // Persist compact Observe/Orient/Decide/Act packets per turn for baton-style context carryover.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS ooda_turn_packets (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                turn_id TEXT,
                observe TEXT NOT NULL,
                orient TEXT NOT NULL,
                decide TEXT NOT NULL,
                act TEXT NOT NULL,
                created_at TEXT NOT NULL
            )"#,
            [],
        )?;

        self.ensure_chat_messages_conversation_column(&conn)?;
        self.ensure_chat_conversations_runtime_columns(&conn)?;
        self.ensure_chat_turns_prompt_columns(&conn)?;
        self.ensure_default_chat_session(&conn)?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_messages_created_at ON chat_messages(created_at ASC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_messages_conversation_created_at ON chat_messages(conversation_id, created_at ASC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_messages_turn_id ON chat_messages(turn_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_conversations_session_updated ON chat_conversations(session_id, updated_at DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_conversations_runtime_state ON chat_conversations(runtime_state)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_conversation_summaries_updated_at ON chat_conversation_summaries(updated_at DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_turns_conversation_started ON chat_turns(conversation_id, started_at DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_turns_phase_state ON chat_turns(phase_state)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chat_turn_tool_calls_turn_idx ON chat_turn_tool_calls(turn_id, call_index ASC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_due ON scheduled_jobs(enabled, next_run_at ASC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_journal_timestamp ON journal_entries(timestamp DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_journal_type ON journal_entries(entry_type)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_concerns_salience ON concerns(salience)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_concerns_last_touched ON concerns(last_touched DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_orientation_timestamp ON orientation_snapshots(timestamp DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_unsurfaced ON pending_thoughts_queue(surfaced_at) WHERE surfaced_at IS NULL",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_ooda_packets_conversation_created ON ooda_turn_packets(conversation_id, created_at DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_ooda_packets_turn_id ON ooda_turn_packets(turn_id)",
            [],
        )?;

        self.ensure_default_chat_conversation(&conn)?;

        Ok(())
    }

    /// Get a state value
    pub fn get_state(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock_conn()?;
        let result = conn.query_row(
            "SELECT value FROM agent_state WHERE key = ?1",
            [key],
            |row| row.get(0),
        );

        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a state value
    pub fn set_state(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO agent_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::concerns::{Concern, ConcernContext, ConcernType, Salience};
    use crate::agent::journal::{JournalContext, JournalEntry, JournalEntryType, JournalMood};
    use chrono::Duration as ChronoDuration;
    use std::path::PathBuf;

    fn temp_db_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("ponderer_{}_{}.db", name, uuid::Uuid::new_v4()));
        path
    }

    #[test]
    fn chat_turn_lifecycle_persists_state_and_tool_calls() {
        let path = temp_db_path("chat_turn_lifecycle");
        let db = AgentDatabase::new(&path).expect("db init");

        let conversation = db
            .create_chat_conversation(Some("Lifecycle test"))
            .expect("create conversation");
        let operator_message_id = db
            .add_chat_message_in_conversation(&conversation.id, "operator", "Please investigate.")
            .expect("insert operator message");

        let turn_id = db
            .begin_chat_turn(
                &conversation.id,
                std::slice::from_ref(&operator_message_id),
                1,
            )
            .expect("begin turn");
        db.set_chat_turn_prompt_bundle(&turn_id, "USER PROMPT", "SYSTEM PROMPT")
            .expect("persist prompt bundle");

        db.record_chat_turn_tool_call(
            &turn_id,
            0,
            "list_directory",
            r#"{"path":"."}"#,
            "Found 3 entries",
        )
        .expect("record tool call");

        let agent_message_id = db
            .add_chat_message_in_turn(&conversation.id, &turn_id, "agent", "Done.")
            .expect("insert agent turn message");

        db.mark_message_processed(&operator_message_id)
            .expect("mark processed");

        db.complete_chat_turn(
            &turn_id,
            ChatTurnPhase::Completed,
            "yield",
            "done",
            "Done.",
            Some("Completed investigation"),
            1,
            Some(&agent_message_id),
        )
        .expect("complete turn");

        let turns = db
            .list_chat_turns_for_conversation(&conversation.id, 10)
            .expect("list turns");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].id, turn_id);
        assert_eq!(turns[0].phase_state, ChatTurnPhase::Completed);
        assert_eq!(turns[0].tool_call_count, 1);
        assert_eq!(
            turns[0].agent_message_id.as_deref(),
            Some(agent_message_id.as_str())
        );
        assert_eq!(
            turns[0].trigger_message_ids,
            vec![operator_message_id.clone()]
        );
        assert_eq!(turns[0].prompt_text.as_deref(), Some("USER PROMPT"));
        assert_eq!(
            turns[0].system_prompt_text.as_deref(),
            Some("SYSTEM PROMPT")
        );

        let prompt_bundle = db
            .get_chat_turn_prompt_bundle(&turn_id)
            .expect("load prompt bundle")
            .expect("bundle exists");
        assert_eq!(prompt_bundle.0.as_deref(), Some("USER PROMPT"));
        assert_eq!(prompt_bundle.1.as_deref(), Some("SYSTEM PROMPT"));

        let tool_calls = db
            .list_chat_turn_tool_calls(&turn_id)
            .expect("list tool calls");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool_name, "list_directory");

        let conversations = db.list_chat_conversations(50).expect("list conversations");
        let convo_state = conversations
            .iter()
            .find(|c| c.id == conversation.id)
            .expect("find conversation");
        assert_eq!(convo_state.runtime_state, ChatTurnPhase::Completed);
        assert_eq!(convo_state.active_turn_id, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn failed_turn_keeps_operator_message_unprocessed_for_retry() {
        let path = temp_db_path("chat_turn_failure");
        let db = AgentDatabase::new(&path).expect("db init");

        let conversation = db
            .create_chat_conversation(Some("Failure test"))
            .expect("create conversation");
        let operator_message_id = db
            .add_chat_message_in_conversation(&conversation.id, "operator", "Try and fail")
            .expect("insert operator message");

        let turn_id = db
            .begin_chat_turn(
                &conversation.id,
                std::slice::from_ref(&operator_message_id),
                1,
            )
            .expect("begin turn");
        db.fail_chat_turn(&turn_id, "tool timeout")
            .expect("fail turn");

        let unprocessed = db
            .get_unprocessed_operator_messages()
            .expect("query unprocessed messages");
        assert!(unprocessed.iter().any(|m| m.id == operator_message_id));

        let conversations = db.list_chat_conversations(50).expect("list conversations");
        let convo_state = conversations
            .iter()
            .find(|c| c.id == conversation.id)
            .expect("find conversation");
        assert_eq!(convo_state.runtime_state, ChatTurnPhase::Failed);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn scheduled_jobs_enqueue_messages_and_advance_timestamps() {
        let path = temp_db_path("scheduled_jobs_enqueue");
        let db = AgentDatabase::new(&path).expect("db init");
        let job = db
            .create_scheduled_job("Morning plan", "Review priorities.", 15)
            .expect("create scheduled job");

        let due_at = chrono::Utc::now() - ChronoDuration::minutes(3);
        {
            let conn = db.lock_conn().expect("lock conn");
            conn.execute(
                "UPDATE scheduled_jobs SET next_run_at = ?1 WHERE id = ?2",
                params![due_at.to_rfc3339(), &job.id],
            )
            .expect("set due timestamp");
        }

        let now = chrono::Utc::now();
        let queued = db
            .take_due_scheduled_jobs(now, 8)
            .expect("take due scheduled jobs");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, job.id);
        assert_eq!(queued[0].last_run_at, Some(now));
        assert!(queued[0].next_run_at > now);

        let history = db
            .get_chat_history_for_conversation(&job.conversation_id, 8)
            .expect("load scheduled conversation history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "operator");
        assert!(!history[0].processed);
        assert!(history[0]
            .content
            .contains("Scheduled job \"Morning plan\":\nReview priorities."));

        let persisted = db
            .get_scheduled_job(&job.id)
            .expect("load scheduled job")
            .expect("scheduled job exists");
        assert_eq!(persisted.last_run_at, Some(now));
        assert!(persisted.next_run_at > now);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn next_scheduled_job_due_at_ignores_disabled_jobs() {
        let path = temp_db_path("scheduled_jobs_next_due");
        let db = AgentDatabase::new(&path).expect("db init");

        let job_a = db
            .create_scheduled_job("A", "Task A", 60)
            .expect("create job a");
        let job_b = db
            .create_scheduled_job("B", "Task B", 60)
            .expect("create job b");

        let later = chrono::Utc::now() + ChronoDuration::minutes(20);
        let sooner = chrono::Utc::now() + ChronoDuration::minutes(5);
        {
            let conn = db.lock_conn().expect("lock conn");
            conn.execute(
                "UPDATE scheduled_jobs SET next_run_at = ?1 WHERE id = ?2",
                params![later.to_rfc3339(), &job_a.id],
            )
            .expect("set job_a next_run");
            conn.execute(
                "UPDATE scheduled_jobs SET next_run_at = ?1 WHERE id = ?2",
                params![sooner.to_rfc3339(), &job_b.id],
            )
            .expect("set job_b next_run");
        }

        db.update_scheduled_job(&job_b.id, None, None, None, Some(false))
            .expect("disable job_b");
        let next_due = db
            .next_scheduled_job_due_at()
            .expect("query next due")
            .expect("job exists");
        assert_eq!(next_due, later);

        db.update_scheduled_job(&job_b.id, None, None, None, Some(true))
            .expect("enable job_b");
        let next_due = db
            .next_scheduled_job_due_at()
            .expect("query next due")
            .expect("job exists");
        assert_eq!(next_due, sooner);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn memory_search_returns_relevant_entries() {
        let path = temp_db_path("memory_search");
        let db = AgentDatabase::new(&path).expect("db init");

        db.set_working_memory("project-goal", "build desktop companion agent")
            .expect("seed memory");
        db.set_working_memory("music", "buy new synth")
            .expect("seed memory");

        let results = db
            .search_working_memory("desktop agent", 5)
            .expect("search memory");
        assert!(!results.is_empty());
        assert_eq!(results[0].key, "project-goal");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_daily_activity_log_accumulates_lines() {
        let path = temp_db_path("daily_activity_log");
        let db = AgentDatabase::new(&path).expect("db init");

        db.append_daily_activity_log("Ran memory search tool")
            .expect("append first");
        db.append_daily_activity_log("Answered operator request")
            .expect("append second");

        let today_key = format!("activity-log-{}", chrono::Utc::now().format("%Y-%m-%d"));
        let item = db
            .get_working_memory(&today_key)
            .expect("get memory")
            .expect("daily log exists");
        assert!(item.content.contains("Ran memory search tool"));
        assert!(item.content.contains("Answered operator request"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn conversation_scoped_working_memory_filters_activity_lines() {
        let path = temp_db_path("conversation_working_memory");
        let db = AgentDatabase::new(&path).expect("db init");

        let conversation_id = "73a4b73d-8589-4763-9e8f-a0a237225f8d";
        let conversation_tag = helpers::short_conversation_tag(conversation_id);
        let other_tag = helpers::short_conversation_tag("0fc67f06-da6d-42da-90fd-619ef8e9b6b2");

        db.set_working_memory(
            "activity-log-2026-02-19",
            &format!(
                "Daily activity log for 2026-02-19\n\n- [04:00:00 UTC] operator [{}]: first task\n- [04:01:00 UTC] agent [{}] turn 1: decision=yield\n- [04:02:00 UTC] operator [{}]: unrelated task\n- [04:03:00 UTC] self-directive: tools=0 summary=\n",
                conversation_tag, conversation_tag, other_tag
            ),
        )
        .expect("seed activity log");
        db.set_working_memory(
            "project-brief",
            "Keep autonomous turns bounded to useful work.",
        )
        .expect("seed stable memory");

        let scoped = db
            .get_working_memory_context_for_conversation(conversation_id, 4000)
            .expect("scoped context");

        assert!(scoped.contains(&conversation_tag));
        assert!(!scoped.contains(&other_tag));
        assert!(scoped.contains("self-directive"));
        assert!(scoped.contains("project-brief"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn chat_context_strips_raw_metadata_blocks() {
        let path = temp_db_path("chat_context_sanitization");
        let db = AgentDatabase::new(&path).expect("db init");
        let conversation = db
            .create_chat_conversation(Some("Context sanitize"))
            .expect("create conversation");

        db.add_chat_message_in_conversation(&conversation.id, "operator", "Please run tools")
            .expect("insert operator message");
        db.add_chat_message_in_conversation(
            &conversation.id,
            "agent",
            "Done.\n\n[tool_calls]\n[{\"tool_name\":\"list_directory\",\"output_kind\":\"text\",\"output_preview\":\"file-a\\nfile-b\"}]\n[/tool_calls]\n\n[thinking]\n[\"private note\"]\n[/thinking]\n\n[turn_control]\n{\"decision\":\"yield\",\"status\":\"done\"}\n[/turn_control]",
        )
        .expect("insert agent message");

        let context = db
            .get_chat_context_for_conversation(&conversation.id, 10)
            .expect("chat context");

        assert!(context.contains("Done."));
        assert!(context.contains("tools=1"));
        assert!(context.contains("thinking=1 hidden"));
        assert!(context.contains("turn=yield/done"));
        assert!(!context.contains("[tool_calls]"));
        assert!(!context.contains("file-a\\nfile-b"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn chat_conversation_summary_roundtrip_and_history_slice() {
        let path = temp_db_path("chat_summary_roundtrip");
        let db = AgentDatabase::new(&path).expect("db init");

        let conversation = db
            .create_chat_conversation(Some("Summary test"))
            .expect("create conversation");

        for idx in 0..8 {
            let role = if idx % 2 == 0 { "operator" } else { "agent" };
            db.add_chat_message_in_conversation(
                &conversation.id,
                role,
                &format!("message {}", idx),
            )
            .expect("insert chat message");
        }

        let total = db
            .count_chat_messages_for_conversation(&conversation.id)
            .expect("count conversation messages");
        assert_eq!(total, 8);

        let slice = db
            .get_chat_history_slice_for_conversation(&conversation.id, 2, 3)
            .expect("slice chat messages");
        assert_eq!(slice.len(), 3);
        assert!(slice[0].content.contains("message 3"));
        assert!(slice[2].content.contains("message 5"));

        db.upsert_chat_conversation_summary(&conversation.id, "Older context summary", 5)
            .expect("upsert summary");
        let summary = db
            .get_chat_conversation_summary(&conversation.id)
            .expect("get summary")
            .expect("summary exists");
        assert_eq!(summary.summarized_message_count, 5);
        assert!(summary.summary_text.contains("Older context summary"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn journal_entry_roundtrip_search_and_context() {
        let path = temp_db_path("journal_roundtrip");
        let db = AgentDatabase::new(&path).expect("db init");

        let entry = JournalEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            entry_type: JournalEntryType::Reflection,
            content: "I noticed Max is iterating faster on calibration today.".to_string(),
            context: JournalContext {
                trigger: "ambient_orientation".to_string(),
                user_state_at_time: "deep_work".to_string(),
                time_of_day: "afternoon".to_string(),
            },
            related_concerns: vec!["thermal-array".to_string()],
            mood_at_time: Some(JournalMood {
                valence: 0.3,
                arousal: 0.6,
            }),
        };

        db.add_journal_entry(&entry).expect("save journal entry");

        let recent = db.get_recent_journal(5).expect("get recent journal");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, entry.id);
        assert_eq!(recent[0].entry_type, JournalEntryType::Reflection);
        assert_eq!(
            recent[0].related_concerns,
            vec!["thermal-array".to_string()]
        );

        let found = db.search_journal("calibration", 5).expect("search journal");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, entry.id);

        let context = db
            .get_journal_for_context(64)
            .expect("journal context string");
        assert!(context.contains("Recent Journal Notes"));
        assert!(context.contains("calibration"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn concerns_roundtrip_touch_and_salience_update() {
        let path = temp_db_path("concerns_roundtrip");
        let db = AgentDatabase::new(&path).expect("db init");

        let now = chrono::Utc::now();
        let concern = Concern {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: now,
            last_touched: now,
            summary: "Thermal array calibration workflow".to_string(),
            concern_type: ConcernType::CollaborativeProject {
                project_name: "thermal-array".to_string(),
                my_role: "observer".to_string(),
            },
            salience: Salience::Active,
            my_thoughts: "Track iteration speed and toolchain friction.".to_string(),
            related_memory_keys: vec!["activity-log".to_string()],
            context: ConcernContext {
                how_it_started: "operator conversation".to_string(),
                key_events: vec!["initial planning".to_string()],
                last_update_reason: "created".to_string(),
            },
        };

        db.save_concern(&concern).expect("save concern");

        let loaded = db
            .get_concern(&concern.id)
            .expect("get concern")
            .expect("concern exists");
        assert_eq!(loaded.summary, concern.summary);
        assert_eq!(loaded.salience, Salience::Active);

        let active = db.get_active_concerns().expect("active concerns");
        assert!(active.iter().any(|c| c.id == concern.id));

        db.touch_concern(&concern.id, "checked during ll.1 test")
            .expect("touch concern");
        let touched = db
            .get_concern(&concern.id)
            .expect("get touched concern")
            .expect("touched concern exists");
        assert_eq!(
            touched.context.last_update_reason,
            "checked during ll.1 test"
        );

        db.update_concern_salience(&concern.id, Salience::Dormant)
            .expect("demote concern");
        let active_after = db.get_active_concerns().expect("active after demote");
        assert!(!active_after.iter().any(|c| c.id == concern.id));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn orientation_snapshot_and_pending_thought_queue_roundtrip() {
        let path = temp_db_path("orientation_pending");
        let db = AgentDatabase::new(&path).expect("db init");

        let snapshot = OrientationSnapshotRecord {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            user_state: serde_json::json!({"type":"deep_work","activity":"coding"}),
            disposition: "observe".to_string(),
            synthesis: "Max is focused and stable.".to_string(),
            salience_map: serde_json::json!([{"summary":"Code task","relevance":0.8}]),
            anomalies: serde_json::json!([]),
            pending_thoughts: serde_json::json!([{"content":"Mention GPU temp drift"}]),
            mood_valence: Some(0.2),
            mood_arousal: Some(0.5),
        };
        db.save_orientation_snapshot(&snapshot)
            .expect("save orientation snapshot");

        let recent = db.get_recent_orientations(3).expect("recent orientations");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, snapshot.id);
        assert_eq!(recent[0].disposition, "observe");

        let thought_a = PendingThoughtRecord {
            id: uuid::Uuid::new_v4().to_string(),
            content: "Ask whether to run a thermal check.".to_string(),
            context: Some("orientation.surface".to_string()),
            priority: 0.9,
            relates_to: vec!["thermal-array".to_string()],
            created_at: chrono::Utc::now(),
            surfaced_at: None,
            dismissed_at: None,
        };
        let thought_b = PendingThoughtRecord {
            id: uuid::Uuid::new_v4().to_string(),
            content: "Share calibration trend if asked.".to_string(),
            context: Some("orientation.surface".to_string()),
            priority: 0.4,
            relates_to: vec![],
            created_at: chrono::Utc::now(),
            surfaced_at: None,
            dismissed_at: None,
        };
        db.queue_pending_thought(&thought_a)
            .expect("queue thought a");
        db.queue_pending_thought(&thought_b)
            .expect("queue thought b");

        let unsurfaced = db
            .get_unsurfaced_thoughts()
            .expect("get unsurfaced thoughts");
        assert_eq!(unsurfaced.len(), 2);
        assert_eq!(unsurfaced[0].id, thought_a.id);

        db.mark_thought_surfaced(&thought_a.id)
            .expect("mark surfaced");
        db.dismiss_thought(&thought_b.id).expect("dismiss thought");
        let remaining = db
            .get_unsurfaced_thoughts()
            .expect("remaining unsurfaced thoughts");
        assert!(remaining.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ooda_turn_packet_roundtrip_and_action_digest() {
        let path = temp_db_path("ooda_packet");
        let db = AgentDatabase::new(&path).expect("db init");
        let conversation = db
            .create_chat_conversation(Some("OODA packet test"))
            .expect("create conversation");

        let turn_id = db
            .begin_chat_turn(&conversation.id, &[], 1)
            .expect("begin turn");
        db.record_chat_turn_tool_call(
            &turn_id,
            0,
            "list_directory",
            "{\"path\":\".\"}",
            "Cargo.toml",
        )
        .expect("record tool call");
        let agent_message_id = db
            .add_chat_message_in_turn(
                &conversation.id,
                &turn_id,
                "agent",
                "Listed files.\n\n[tool_calls]\n[{\"tool_name\":\"list_directory\",\"output_kind\":\"text\",\"output_preview\":\"Cargo.toml\"}]\n[/tool_calls]",
            )
            .expect("insert agent message");
        db.complete_chat_turn(
            &turn_id,
            ChatTurnPhase::Completed,
            "yield",
            "done",
            "Completed listing files",
            Some("completed"),
            1,
            Some(&agent_message_id),
        )
        .expect("complete turn");

        let packet = OodaTurnPacketRecord {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation.id.clone(),
            turn_id: Some(turn_id.clone()),
            observe: "Operator requested directory listing.".to_string(),
            orient: "User likely validating workspace visibility.".to_string(),
            decide: "Use list_directory then yield.".to_string(),
            act: "Ran list_directory and summarized output.".to_string(),
            created_at: chrono::Utc::now(),
        };
        db.save_ooda_turn_packet(&packet).expect("save ooda packet");

        let loaded = db
            .get_latest_ooda_turn_packet_for_conversation(&conversation.id)
            .expect("load conversation packet")
            .expect("packet exists");
        assert_eq!(loaded.id, packet.id);
        assert_eq!(loaded.turn_id, packet.turn_id);

        let global = db
            .get_latest_ooda_turn_packet()
            .expect("load latest packet")
            .expect("latest packet exists");
        assert_eq!(global.id, packet.id);

        let older_packet = OodaTurnPacketRecord {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation.id.clone(),
            turn_id: None,
            observe: "Older observe".to_string(),
            orient: "Older orient".to_string(),
            decide: "Older decide".to_string(),
            act: "Older act".to_string(),
            created_at: packet.created_at - chrono::Duration::minutes(5),
        };
        db.save_ooda_turn_packet(&older_packet)
            .expect("save older packet");

        let packets_before_latest = db
            .get_recent_ooda_turn_packets_for_conversation_before(
                &conversation.id,
                &packet.created_at,
                8,
            )
            .expect("query packet window");
        assert_eq!(packets_before_latest.len(), 2);
        assert_eq!(packets_before_latest[0].id, older_packet.id);
        assert_eq!(packets_before_latest[1].id, packet.id);

        let digest = db
            .get_recent_action_digest_for_conversation(&conversation.id, 8, 1000)
            .expect("digest");
        assert!(digest.contains("phase=completed"));
        assert!(digest.contains("decision=yield"));
        assert!(digest.contains("list_directory"));
        assert!(digest.contains("reply=\"Listed files."));

        let _ = std::fs::remove_file(&path);
    }
}
