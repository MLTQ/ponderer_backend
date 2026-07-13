# plugin_lifecycle.rs

## Purpose

Defines the pure desired/actual lifecycle state machine for one supervised plugin. It records truthful operational status and reserves work before asynchronous process operations, without depending on a transport, Tokio task, or the cognitive agent loop.

## Components

### `PluginLifecycleMachine`

- **Does**: Reconciles availability and desired enablement into explicit start/stop actions, records process outcomes, and schedules recovery through `PluginRestartPolicy`.
- **Interacts with**: `plugin_restart_policy.rs` and the future plugin supervisor.
- **Rationale**: A pure state machine makes control-plane behavior testable and allows reconciliation to run independently while cognition is paused.

### `PluginDesiredState` / `PluginOperationalState`

- **Does**: Separate operator intent from actual runtime state, including package unavailability, active transitions, degraded health, backoff, an open circuit, and terminal failure.
- **Rationale**: The API must not report an enabled setting as though it proved that a process is running.

### `PluginLifecycleAction` / `PluginStartReason`

- **Does**: Reserve exactly one generation-tagged start or stop operation per reconciliation transition and distinguish initial starts, desired-state resumption, retries, and half-open circuit probes.
- **Interacts with**: The future process supervisor, which performs the returned asynchronous work and records its outcome.

### `PluginLifecycleSnapshot`

- **Does**: Captures internal lifecycle state, timestamps, generations, retry counts, the bounded last error, and the next retry deadline for conversion into the public status DTO.
- **Interacts with**: `PluginRuntimeStatus` in `plugin_contract.rs` through an explicit conversion boundary.

### `PluginLifecycleTransitionError`

- **Does**: Rejects stale or impossible async completions instead of silently corrupting lifecycle state.

### `reset_recovery_after_input_change`

- **Does**: Closes backoff, an open circuit, or terminal failure after settings/package inputs materially change, making the plugin immediately eligible for reconciliation.
- **Rationale**: Recovery policy applies to one attempted input, not to the plugin identity forever; an operator fix should not wait behind stale cooldown.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Future plugin supervisor | Call `reconcile` before doing I/O, then return the action's generation to `mark_running`, `mark_stopped`, or a failure method | Bypassing the reservation sequence, dropping generation tokens, or making actions non-exclusive |
| Plugin status API | Snapshot distinguishes desired state, package availability, and actual process state | Collapsing operational states or changing timestamp meanings |
| `plugin_restart_policy.rs` | Consecutive failures reset only after stable execution or an intentional disabled/unavailable settlement | Resetting on process spawn or handshake alone |

## Notes

- Unexpected exits must use `mark_failed`; `mark_stopped` is reserved for an intentional stop already placed in `Stopping` by reconciliation.
- Generation tokens reject late async completions from a replaced process instance.
- `mark_degraded` records a health failure while the process remains usable; `mark_healthy` can return it to `Running` without consuming a restart.
- The degraded transition is intentionally reserved for future health signals
  known not to create process/host state ambiguity. Callback acceptance and
  reconfiguration failures restart instead, so protocol-v1 rollback remains
  sound.
- `mark_terminal_failure` suppresses automatic retry until disabling/reenabling or package availability changes clears the terminal state.
- A supervisor calls `reset_recovery_after_input_change` only when it observes materially changed configuration or a replacement package.
- The half-open circuit probe becomes `Running` after startup but retains its failure streak. A prompt failure reopens the circuit; a stable run resets the streak.
- Disabling a plugin or removing its package cancels pending retries. A subsequent enable or rediscovery begins a fresh desired-state start.
- Retained error text is trimmed and bounded to 2,000 Unicode scalar values so status snapshots cannot grow without limit.
