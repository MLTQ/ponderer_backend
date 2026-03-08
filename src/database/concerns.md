# database/concerns.rs

## Purpose
Living Loop foundation: persistent concern CRUD -- the agent's ongoing worries, projects, and tracked situations.

## Components

### Concern methods on `AgentDatabase`
- `save_concern` — inserts or replaces a `Concern` record, serializing `concern_type`, `related_memory_keys`, and `context` as JSON
- `get_concern` — fetches a single concern by ID, deserializing all JSON fields
- `get_active_concerns` — returns concerns with salience `Active` or `Monitoring`, ordered by `last_touched` desc
- `get_all_concerns` — returns all concerns ordered by `last_touched` desc
- `update_concern_salience` — updates the salience column and `updated_at` timestamp
- `touch_concern` — loads a concern, updates `last_touched` and `context.last_update_reason`, re-saves

## Contracts
| Dependent | Expects |
|-----------|---------|
| `agent::concerns` | `Concern`, `ConcernContext`, `ConcernType`, `Salience` types accepted and returned |
| Living Loop ambient loop | `save_concern`, `touch_concern`, `get_active_concerns` for concern lifecycle |

## Notes
- `concern_type` stored as JSON (serde-serialized enum) to preserve variant structure
- `related_memory_keys` stored as JSON array of strings
- `context` stored as JSON-serialized `ConcernContext` struct; falls back to `Default` if null in DB
- `salience` stored as string via `Salience::as_db_str()` / `Salience::from_db()`
- `get_active_concerns` and `get_all_concerns` use a two-phase approach: first collect IDs (releasing lock), then fetch each concern individually to avoid nested lock conflicts
