# validate_backend_parity_mock.sh

## Purpose
Runs deterministic standalone parity validation using a local mock OpenAI-compatible server. Validates autonomous continuation -> background-subtask handoff and websocket event flow without external model dependencies.

## Components

### Local mock model bootstrap
- **Does**: Starts `mock_openai_server.py` and points backend `llm_api_url` at it through a temporary `ponderer_config.toml`.

### Standalone backend boot
- **Does**: Starts `ponderer_backend` in a temp working directory, auth disabled for local ws capture simplicity.

### Parity assertions
- **Does**:
  - Creates conversation and sends operator message
  - Confirms foreground turn decides `continue`
  - Confirms background turns (`iteration >= 100`) and final `yield/status=done`
  - Confirms agent/background response persisted in chat history
  - Captures `/v1/ws/events` and validates `chat_streaming`, `tool_call_progress`, `action_taken`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `Ponderer-cpf.9.6` validation | Script exits non-zero on orchestration regressions | Removing assertions or changing expected turn semantics |
| Backend chat turn model | `iteration`, `decision`, `status` fields in `/v1/conversations/:id/turns` | Schema/semantic changes require script updates |
| WS stream consumers | `event_type` envelope field presence | Event envelope schema changes |

## Notes
- This script focuses on orchestration parity, not LLM quality.
- It intentionally uses auth-disabled mode to simplify WS capture (auth boundaries are validated separately in `validate_backend_standalone.sh`).
