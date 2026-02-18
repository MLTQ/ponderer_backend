# agentic.rs

## Purpose
Implements the multi-step tool-calling loop that drives autonomous and chat-mode execution. It repeatedly calls the LLM, executes requested tools, feeds tool output back, and stops when the model returns final text or iteration limits are reached. It now supports provider token streaming for OpenAI-compatible backends.

## Components

### `AgenticConfig`
- **Does**: Configures optional iteration limit (`None` = unbounded) and LLM request parameters (`api_url`, `model`, `temperature`, `max_tokens`)
- **Interacts with**: `Agent` runtime setup in `../agent/mod.rs`

### `AgenticLoop::run` / `run_with_history`
- **Does**: Executes the function-calling loop and returns final response + tool call records
- **Interacts with**: `ToolRegistry` (context-filtered tool defs + execution), tool safety checks, OpenAI-compatible chat completions endpoint

### `AgenticLoop::run_with_history_streaming`
- **Does**: Executes the same loop while forwarding incremental assistant text (`content`) to a callback as it streams in
- **Interacts with**: `AgentEvent::ChatStreaming` emission in `../agent/mod.rs`

### `AgenticLoop::run_with_history_streaming_and_tool_events`
- **Does**: Streaming variant that additionally emits a callback for each completed tool call record as the loop runs
- **Interacts with**: `AgentEvent::ToolCallProgress` emission in `../agent/mod.rs`

### `call_llm_streaming`
- **Does**: Calls `chat/completions` with `"stream": true`, parses SSE `data:` payloads, accumulates text/tool-call deltas, and produces a final assistant message
- **Interacts with**: OpenAI, vLLM, and LMStudio-compatible stream payloads; fallback path in `call_llm`

### `AgenticResult`
- **Does**: Returns the visible response, extracted thinking blocks, tool calls made, iteration count, and limit status
- **Interacts with**: Chat formatting and UI rendering in `../agent/mod.rs` and `../ui/chat.rs`

### `split_visible_and_thinking`
- **Does**: Strips `<think>`/`<thinking>` sections from model content and returns hidden reasoning blocks separately
- **Interacts with**: Prevents chain-of-thought leakage into normal user-facing output

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `../agent/mod.rs` | `AgenticResult` includes `response`, `thinking_blocks`, and `tool_calls_made` | Renaming/removing these fields |
| `../agent/mod.rs` | `run_with_history_streaming` callback receives evolving full text and a done flag; tool-event callback receives completed `ToolCallRecord`s | Changing callback semantics |
| `../tools/mod.rs` | Tool calls are executed via `ToolRegistry::execute_call` and tool visibility honors `ToolContext` policy | Changing execution or filtering flow contracts |
| OpenAI-compatible backends | Request/response shape uses `chat/completions` with optional `tools` and optional streaming SSE | Non-compatible payload format changes |

## Notes
- Tool outputs are sanitized before being fed back into the loop.
- Tool definitions are now filtered per `ToolContext` before each loop run, preventing out-of-scope tools from being proposed/called.
- Thinking tags are preserved only as structured metadata (`thinking_blocks`) for optional UI/debug display.
- Streaming failures automatically degrade to the non-streaming code path instead of failing the entire agentic call.
- HTTP client initialization now has a panic-safe fallback (`no_proxy`) if default system proxy discovery fails on host OS APIs.
