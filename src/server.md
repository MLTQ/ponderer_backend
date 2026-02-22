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
- **Does**: Provide CRUD-like operations for config/conversations/messages plus turn/tool-call/prompt inspection, plugin manifest discovery, pause/status/stop controls, and tool session-approval grants. Message enqueue also triggers an immediate agent wake signal.
- **Interacts with**: `database.rs` chat lifecycle APIs, `plugin.rs` manifests, and `agent` runtime control methods.

### `POST /v1/agent/tools/:tool_name/approve`
- **Does**: Grants session-level approval for a specific tool, allowing it to run autonomously without prompting for the rest of the process lifetime.
- **Interacts with**: `agent/mod.rs` `Agent::grant_session_tool_approval` → `ToolRegistry::grant_session_approval`.

### `cycle_start` WS event
- **Does**: Emitted by `map_agent_event` whenever the backend fires `AgentEvent::CycleStart { label }`. Carries a `label` string (e.g. `"💬 Engaged"`, `"🌿 Ambient"`) that the frontend uses to group activity-log events into collapsible turn groups.

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
- Message enqueue (`POST /v1/conversations/:id/messages`) now nudges the agent runtime to wake immediately instead of waiting for the next ambient/poll sleep interval.
- Conversation-scoped handlers guard against missing conversation IDs with explicit `404` responses.
- `GET /v1/turns/:id/prompt` returns the stored per-turn context prompt plus optional stored system prompt, enabling richer per-message context inspection in the frontend.
- `PUT /v1/agent/pause` is preferred for explicit control; `POST /v1/agent/toggle-pause` remains for backward compatibility.
- `POST /v1/agent/stop` requests immediate cancellation of in-flight agentic turns and aborts detached background subtasks.
