# runtime.rs

## Purpose
Provides backend-only runtime bootstrap for Ponderer. This is the non-UI assembly layer that wires config, skills, tool registry, database access, and agent lifecycle so frontend clients can consume backend capabilities through a stable boundary.

## Components

### `BackendRuntime`
- **Does**: Owns the initialized backend runtime objects (`Agent`, `ToolRegistry`, `AgentDatabase`, `AgentConfig`) plus loaded plugin manifests.
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

### `build_skills(config)`
- **Does**: Creates configured built-in skill instances (currently Graphchan).
- **Interacts with**: `skills::graphchan::GraphchanSkill`.

### `register_builtin_tools(registry)`
- **Does**: Registers all built-in tools into `ToolRegistry`.
- **Interacts with**: tool modules under `tools/`.

### `builtin_manifest(config)`
- **Does**: Declares built-in tool/skill capability manifest as a first-class plugin entry (`builtin.core`).
- **Interacts with**: `plugin.rs` manifest contracts.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `src/main.rs` | `BackendRuntime::bootstrap` and `spawn_agent_loop` remain stable | Renaming/removing bootstrap APIs |
| Future backend API server | Runtime object graph can be initialized without any UI code | Introducing UI dependencies here |
| External plugin crates | `BackendRuntimeBuilder` accepts `Arc<dyn BackendPlugin>` and executes tool/skill hooks during bootstrap | Removing plugin hooks or changing call order |

## Notes
- This module intentionally centralizes runtime wiring previously located in the desktop entrypoint.
- No UI modules are referenced from this file.
- Runtime bootstrap order is deterministic: built-in tools/skills first, then extension plugins.
