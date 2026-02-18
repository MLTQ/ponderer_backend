# main.rs

## Purpose
Desktop entry point for the API-only frontend. Initializes logging, resolves backend API connection settings, and launches the egui UI without bootstrapping an in-process backend runtime.

## Components

### `main()`
- **Does**: Orchestrates startup: logging -> fallback config load -> API client creation -> UI launch.
- **Interacts with**: `api::ApiClient`, `ponderer_backend::config::AgentConfig`, `ui::app::AgentApp`.
- **Rationale**: Frontend and backend are now hard-separated; this binary is a pure client.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `ui::app::AgentApp` | `AgentApp::new(api_client, fallback_config)` signature | Changing constructor args breaks startup wiring |
| Backend service | URL/token come from `PONDERER_BACKEND_URL` / `PONDERER_BACKEND_TOKEN` | Changing env names without updating `ApiClient::from_env` |
| Shared config model | `AgentConfig::load()` remains available for local fallback panel state | Removing config load API |

## Notes
- Default backend URL is `http://127.0.0.1:8787` when env var is unset.
- If token env var is missing, frontend still launches but authenticated API calls will fail unless backend auth mode is disabled.
