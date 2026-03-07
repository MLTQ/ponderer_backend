# memory.rs

## Purpose
Adds memory-management tools the agent can call during the tool loop: searching persisted working memory, writing notes, a cross-session handoff note, private-chat mode control, and a task-scoped scratchpad. Gives private chat turns explicit long-term recall/update abilities with distinct tools for different time horizons.

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

### `PrivateChatModeTool`
- **Does**: Implements `private_chat_mode` with `get`, `set`, and `toggle` actions for runtime private-chat mode (`agentic` vs `direct`), updating both in-memory DB state and persisted config.
- **Interacts with**: `AgentDatabase::get_state/set_state` using `PRIVATE_CHAT_MODE_STATE_KEY`, `AgentConfig::load/save`, `agent/mod.rs` mode selection.

### `PRIVATE_CHAT_MODE_STATE_KEY` (pub const)
- **Does**: The fixed state key (`"private-chat-mode"`) used for live mode switching without requiring backend restart.
- **Interacts with**: `PrivateChatModeTool`, `Agent::private_chat_execution_mode`.

### `ScratchNoteTool`
- **Does**: Implements `scratch_note`, a task-scoped ephemeral scratchpad stored under the `scratchpad` key in working memory. Supports modes: `replace`, `append`, `clear`, `read`. Appears naturally in working memory context; agent is expected to clear it when a task completes.
- **Interacts with**: `AgentDatabase::get_working_memory`, `AgentDatabase::set_working_memory`

### `SCRATCHPAD_KEY` (pub const)
- **Does**: The fixed working-memory key (`"scratchpad"`) for the active task scratchpad.
- **Interacts with**: `ScratchNoteTool`

### `open_database()` (private)
- **Does**: Loads runtime config and opens the configured SQLite memory DB path for tool operations.
- **Interacts with**: `AgentConfig::load`, `AgentDatabase::new`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime.rs` | Tools constructible via `MemorySearchTool::new`, `MemoryWriteTool::new`, `WriteSessionHandoffTool::new`, `PrivateChatModeTool::new`, `ScratchNoteTool::new` | Renaming tool structs or constructors |
| LLM tool-calling | Tool names and parameter schemas remain stable (`search_memory`, `write_memory`, `write_session_handoff`, `private_chat_mode`, `scratch_note`) | Renaming tools or changing required params |
| `database.rs` | Search/write APIs behave synchronously and return durable state | Changing DB API names or return semantics |
| `agent/mod.rs` | `SESSION_HANDOFF_KEY` constant remains stable; `get_working_memory(SESSION_HANDOFF_KEY)` returns the handoff note | Renaming key or changing storage format |

## Notes
- `search_memory` clamps result count to 1-50 and requires a non-empty query.
- `write_memory` appends a daily activity-log line on successful writes for longitudinal traceability.
- `write_session_handoff` always overwrites the previous note — one clean note per wrap-up, not append.
- `private_chat_mode` updates the runtime mode immediately through DB state and attempts to persist the same mode in TOML for restart continuity.
- The handoff note is injected before all other context sections in `build_private_chat_agentic_prompt` so it's the first thing the agent reads on resumption.
- `scratch_note` mode=clear sets the key to an empty string (filtered from context by the empty-content check in `get_working_memory_context_for_conversation`).
- Distinct tool time horizons: scratchpad = current task; working memory = cross-task notes; handoff note = cross-session continuity.
