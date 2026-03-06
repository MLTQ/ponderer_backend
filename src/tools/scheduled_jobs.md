# scheduled_jobs.rs

## Purpose
Adds agent-callable tools for managing recurring scheduled jobs in the local SQLite database. These tools make schedule creation, editing, listing, and deletion available directly inside the autonomous tool loop.

## Components

### `ListScheduledJobsTool`
- **Does**: Implements `list_scheduled_jobs` and returns current schedule entries with timing + enabled state.
- **Interacts with**: `AgentDatabase::list_scheduled_jobs`

### `CreateScheduledJobTool`
- **Does**: Implements `create_scheduled_job` with required `name` + `prompt`, optional interval and enabled flag.
- **Interacts with**: `AgentDatabase::create_scheduled_job`, `AgentDatabase::update_scheduled_job`

### `UpdateScheduledJobTool`
- **Does**: Implements `update_scheduled_job` for partial updates to name/prompt/interval/enabled.
- **Interacts with**: `AgentDatabase::update_scheduled_job`

### `DeleteScheduledJobTool`
- **Does**: Implements `delete_scheduled_job` by ID.
- **Interacts with**: `AgentDatabase::delete_scheduled_job`

### `open_database()`
- **Does**: Opens the configured runtime database so scheduler tools operate against the same persistent store as the backend.
- **Interacts with**: `AgentConfig::load`, `AgentDatabase::new`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime.rs` | Tool constructors remain available for registration | Renaming/removing tool structs |
| Agent tool-calling | Stable tool names: `list_scheduled_jobs`, `create_scheduled_job`, `update_scheduled_job`, `delete_scheduled_job` | Renaming tools or required-parameter changes |
| `database.rs` | Scheduled-job CRUD APIs remain compatible | Changing method signatures or semantics |

## Notes
- Parameter validation errors are returned as `ToolOutput::Error` so the model can self-correct without crashing the tool loop.
- Intervals are normalized by `ScheduledJob::normalized_interval_minutes` in database-layer create/update paths.
