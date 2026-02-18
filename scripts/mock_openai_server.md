# mock_openai_server.py

## Purpose
Provides a deterministic local OpenAI-compatible `/v1/chat/completions` mock for standalone backend parity tests.

## Components

### Foreground chat behavior
- First operator turn returns a `list_directory` tool call.
- Tool-result follow-up returns a `turn_control` continue decision to trigger background handoff.

### Background behavior
- First continuation call returns `decision=continue`.
- Second continuation call returns `decision=yield,status=done`.

### Streaming behavior
- Requests with `"stream": true` return `400` so backend exercises its non-streaming fallback path.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `validate_backend_parity_mock.sh` | Stable deterministic state machine for tool-call + background handoff flow | Response sequence changes without script updates |
| Backend LLM clients | OpenAI-compatible response envelope with `choices[0].message` | Invalid response schema |

## Notes
- This mock is test-only and intentionally minimal.
- It is designed to validate backend orchestration behavior, not model quality.
