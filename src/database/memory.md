# database/memory.rs

## Purpose
Working memory CRUD, conversation-scoped memory context, memory design versioning, memory archive/eval/promotion methods.

## Components

### Memory design version methods
- `get_memory_design_version` / `set_memory_design_version` — read/write `memory_design_id` and `memory_schema_version` from `agent_state`
- `apply_memory_migration` — applies a direct migration via `MemoryMigrationRegistry`, updates persisted version

### Memory archive methods
- `save_memory_design_archive_entry` / `list_memory_design_archive_entries` — persist versioned memory design snapshots
- `save_memory_eval_run` / `get_memory_eval_run` — store raw eval report JSON
- `save_memory_promotion_decision` / `get_memory_promotion_decision` — persist promotion decisions with explicit rollback fields
- `evaluate_and_record_memory_promotion` — runs policy evaluation against stored eval run, persists decision
- `recompute_memory_promotion_decision` — verifies decision reproducibility from persisted artifacts

### Working memory methods
- `set_working_memory` / `get_working_memory` / `get_all_working_memory` / `delete_working_memory` — delegate to `MemoryBackend`
- `search_working_memory` — ranked text search over key/content with multi-term scoring
- `append_daily_activity_log` — accumulates timestamped lines into date-keyed activity log entries
- `get_working_memory_context` — formats all entries as `## Your Working Memory` section
- `get_working_memory_context_for_conversation` — conversation-scoped variant: filters activity log lines to the conversation's tag, truncates to `max_chars`

## Contracts
| Dependent | Expects |
|-----------|---------|
| `memory::MemoryBackend` | `set_entry`, `get_entry`, `list_entries`, `delete_entry` trait methods |
| `memory::archive` | Archive record types and `evaluate_promotion_policy` function |
| `agent::Agent` | Working memory read/write/search, daily activity log |
| `agent::process_chat_messages` | Conversation-scoped context injection |

## Notes
- `promotion_decisions` enforces `rollback_design_id` / `rollback_schema_version` as NOT NULL to ensure every decision includes an explicit fallback target
- Memory design metadata is stored in `agent_state` under `MEMORY_DESIGN_STATE_KEY` and `MEMORY_SCHEMA_VERSION_STATE_KEY`
- Activity log filtering uses `short_conversation_tag` (first 12 chars of conversation ID) for compact line tagging
