# runtime.rs

## Purpose
Provides backend-only runtime bootstrap for Ponderer. This is the non-UI assembly layer that wires config, skills, tool registry, runtime-process plugin discovery, database access, and agent lifecycle so frontend clients can consume backend capabilities through a stable boundary.

## Components

### `BackendRuntime`
- **Does**: Owns the initialized backend runtime objects (`Agent`, `ToolRegistry`, `ProcessRegistry`, `RuntimePluginHost`, `AgentDatabase`, `AgentConfig`), shared agent-loop supervisor health, and loaded plugin manifests.
- **Interacts with**: frontend bootstrap (`src/main.rs`) and future API server entrypoints.

### `AgentLoopSupervisor` / `AgentLoopSupervisorStatus`
- **Does**: Shares a synchronous snapshot of whether the agent task is active, its generation and restart count, its latest start/exit timestamps, and its last error.
- **Interacts with**: `BackendRuntime::spawn_agent_loop()` for lifecycle updates and `server.rs` for truthful health reporting.
- **Notes**: A prior error remains available after recovery for diagnosis; `active` identifies whether the current generation is presently running.

### `BackendRuntimeBuilder`
- **Does**: Provides plugin-aware runtime assembly with `with_plugin` / `with_plugins` hooks before final `build()`.
- **Interacts with**: `plugin.rs` (`BackendPlugin`), built-in skill/tool registration helpers.

### `BackendRuntime::bootstrap(config, event_tx)`
- **Does**: Convenience wrapper around `BackendRuntimeBuilder::new(...).build()` for built-ins-only startup.
- **Interacts with**: `BackendRuntimeBuilder`.

### `BackendRuntime::spawn_agent_loop()`
- **Does**: Runs `Agent::run_loop()` on a dedicated thread with its own Tokio runtime, catches unwinding panics and returned failures at the future boundary, and restarts failed generations with exponential backoff capped at 30 seconds.
- **Interacts with**: background agent lifecycle.

### Built-in registration groups
- **Does**: Registers the core built-in tools; additional skill/tool integrations are supplied by runtime-process or extension plugins.
- **Interacts with**: tool modules under `tools/`, shared runtime services such as `process_registry.rs`, and plugin manifests exposed to the frontend.
- **Notes**: Core tools include schedule-management tools (`list_scheduled_jobs`, `create_scheduled_job`, `update_scheduled_job`, `delete_scheduled_job`) and `private_chat_mode` for runtime chat-mode control (`agentic` vs `direct`).

### Runtime-process plugin discovery
- **Does**: Ensures the shared plugin directory exists, scans it for subprocess-backed runtime bundles, exposes their settings manifests up front, and hands their launch specs to the runtime plugin host.
- **Interacts with**: `runtime_process_plugin.rs` and `runtime_plugin_host.rs`.

### Runtime plugin host bootstrap
- **Does**: Creates the shared `RuntimePluginHost`, exposes discovered runtime-process manifests up front, and passes the host into `Agent`; actual runtime-process startup is deferred to the agent loop so plugin stdio/process handles live on the same runtime as tool execution.
- **Interacts with**: `runtime_plugin_host.rs`, `agent/mod.rs`.

### Built-in manifests
- **Does**: Declares the `builtin.core` plugin manifest.
- **Interacts with**: `plugin.rs` manifest contracts and `/v1/plugins`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `src/main.rs` | `BackendRuntime::bootstrap` and `spawn_agent_loop` remain stable | Renaming/removing bootstrap APIs |
| Backend API server | Runtime object graph can be initialized without UI code, and supervisor snapshots are cheap and thread-safe | Introducing UI dependencies or blocking lifecycle state access |
| External plugin crates | `BackendRuntimeBuilder` accepts `Arc<dyn BackendPlugin>` and executes tool/skill hooks during bootstrap | Removing plugin hooks or changing call order |

## Notes
- This module intentionally centralizes runtime wiring previously located in the desktop entrypoint.
- No UI modules are referenced from this file.
- Runtime bootstrap order is deterministic: built-in core tools first, then discovered runtime-process bundles, then extension plugins.
- Runtime-process plugin manifests are always exposed to the frontend, even if a bundle is installed but currently disabled by config.
- Runtime-process startup intentionally happens inside `Agent::run_loop` (not during `BackendRuntimeBuilder::build`) to avoid creating Tokio process/stdio resources on a short-lived initialization runtime.
- Agent-loop supervision is outside `Agent::run_loop`: each non-`Send` generation remains on the dedicated runtime thread and is wrapped in an unwind boundary so a panic cannot silently kill supervision.
- Restart delays grow from 1 second to a maximum of 30 seconds. Health is inactive during the delay and active once the next generation starts.
- Startup now creates the local `plugins/` directory automatically beside the executable/config for portable installs when no custom `PONDERER_PLUGIN_DIR` is set.
