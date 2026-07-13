# runtime.rs

## Purpose
Provides backend-only runtime bootstrap for Ponderer. This non-UI assembly layer
wires config, the tool registry, protocol-v1 package discovery, database access,
and agent lifecycle behind a stable frontend boundary.

## Components

### `BackendRuntime`
- **Does**: Owns the initialized backend runtime objects (`Agent`, `ToolRegistry`, `ProcessRegistry`, `RuntimePluginHost`, `AgentDatabase`, `AgentConfig`), shared agent-loop supervisor health, and loaded plugin manifests.
- **Interacts with**: frontend bootstrap (`src/main.rs`) and future API server entrypoints.

### `AgentLoopSupervisor` / `AgentLoopSupervisorStatus`
- **Does**: Shares a synchronous snapshot of whether the agent task is active, its generation and restart count, its latest start/exit timestamps, and its last error.
- **Interacts with**: `BackendRuntime::spawn_agent_loop()` for lifecycle updates and `server.rs` for truthful health reporting.
- **Notes**: A prior error remains available after recovery for diagnosis; `active` identifies whether the current generation is presently running.

### `BackendRuntimeBuilder`
- **Does**: Holds bootstrap inputs and constructs the backend object graph through `build()`.
- **Interacts with**: built-in tool registration and runtime-process package discovery.

### `BackendRuntime::bootstrap(config, event_tx)`
- **Does**: Convenience wrapper around `BackendRuntimeBuilder::new(...).build()` for built-ins-only startup.
- **Interacts with**: `BackendRuntimeBuilder`.

### `BackendRuntime::spawn_agent_loop()`
- **Does**: Runs `Agent::run_loop()` on a dedicated thread with its own Tokio runtime, pairs each cognitive generation with a structured plugin-control task, catches failures, and restarts failed generations with capped exponential backoff.
- **Interacts with**: background agent lifecycle and `RuntimePluginHost::apply_config`.

### `supervise_runtime_plugins`
- **Does**: Reconciles package discovery, desired config, process health, and recovery every second on the same long-lived Tokio runtime as plugin I/O.
- **Interacts with**: `Agent::config_snapshot`, `RuntimePluginHost`, and `ToolRegistry`.
- **Rationale**: Plugin senses and tools remain managed while cognition is paused, sleeping, or occupied. The task is aborted and joined when its paired cognitive generation exits so duplicate controllers cannot accumulate.
- **Startup**: One reconciliation completes before cognition begins; the sibling task then owns periodic ticks, preventing a first-turn race with tool registration.

### `current_plugin_manifests` / `plugin_statuses`
- **Does**: Returns live host projections rather than the compatibility manifest snapshot captured at bootstrap.
- **Interacts with**: future status/API surfaces.

### Built-in registration groups
- **Does**: Registers the core built-in tools, including the inert
  `plugin_workbench`; additional integrations are supplied by runtime-process
  packages.
- **Interacts with**: tool modules under `tools/`, shared runtime services such as `process_registry.rs`, and plugin manifests exposed to the frontend.
- **Notes**: Core tools include schedule-management tools (`list_scheduled_jobs`, `create_scheduled_job`, `update_scheduled_job`, `delete_scheduled_job`) and `private_chat_mode` for runtime chat-mode control (`agentic` vs `direct`).

### Runtime-process plugin discovery
- **Does**: Ensures the shared plugin directory exists, scans it for subprocess-backed runtime bundles, exposes their settings manifests up front, and hands their launch specs to the runtime plugin host.
- **Interacts with**: `runtime_process_plugin.rs` and `runtime_plugin_host.rs`.

### Runtime plugin host bootstrap
- **Does**: Creates the shared `RuntimePluginHost` with the host-owned database,
  exposes discovered manifests up front, and passes it to both cognition and the
  independent plugin-control task; process startup occurs on their shared
  long-lived runtime.
- **Interacts with**: `runtime_plugin_host.rs`, `agent/mod.rs`.

### Built-in manifests
- **Does**: Declares the `builtin.core` plugin manifest with the current shared contract versions.
- **Interacts with**: `plugin_contract`, compatibility exports in `plugin.rs`, and `/v1/plugins`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `src/main.rs` | `BackendRuntime::bootstrap` and `spawn_agent_loop` remain stable | Renaming/removing bootstrap APIs |
| Backend API server | Runtime object graph can be initialized without UI code, and supervisor snapshots are cheap and thread-safe | Introducing UI dependencies or blocking lifecycle state access |
| Runtime plugin packages | Discovery feeds the supervised protocol-v1 host; no package code executes during bootstrap | Changing package discovery or startup ownership |

## Notes
- This module intentionally centralizes runtime wiring previously located in the desktop entrypoint.
- No UI modules are referenced from this file.
- Runtime bootstrap order is deterministic: built-in core tools first, then discovered protocol-v1 package manifests.
- Runtime-process plugin manifests are always exposed to the frontend, even if a bundle is installed but currently disabled by config.
- Runtime-process startup intentionally happens in the sibling plugin-control task (not during `BackendRuntimeBuilder::build`) to avoid short-lived initialization runtimes and cognitive pause coupling.
- Agent-loop supervision is outside `Agent::run_loop`: each non-`Send` generation remains on the dedicated runtime thread and is wrapped in an unwind boundary so a panic cannot silently kill supervision.
- Restart delays grow from 1 second to a maximum of 30 seconds. Health is inactive during the delay and active once the next generation starts.
- Startup now creates the local `plugins/` directory automatically beside the executable/config for portable installs when no custom `PONDERER_PLUGIN_DIR` is set.
- The pause-independent reconciliation loop also runs cursor-aware event-ledger
  compaction at most hourly.
- The removed in-process `BackendPlugin` builder hooks were intentionally not
  retained as a second extension mechanism: packages must cross manifest,
  protocol, effect-policy, lifecycle, and receipt boundaries.
