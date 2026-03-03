# scheduled_jobs.rs

## Purpose
Defines the persisted scheduled-job shape shared by the database, agent loop, and REST API. It centralizes interval normalization so recurring jobs stay within a bounded, simple scheduling model.

## Components

### `ScheduledJob`
- **Does**: Represents one recurring operator-defined task with its prompt, interval, dedicated conversation, enabled flag, and run timestamps.
- **Interacts with**: `database.rs` persistence, `agent/mod.rs` due-job enqueueing, and `server.rs` CRUD endpoints.

### `ScheduledJob::normalized_interval_minutes`
- **Does**: Clamps user input into the supported interval window.
- **Interacts with**: Database create/update flows.

### `ScheduledJob::next_run_after`
- **Does**: Computes the next due timestamp from a base time and interval.
- **Interacts with**: Database scheduling updates after create/edit/run.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `database.rs` | Field names and interval helpers remain stable | Renaming fields or changing interval semantics |
| `server.rs` | `ScheduledJob` is serializable for REST responses | Removing serde derives or changing field types |

## Notes
- The current scheduler is intentionally interval-based, not cron-based. It is meant to be simple, predictable, and easy to evolve later.
