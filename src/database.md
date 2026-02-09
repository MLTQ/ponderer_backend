# database.rs

## Purpose
Provides the agent's persistent memory layer via SQLite. Stores important posts, reflection history, persona snapshots (for personality evolution tracking), working memory (scratchpad), private chat messages, and key-value state. Designed for concurrent access via WAL mode.

## Components

### `AgentDatabase`
- **Does**: Wraps a `Mutex<rusqlite::Connection>` and provides typed CRUD methods for all tables
- **Interacts with**: `main.rs` (instantiated), `agent::Agent` (reads/writes memories), `ui::app::AgentApp` (displays data)

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
- **Interacts with**: `get_working_memory_context()` formats all entries for LLM context injection

### `ChatMessage`
- **Does**: Private operator-agent chat messages with `processed` flag for the agent to track unread messages
- **Interacts with**: UI chat panel, `agent::Agent` poll loop

### `CharacterCard` (DB model)
- **Does**: Stores imported character card metadata (format, raw data, derived prompt)
- **Rationale**: Only one card kept at a time (previous deleted on import)

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | `AgentDatabase::new(path)` creates/opens DB and runs migrations | Changing schema without migration logic |
| `agent::Agent` | Methods: `get_recent_important_posts`, `save_important_post`, `get_current_system_prompt`, `set_current_system_prompt`, `get_unprocessed_operator_messages`, `mark_message_processed`, `save_persona_snapshot`, `get_working_memory_context` | Removing or renaming any public method |
| `ui::app` | `get_chat_history`, `get_persona_history`, `get_reflection_history`, `get_all_working_memory` | Changing return types of query methods |

## Notes
- All timestamps stored as RFC 3339 strings in SQLite, parsed back to `chrono::DateTime<Utc>`.
- `ensure_schema()` uses `CREATE TABLE IF NOT EXISTS` -- no formal migration system. Adding columns requires manual ALTER TABLE handling.
- `save_character_card` deletes all existing cards before inserting (singleton pattern).
- The `agent_state` table is a generic key-value store used for `current_system_prompt` and `last_reflection_time`.
