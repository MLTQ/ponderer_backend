# ponderer_backend.rs

## Purpose
Standalone backend executable for Ponderer. Boots config + runtime and serves the REST/WebSocket API without launching any UI.

## Components

### `main()`
- **Does**: Initializes logging, loads `AgentConfig`, creates the event channel, bootstraps `BackendRuntime`, and launches `serve_backend`.
- **Interacts with**: `config.rs`, `runtime.rs`, and `server.rs`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Operators/scripts | `cargo run --manifest-path ponderer_backend/Cargo.toml --bin ponderer_backend` starts backend-only service | Renaming/removing bin target |
| `server.rs` | Receives a fully initialized `BackendRuntime` and event receiver | Changing runtime bootstrap semantics |

## Notes
- Auth defaults to `required` via `PONDERER_BACKEND_AUTH_MODE`; in that mode `PONDERER_BACKEND_TOKEN` is mandatory.
- Optional bind override via `PONDERER_BACKEND_BIND`.
- Uses the same config discovery logic as desktop mode (`AgentConfig::load()`).
- Bootstraps `BackendRuntime` before creating the server Tokio runtime to avoid nested-runtime panics.
