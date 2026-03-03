# process_registry.rs

## Purpose
Tracks long-lived shell processes started by the agent so they can be inspected and stopped later. This gives Ponderer a first-class background-process primitive instead of treating every shell command as a blocking one-shot call.

## Components

### `ProcessRegistry`
- **Does**: Starts background processes, stores them by ID, lists current metadata, returns one process snapshot, and requests shutdown.
- **Interacts with**: `tools/shell.rs` for detached command execution and `server.rs` for operator-facing process endpoints.

### `ProcessInfo`
- **Does**: Serializable snapshot of one tracked process (command, cwd, PID, status, exit code, timestamps, and captured output tail).
- **Interacts with**: REST JSON responses and tool responses.

### Background readers / poller
- **Does**: Stream stdout/stderr into a bounded in-memory tail and poll for process exit without blocking stop requests.
- **Interacts with**: `tokio::process::Child` and `ProcessInfo.recent_output`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `tools/shell.rs` | `start`, `get`, `list`, and `stop` remain async and return `ProcessInfo` snapshots | Renaming methods or changing return shape |
| `server.rs` | `ProcessInfo` stays serializable for REST responses | Removing fields or changing field types |

## Notes
- Output capture is intentionally bounded to a rolling tail to avoid unbounded memory growth.
- Process lifecycle state is eventually consistent: a stop request marks a process as `stopping` until the poller observes the exit.
