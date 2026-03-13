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
- **Does**: Executes the same loop while forwarding structured `StreamingUpdate` packets (`content`, `done`, optional token metrics) to a callback as it streams in
- **Interacts with**: `AgentEvent::ChatStreaming` and `AgentEvent::TokenMetrics` emission in `../agent/mod.rs`

### `AgenticLoop::run_with_history_streaming_and_tool_events`
- **Does**: Streaming variant that additionally emits a callback for each completed tool call record as the loop runs
- **Interacts with**: `AgentEvent::ToolCallProgress` emission in `../agent/mod.rs`

### `call_llm_streaming`
- **Does**: Calls `chat/completions` with `"stream": true`, opportunistically requests token logprobs, parses SSE `data:` payloads, accumulates text/tool-call deltas, and produces a final assistant message
- **Interacts with**: OpenAI, vLLM, and LMStudio-compatible stream payloads; fallback path in `call_llm`

### `StreamingTokenMetric` / `StreamingUpdate`
- **Does**: Represent the per-token-ish novelty samples and per-chunk stream update packets emitted to the agent/UI layer.
- **Interacts with**: `../agent/mod.rs` stream callbacks and the frontend token monitor via the backend WS bridge.

### `TokenNoveltyTracker`
- **Does**: Builds a cheap fallback novelty score from streamed text fragments when provider logprobs are unavailable, using rolling token frequency and bigram reuse.
- **Interacts with**: `call_llm_streaming` and the non-streaming fallback path.

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
| `../agent/mod.rs` | `run_with_history_streaming` callback receives `StreamingUpdate { content, done, token_metrics }`; tool-event callback receives completed `ToolCallRecord`s | Changing callback semantics or metric payload fields |
| `../tools/mod.rs` | Tool calls are executed via `ToolRegistry::execute_call` and tool visibility honors `ToolContext` policy | Changing execution or filtering flow contracts |
| OpenAI-compatible backends | Request/response shape uses `chat/completions` with optional `tools` and optional streaming SSE | Non-compatible payload format changes |

## Notes
- Tool outputs are sanitized before being fed back into the loop.
- Tool definitions are now filtered per `ToolContext` before each loop run, preventing out-of-scope tools from being proposed/called.
- Thinking tags are preserved only as structured metadata (`thinking_blocks`) for optional UI/debug display.
- Streaming failures automatically degrade to the non-streaming code path instead of failing the entire agentic call.
- Non-streaming verification fallback now runs only when the initial streaming response returned neither tool calls nor visible text, reducing duplicate first-pass LLM calls.
- Streaming requests now try `logprobs` / `top_logprobs` first and retry without them when the provider rejects those fields.
- When logprobs are missing, token metrics still flow using a lightweight local tokenizer and novelty estimator so the UI can render a stable live trace.
- HTTP client initialization now has a panic-safe fallback (`no_proxy`) if default system proxy discovery fails on host OS APIs.
