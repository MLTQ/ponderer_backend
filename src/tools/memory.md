# memory.rs

## Purpose
Adds memory-management tools the agent can call directly during the tool loop: searching persisted working memory, writing notes, and writing cross-session handoff notes. This gives private chat turns explicit long-term recall/update abilities.

## Components

### `MemorySearchTool`
- **Does**: Implements `search_memory`, ranking and returning matching working-memory entries for a query with optional limit.
- **Interacts with**: `AgentDatabase::search_working_memory`, tool loop in `agentic.rs`

### `MemoryWriteTool`
- **Does**: Implements `write_memory`, supporting `replace` or `append` modes for key-based notes.
- **Interacts with**: `AgentDatabase::get_working_memory`, `AgentDatabase::set_working_memory`, `AgentDatabase::append_daily_activity_log`

### `WriteSessionHandoffTool`
- **Does**: Implements `write_session_handoff`, writing a cross-session continuity note to the fixed key `session-handoff` in working memory. The note is injected at the very top of the next session's prompt context via `build_private_chat_agentic_prompt`.
- **Interacts with**: `AgentDatabase::set_working_memory` using `SESSION_HANDOFF_KEY`; consumed by `agent/mod.rs` prompt assembly

### `SESSION_HANDOFF_KEY` (pub const)
- **Does**: The fixed working-memory key (`"session-handoff"`) used by both the write tool and the prompt assembler.
- **Interacts with**: `WriteSessionHandoffTool`, `build_private_chat_agentic_prompt` in `agent/mod.rs`

### `open_database()` (private)
- **Does**: Loads runtime config and opens the configured SQLite memory DB path for tool operations.
- **Interacts with**: `AgentConfig::load`, `AgentDatabase::new`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime.rs` | Tools are constructible via `MemorySearchTool::new`, `MemoryWriteTool::new`, `WriteSessionHandoffTool::new` | Renaming tool structs or constructors |
| LLM tool-calling | Tool names and parameter schemas remain stable (`search_memory`, `write_memory`, `write_session_handoff`) | Renaming tools or changing required params |
| `database.rs` | Search/write APIs behave synchronously and return durable state | Changing DB API names or return semantics |
| `agent/mod.rs` | `SESSION_HANDOFF_KEY` constant remains stable; `get_working_memory(SESSION_HANDOFF_KEY)` returns the handoff note | Renaming key or changing storage format |

## Notes
- `search_memory` clamps result count to 1-50 and requires a non-empty query.
- `write_memory` appends a daily activity-log line on successful writes for longitudinal traceability.
- `write_session_handoff` always overwrites the previous note — one clean note per wrap-up, not append.
- The handoff note is injected before all other context sections in `build_private_chat_agentic_prompt` so it's the first thing the agent reads on resumption.
