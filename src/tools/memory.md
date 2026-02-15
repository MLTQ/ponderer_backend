# memory.rs

## Purpose
Adds memory-management tools the agent can call directly during the tool loop: searching persisted working memory and writing notes. This gives private chat turns explicit long-term recall/update abilities.

## Components

### `MemorySearchTool`
- **Does**: Implements `search_memory`, ranking and returning matching working-memory entries for a query with optional limit.
- **Interacts with**: `AgentDatabase::search_working_memory`, tool loop in `agentic.rs`

### `MemoryWriteTool`
- **Does**: Implements `write_memory`, supporting `replace` or `append` modes for key-based notes.
- **Interacts with**: `AgentDatabase::get_working_memory`, `AgentDatabase::set_working_memory`, `AgentDatabase::append_daily_activity_log`

### `open_database()` (private)
- **Does**: Loads runtime config and opens the configured SQLite memory DB path for tool operations.
- **Interacts with**: `AgentConfig::load`, `AgentDatabase::new`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | Tools are constructible via `MemorySearchTool::new` and `MemoryWriteTool::new` | Renaming tool structs or constructors |
| LLM tool-calling | Tool names and parameter schemas remain stable (`search_memory`, `write_memory`) | Renaming tools or changing required params |
| `database.rs` | Search/write APIs behave synchronously and return durable state | Changing DB API names or return semantics |

## Notes
- `search_memory` clamps result count to 1-50 and requires a non-empty query.
- `write_memory` appends a daily activity-log line on successful writes for longitudinal traceability.
