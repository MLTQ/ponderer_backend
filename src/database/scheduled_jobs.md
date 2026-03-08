# database/scheduled_jobs.rs

## Purpose
Persistent scheduled job CRUD and atomic due-job dequeuing with chat message injection.

## Components

### `parse_scheduled_job_row` (private helper)
- **Does**: Maps a rusqlite row to a `ScheduledJob`, parsing timestamp strings and boolean-as-integer fields
- **Interacts with**: `list_scheduled_jobs`, `get_scheduled_job`, `take_due_scheduled_jobs`

### Scheduled job methods on `AgentDatabase`
- `create_scheduled_job` — creates a new job with a dedicated conversation (title `"Schedule: {name}"`), sets `next_run_at` via `ScheduledJob::next_run_after`
- `list_scheduled_jobs` — lists jobs ordered by enabled desc, next_run_at asc
- `get_scheduled_job` — fetches a single job by ID
- `update_scheduled_job` — patches name/prompt/interval/enabled; re-advances `next_run_at` if interval changed or job re-enabled while overdue; updates conversation title to match new name
- `delete_scheduled_job` — removes the job row (conversation is preserved)
- `next_scheduled_job_due_at` — returns the earliest `next_run_at` among enabled jobs; used for sleep-cap in the agent loop
- `take_due_scheduled_jobs` — atomically selects due jobs (within a transaction), injects an operator-style message into each job's conversation, advances `last_run_at` / `next_run_at`, and commits

## Contracts
| Dependent | Expects |
|-----------|---------|
| `scheduled_jobs.rs` | `ScheduledJob` type with `queue_message()`, `next_run_after()`, `normalized_interval_minutes()` |
| `server.rs` | Scheduled job CRUD routes |
| `agent::maybe_enqueue_due_scheduled_jobs` | `take_due_scheduled_jobs` atomicity |

## Notes
- `take_due_scheduled_jobs` uses an explicit `conn.transaction()` to ensure all message inserts and timestamp updates are atomic
- Each job gets its own dedicated conversation at creation time; the conversation is preserved even if the job is deleted
- Job messages are inserted with role `"scheduled"` and picked up by the agent's unprocessed message poll (which queries `role IN ('operator', 'scheduled')`)
