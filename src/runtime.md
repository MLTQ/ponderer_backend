# runtime.rs

## Purpose
Provides backend-only runtime bootstrap for Ponderer. This is the non-UI assembly layer that wires config, skills, tool registry, filesystem plugin discovery (workflow bundles plus runtime-process bundles), database access, and agent lifecycle so frontend clients can consume backend capabilities through a stable boundary.

## Components

### `BackendRuntime`
- **Does**: Owns the initialized backend runtime objects (`Agent`, `ToolRegistry`, `ProcessRegistry`, `RuntimePluginHost`, `AgentDatabase`, `AgentConfig`) plus the discovered workflow-plugin catalog and loaded plugin manifests.
- **Interacts with**: frontend bootstrap (`src/main.rs`) and future API server entrypoints.

### `BackendRuntimeBuilder`
- **Does**: Provides plugin-aware runtime assembly with `with_plugin` / `with_plugins` hooks before final `build()`.
- **Interacts with**: `plugin.rs` (`BackendPlugin`), built-in skill/tool registration helpers.

### `BackendRuntime::bootstrap(config, event_tx)`
- **Does**: Convenience wrapper around `BackendRuntimeBuilder::new(...).build()` for built-ins-only startup.
- **Interacts with**: `BackendRuntimeBuilder`.

### `BackendRuntime::spawn_agent_loop()`
- **Does**: Runs `Agent::run_loop()` on a dedicated thread with its own Tokio runtime.
- **Interacts with**: background agent lifecycle.

### `build_builtin_skills(config)`
- **Does**: Creates configured built-in skill instances, currently grouped under the OrbWeaver integration.
- **Interacts with**: `skills::graphchan::GraphchanSkill`.

### Built-in registration groups
- **Does**: Registers built-ins in capability groups: core tools, ComfyUI integration (including the generic `run_workflow_plugin` tool), and OrbWeaver integration.
- **Interacts with**: tool modules under `tools/`, `workflow_plugin.rs`, shared runtime services such as `process_registry.rs`, and plugin manifests exposed to the frontend.
- **Notes**: Core tools now include schedule-management tools (`list_scheduled_jobs`, `create_scheduled_job`, `update_scheduled_job`, `delete_scheduled_job`) so the agent can manage recurring jobs directly.

### Workflow plugin discovery
- **Does**: Scans the filesystem for data-only Comfy workflow plugin bundles and appends their manifests to `/v1/plugins`.
- **Interacts with**: `workflow_plugin.rs` and the generic settings UI.

### Runtime-process plugin discovery
- **Does**: Ensures the shared plugin directory exists, scans it for subprocess-backed runtime bundles, exposes their settings manifests up front, and hands their launch specs to the runtime plugin host.
- **Interacts with**: `runtime_process_plugin.rs` and `runtime_plugin_host.rs`.

### Runtime plugin host bootstrap
- **Does**: Creates the shared `RuntimePluginHost`, exposes discovered runtime-process manifests up front, and passes the host into `Agent`; actual runtime-process startup is deferred to the agent loop so plugin stdio/process handles live on the same runtime as tool execution.
- **Interacts with**: `runtime_plugin_host.rs`, `agent/mod.rs`.

### Built-in manifests
- **Does**: Declares three built-in plugin manifests: `builtin.core`, `builtin.comfy`, and `builtin.orbweaver`, with optional settings-tab metadata for the two skill integrations.
- **Interacts with**: `plugin.rs` manifest contracts and `/v1/plugins`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `src/main.rs` | `BackendRuntime::bootstrap` and `spawn_agent_loop` remain stable | Renaming/removing bootstrap APIs |
| Future backend API server | Runtime object graph can be initialized without any UI code | Introducing UI dependencies here |
| External plugin crates | `BackendRuntimeBuilder` accepts `Arc<dyn BackendPlugin>` and executes tool/skill hooks during bootstrap | Removing plugin hooks or changing call order |

## Notes
- This module intentionally centralizes runtime wiring previously located in the desktop entrypoint.
- No UI modules are referenced from this file.
- Runtime bootstrap order is deterministic: built-in core/comfy/orbweaver groups first, then discovered workflow bundles, then discovered runtime-process bundles, then extension plugins.
- Runtime-process plugin manifests are always exposed to the frontend, even if a bundle is installed but currently disabled by config.
- Runtime-process startup intentionally happens inside `Agent::run_loop` (not during `BackendRuntimeBuilder::build`) to avoid creating Tokio process/stdio resources on a short-lived initialization runtime.
- Startup now creates the local `plugins/` directory automatically beside the executable/config for portable installs when no custom `PONDERER_PLUGIN_DIR` is set.
