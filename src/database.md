# database/ (module)

## Purpose
Provides the agent's persistent memory layer via SQLite. The `database` module is split into focused submodules, each handling a distinct domain. All submodules implement methods on the shared `AgentDatabase` struct defined in `mod.rs`.

## Module structure

```
src/database/
  mod.rs            - AgentDatabase struct, Connection handling, ensure_schema, migrations, schema helpers, get_state/set_state, tests
  chat.rs           - ChatSession, ChatConversation, ChatConversationSummary, ChatMessage, ChatTurn, ChatTurnToolCall, ChatTurnPhase, OodaTurnPacketRecord, all chat/OODA methods
  concerns.rs       - Concern methods (save_concern, get_concern, get_active_concerns, update_concern_salience, touch_concern, etc.)
  helpers.rs        - Private helper functions (short_conversation_tag, filter_activity_log_for_conversation, summarize_chat_message_for_context, extract_tagged_blocks, summarize_*_blocks, compact_whitespace, truncate_for_db_digest, outcome_to_db)
  journal.rs        - Journal methods (add_journal_entry, get_recent_journal, get_journal_for_context, search_journal)
  memory.rs         - Working memory CRUD, memory design version, archive/eval/promotion methods
  orientation.rs    - OrientationSnapshotRecord, PendingThoughtRecord, orientation snapshot and pending thought methods
  persona.rs        - PersonaSnapshot, PersonaTraits, CharacterCard, ReflectionRecord, all persona/character/reflection methods
  posts.rs          - ImportantPost and all important post methods
  scheduled_jobs.rs - ScheduledJob methods (create, list, get, update, delete, next_due_at, take_due)
```

Each `.rs` file has a companion `.md` with detailed component documentation.

## Components

### `AgentDatabase` (mod.rs)
- **Does**: Wraps a `Mutex<rusqlite::Connection>`, delegates working-memory CRUD to `MemoryBackend`, and provides typed CRUD methods for all tables via the submodule `impl` blocks
- **Interacts with**: `main.rs` (instantiated), `agent::Agent` (reads/writes memories), `ui::app::AgentApp` (displays data), `memory::KvMemoryBackend` (default backend), `memory::MemoryMigrationRegistry` (upgrade scaffolding)

### Schema management (mod.rs)
- `ensure_schema` — creates all tables and indexes using `CREATE TABLE IF NOT EXISTS`; applies manual column migrations via `PRAGMA table_info` checks
- `get_state` / `set_state` — generic key-value store backed by `agent_state` table

See each submodule's `.md` file for detailed component documentation.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | `AgentDatabase::new(path)` creates/opens DB, ensures schema, and initializes memory design metadata | Changing startup initialization or metadata keys |
| `agent::Agent` | Chat APIs include turn lifecycle methods plus conversation-scoped message/context methods (`begin_chat_turn`, `record_chat_turn_tool_call`, `complete_chat_turn`, `fail_chat_turn`, `add_chat_message_in_turn`) | Removing/renaming lifecycle methods or changing state semantics |
| `ui::app` | `ChatConversation` includes `runtime_state`; conversation APIs remain (`list_chat_conversations`, `create_chat_conversation`, `get_chat_history_for_conversation`, `add_chat_message_in_conversation`) | Removing runtime state fields or changing chat query/write signatures |
| `server.rs` | Conversation fetch + diagnostics APIs (`get_chat_conversation`, `get_chat_conversation_summary`, `list_chat_turns_for_conversation`, `list_chat_turn_tool_calls`) remain available for REST routes | Renaming/removing these query methods or changing return semantics |
| `memory::mod` | `MemoryBackend` trait and `MemoryDesignVersion` metadata keys remain stable | Changing backend trait signatures or metadata semantics |
| `memory::archive` | Archive methods serialize/deserialize policy + metrics snapshots without loss | Changing JSON field contracts for policy/snapshot structs |
| `agent::{journal, concerns}` | DB CRUD accepts/returns these typed records | Breaking type-field compatibility or db string mappings |

## Notes
- All timestamps stored as RFC 3339 strings in SQLite, parsed back to `chrono::DateTime<Utc>`.
- `ensure_schema()` uses `CREATE TABLE IF NOT EXISTS` -- no formal migration system. Adding columns requires manual ALTER TABLE handling.
- `ensure_schema()` performs manual chat migrations by checking `PRAGMA table_info(...)` and adding missing columns (`conversation_id`, `turn_id`, `session_id`, `runtime_state`, `active_turn_id`) in place.
- `ensure_schema()` also adds `chat_turns.prompt_text` and `chat_turns.system_prompt_text` in place for existing DBs so turn-level prompt inspection is backward-compatible.
- Conversation compaction snapshots are stored in `chat_conversation_summaries` and updated opportunistically by the agent loop when message-count thresholds are exceeded.
- Scheduled jobs live in their own additive `scheduled_jobs` table and create a dedicated chat conversation on insert so recurring runs retain thread-local history.
- Living Loop ll.1 adds four additive tables: `journal_entries`, `concerns`, `orientation_snapshots`, `pending_thoughts_queue`.
- OODA continuity adds additive table `ooda_turn_packets` plus supporting indexes on `(conversation_id, created_at)` and `(turn_id)`.
- Conversation-scoped working-memory context keeps stable notes while filtering noisy cross-conversation activity lines by conversation tag.
- Chat-context rendering now compacts metadata envelopes into concise tags so prompt windows avoid large embedded tool outputs.
- Memory design metadata is stored in `agent_state` under `memory_design_id` and `memory_schema_version`.
- Memory evolution archive uses three tables: `memory_design_archive`, `memory_eval_runs`, `memory_promotion_decisions`.
- `memory_promotion_decisions` enforces rollback fields (`rollback_design_id`, `rollback_schema_version`) as NOT NULL.
- `save_character_card` deletes all existing cards before inserting (singleton pattern).
- The `agent_state` table is a generic key-value store used for `current_system_prompt` and `last_reflection_time`.
- Default chat bootstrap rows are auto-created for both session (`default_session`) and conversation (`default`) compatibility.
