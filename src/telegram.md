# telegram.rs

## Purpose
Telegram bot integration for the standalone backend. It long-polls Telegram for inbound chat messages, routes them into the dedicated `telegram` conversation, waits for the agent's `chat_reply` event, and relays the reply back to Telegram.

## Components

### `TelegramBotManager`
- **Does**: Owns the background Telegram task and live-reconfigures it when token/chat-id settings change.
- **Interacts with**: `server.rs` startup and config-update flow.
- **Rationale**: Telegram settings are editable from the UI, so startup-only bot spawning leaves the runtime out of sync until restart.

### `run_bot`
- **Does**: Runs the long-poll loop, persists inbound messages into the Telegram conversation, wakes the agent, and relays `chat_reply` events.
- **Interacts with**: `database/chat.rs`, `agent/mod.rs`, and `server.rs` WS event bridge.

### `poll_updates`
- **Does**: Calls Telegram `getUpdates`, handles transport/status/JSON failures, and logs Telegram error payloads when the API rejects a request.
- **Interacts with**: Telegram Bot API over `reqwest`.

### `wait_for_reply`
- **Does**: Waits up to 120 seconds for a `chat_reply` event for the Telegram conversation, tolerating lagged broadcast receivers.
- **Interacts with**: `ServerState.ws_events` and `server.rs` event mapping.

### `send_message`
- **Does**: Sends the final agent reply back to Telegram with UTF-8-safe truncation and response-body logging on failure.
- **Interacts with**: Telegram Bot API `sendMessage`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `server.rs` | `TelegramBotManager::reconfigure` can safely start/stop/restart the bot at runtime | Removing live reconfiguration or changing the method signature |
| `agent/mod.rs` | Telegram replies arrive via `chat_reply` for conversation `telegram` | Renaming the event or conversation ID without updating relay logic |
| Operators | Invalid Telegram tokens/chat restrictions produce actionable logs | Removing API error-body logging |

## Notes
- The bot only relays plain text Telegram `message` updates right now.
- Telegram failures were previously hard to diagnose because `ok=false` responses were logged without the API description.
