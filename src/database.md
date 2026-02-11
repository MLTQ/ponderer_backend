# database.rs

## Purpose
Provides the agent's persistent memory layer via SQLite. Stores important posts, reflection history, persona snapshots (for personality evolution tracking), private chat messages, chat conversation threads, and key-value state. Working-memory CRUD now routes through a versioned `MemoryBackend` abstraction while preserving the existing KV behavior (`kv_v1`).

## Components

### `AgentDatabase`
- **Does**: Wraps a `Mutex<rusqlite::Connection>`, delegates working-memory CRUD to `MemoryBackend`, and provides typed CRUD methods for all tables
- **Interacts with**: `main.rs` (instantiated), `agent::Agent` (reads/writes memories), `ui::app::AgentApp` (displays data), `memory::KvMemoryBackend` (default backend), `memory::MemoryMigrationRegistry` (upgrade scaffolding)

### `ImportantPost`
- **Does**: Records a forum post the agent found significant, with `importance_score` (0.0-1.0) and `why_important` explanation
- **Interacts with**: `agent::reasoning` (marks posts), UI persona/memory views

### `PersonaSnapshot`
- **Does**: Captures the agent's personality state at a point in time, including dynamic `PersonaTraits` dimensions, system prompt, trigger, self-description, and inferred trajectory
- **Rationale**: Core data structure for "Ludonarrative Assonantic Tracing" -- tracking personality evolution over time

### `PersonaTraits`
- **Does**: Flexible `HashMap<String, f64>` mapping dimension names to 0.0-1.0 scores
- **Rationale**: Avoids fixed personality models; agents define their own axes via `guiding_principles` or LLM self-reflection

### `ReflectionRecord`
- **Does**: Logs each self-reflection event with old/new system prompts, reasoning, and guiding principles

### `WorkingMemoryEntry`
- **Does**: Key-value scratchpad entries the agent can read/write between sessions
- **Interacts with**: `memory::MemoryBackend` contract, `get_working_memory_context()` formats all entries for LLM context injection

### `get_memory_design_version` / `set_memory_design_version`
- **Does**: Persists and reads active memory design metadata from `agent_state` (`memory_design_id`, `memory_schema_version`)
- **Interacts with**: `memory::MemoryDesignVersion`, startup initialization in `AgentDatabase::new`

### `apply_memory_migration`
- **Does**: Applies a direct migration via `MemoryMigrationRegistry` and updates persisted memory design metadata
- **Interacts with**: Future scheduled upgrade flow (ALMA-lite memory evolution loop)

### Memory Archive Methods (`save_memory_design_archive_entry`, `save_memory_eval_run`, `save_memory_promotion_decision`, `evaluate_and_record_memory_promotion`)
- **Does**: Persists memory design versions, eval reports, and policy decisions into dedicated archive tables
- **Interacts with**: `memory::archive` policy evaluator + record types, `memory::eval::MemoryEvalReport`
- **Rationale**: Makes promotion outcomes auditable/reproducible and ensures rollback target is always persisted

### `ChatMessage`
- **Does**: Private operator-agent chat messages with `conversation_id` routing and a `processed` flag for the agent to track unread messages
- **Interacts with**: UI chat panel, `agent::Agent` poll loop

### `ChatConversation`
- **Does**: Conversation-level metadata (`id`, `title`, timestamps, message stats) used by the UI for multi-chat selection
- **Interacts with**: `ui::app` conversation picker, `chat_messages` table via `conversation_id`

### Chat conversation methods (`create_chat_conversation`, `list_chat_conversations`, `add_chat_message_in_conversation`, `get_chat_history_for_conversation`, `get_chat_context_for_conversation`)
- **Does**: Creates and lists conversation threads, writes messages to a specific thread, and returns thread-scoped history/context
- **Interacts with**: `ui::app::AgentApp` new-chat/switch-chat actions, `agent::process_chat_messages` per-conversation prompt building

### `CharacterCard` (DB model)
- **Does**: Stores imported character card metadata (format, raw data, derived prompt)
- **Rationale**: Only one card kept at a time (previous deleted on import)

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | `AgentDatabase::new(path)` creates/opens DB, ensures schema, and initializes memory design metadata | Changing startup initialization or metadata keys |
| `agent::Agent` | Methods: `get_recent_important_posts`, `save_important_post`, `get_current_system_prompt`, `set_current_system_prompt`, `get_unprocessed_operator_messages`, `mark_message_processed`, `save_persona_snapshot`, `get_working_memory_context`, `get_chat_context_for_conversation`, `add_chat_message_in_conversation` | Removing or renaming any public method |
| `ui::app` | `list_chat_conversations`, `create_chat_conversation`, `get_chat_history_for_conversation`, `add_chat_message_in_conversation`, plus legacy `get_chat_history` | Changing return types/signatures of chat query and write methods |
| `memory::mod` | `MemoryBackend` trait and `MemoryDesignVersion` metadata keys remain stable | Changing backend trait signatures or metadata semantics |
| `memory::archive` | Archive methods serialize/deserialize policy + metrics snapshots without loss | Changing JSON field contracts for policy/snapshot structs |

## Notes
- All timestamps stored as RFC 3339 strings in SQLite, parsed back to `chrono::DateTime<Utc>`.
- `ensure_schema()` uses `CREATE TABLE IF NOT EXISTS` -- no formal migration system. Adding columns requires manual ALTER TABLE handling.
- `ensure_schema()` now performs a manual chat migration by checking `PRAGMA table_info(chat_messages)` and adding `conversation_id` when missing.
- Memory design metadata is stored in `agent_state` under `memory_design_id` and `memory_schema_version`.
- Memory evolution archive uses three tables: `memory_design_archive`, `memory_eval_runs`, `memory_promotion_decisions`.
- `memory_promotion_decisions` enforces rollback fields (`rollback_design_id`, `rollback_schema_version`) as NOT NULL.
- `save_character_card` deletes all existing cards before inserting (singleton pattern).
- The `agent_state` table is a generic key-value store used for `current_system_prompt` and `last_reflection_time`.
- A default thread (`id = "default"`) is auto-created for backward compatibility with old data and legacy `add_chat_message`.
