# server.rs

## Purpose
Runs the standalone backend HTTP surface for Ponderer. It exposes authenticated REST endpoints for configuration, conversations, messages, turn/tool-call diagnostics, and agent control, plus a WebSocket stream for live agent events.

## Components

### `serve_backend(runtime, event_rx)`
- **Does**: Boots Axum server, validates bind/token env vars, wires runtime state, spawns the agent loop, and starts REST+WS routes.
- **Interacts with**: `runtime.rs` (`BackendRuntime`), `agent/mod.rs` (`AgentEvent`), `database.rs` (`AgentDatabase` chat APIs).

### `ServerState`
- **Does**: Shared application state containing agent handle, DB handle, auth token, mutable config snapshot, and WS broadcaster.
- **Interacts with**: all route handlers and auth middleware.

### REST handlers (`/v1/...`)
- **Does**: Provide CRUD-like operations for config/conversations/messages plus turn/tool-call inspection, plugin manifest discovery, and pause/status controls.
- **Interacts with**: `database.rs` chat lifecycle APIs, `plugin.rs` manifests, and `agent` runtime control methods.

### WS handlers (`/v1/ws/events`)
- **Does**: Broadcasts serialized `ApiEventEnvelope` events (timestamped) to connected clients.
- **Interacts with**: `spawn_event_bridge`, `map_agent_event`, backend frontend/event consumers.

### Auth (`auth_middleware`, `authorize`)
- **Does**: Enforces bearer-token auth on protected routes.
- **Interacts with**: `PONDERER_BACKEND_AUTH_MODE` (`required` default, `disabled` override) and `PONDERER_BACKEND_TOKEN`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Future API frontend client | Stable `/v1` routes and JSON payload structure for conversations/messages/turns/tools | Renaming routes or changing response schemas |
| Backend operators | `PONDERER_BACKEND_BIND` and `PONDERER_BACKEND_TOKEN` env vars control bind + auth | Removing env var support or token requirement |
| Event consumers | WS events contain `event_type`, `emitted_at`, and `payload` | Changing envelope shape or event-type names |
| `agent/mod.rs` | `AgentEvent` variants can be mapped into API event payloads | Removing variants without updating mapping |

## Notes
- `/v1/health` is authenticated in `required` mode, matching deny-by-default auth boundaries.
- `/v1/plugins` exposes loaded plugin manifests (`builtin.core` + extension plugins) for client-side capability discovery.
- Message enqueue validates non-empty content and returns the created `message_id`.
- Conversation-scoped handlers guard against missing conversation IDs with explicit `404` responses.
- `PUT /v1/agent/pause` is preferred for explicit control; `POST /v1/agent/toggle-pause` remains for backward compatibility.
