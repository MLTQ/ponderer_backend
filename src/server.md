# server.rs

## Purpose
Runs the standalone backend HTTP surface for Ponderer. It exposes authenticated REST endpoints for configuration, conversations, messages, turn/tool-call diagnostics, and agent control, plus a WebSocket stream for live agent events.

## Components

### `serve_backend(runtime, event_rx)`
- **Does**: Boots Axum server, validates bind/token env vars, wires runtime state, spawns the agent loop, and starts REST+WS routes.
- **Interacts with**: `runtime.rs` (`BackendRuntime`), `agent/mod.rs` (`AgentEvent`), `database.rs` (`AgentDatabase` chat APIs).

### `ServerState`
- **Does**: Shared application state containing agent handle, DB handle, auth token, mutable config snapshot, shared process registry, plugin manifests, and WS broadcaster.
- **Interacts with**: all route handlers and auth middleware.

### REST handlers (`/v1/...`)
- **Does**: Provide CRUD-like operations for config/conversations/messages, scheduled jobs, process inspection, turn/tool-call/prompt inspection, plugin manifest discovery, pause/status/stop controls, direct private-chat-mode get/set control, and tool session-approval grants. Config updates normalize private-chat mode before save/reload. Message enqueue also triggers an immediate agent wake signal.
- **Interacts with**: `database.rs` chat + scheduled-job APIs, `process_registry.rs`, `plugin.rs` manifests, and `agent` runtime control methods.

### Scheduled-job routes (`/v1/scheduled-jobs`)
- **Does**: Exposes list/create/get/update/delete endpoints for interval-based recurring jobs backed by SQLite.
- **Interacts with**: `database.rs` scheduled-job CRUD and `agent/mod.rs` due-job enqueueing.

### Process routes (`/v1/processes`)
- **Does**: Lists tracked background processes, returns one process snapshot, and requests process shutdown.
- **Interacts with**: `process_registry.rs` and `tools/shell.rs` detached execution mode.

### `POST /v1/agent/tools/:tool_name/approve`
- **Does**: Grants session-level approval for a specific tool, allowing it to run autonomously without prompting for the rest of the process lifetime.
- **Interacts with**: `agent/mod.rs` `Agent::grant_session_tool_approval` → `ToolRegistry::grant_session_approval`.

### `cycle_start` WS event
- **Does**: Emitted by `map_agent_event` whenever the backend fires `AgentEvent::CycleStart { label }`. Carries a `label` string (e.g. `"💬 Engaged"`, `"🌿 Ambient"`) that the frontend uses to group activity-log events into collapsible turn groups.

### `token_metrics` WS event
- **Does**: Broadcasts live token novelty samples for the current streamed reply (`conversation_id`, `clear`, `samples[]` with `text`, optional `logprob`/`entropy`, and `novelty`).
- **Interacts with**: `tools/agentic.rs` streaming callbacks and the desktop token monitor UI.

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
- `/v1/plugins` exposes loaded plugin manifests (built-ins, discovered workflow bundles, discovered runtime-process bundles, and extension plugins) for client-side capability discovery, including optional settings-tab metadata and inline settings schemas for the desktop settings window.
- Runtime-process plugins are initialized by `Agent::run_loop` on the dedicated agent runtime thread so plugin stdio/process handles and tool execution share one Tokio runtime context.
- Message enqueue validates non-empty content and returns the created `message_id`.
- Message enqueue (`POST /v1/conversations/:id/messages`) now nudges the agent runtime to wake immediately instead of waiting for the next ambient/poll sleep interval.
- The WS stream now includes `token_metrics` alongside `chat_streaming`, allowing clients to render per-token-ish novelty traces without polling.
- Conversation-scoped handlers guard against missing conversation IDs with explicit `404` responses.
- `GET /v1/turns/:id/prompt` returns the stored per-turn context prompt plus optional stored system prompt, enabling richer per-message context inspection in the frontend.
- `PUT /v1/agent/pause` is preferred for explicit control; `POST /v1/agent/toggle-pause` remains for backward compatibility.
- `GET/PUT /v1/agent/private-chat-mode` provides a narrow API for top-level Direct/Agentic toggles without requiring full config round-trips.
- `POST /v1/agent/stop` requests immediate cancellation of in-flight agentic turns and aborts detached background subtasks.
- Scheduled-job CRUD routes now wake the agent loop immediately after create/update/delete so timing/config changes are applied without waiting for the next ambient/legacy sleep interval.
- Config updates sanitize `private_chat_mode` (`agentic` or `direct`) before persisting and reloading runtime state.
- Process routes only expose processes started through the tracked background shell path.
