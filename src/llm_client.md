# llm_client.rs

## Purpose
HTTP client for OpenAI-compatible chat completion APIs (Ollama, LM Studio, vLLM, OpenAI, Anthropic). Handles text generation, structured JSON extraction, response-decision queries, and vision/image evaluation.

## Components

### `LlmClient`
- **Does**: Wraps `reqwest::Client` with API URL, key, and model; provides async generation methods
- **Interacts with**: `agent::Agent` (all LLM calls go through this), `config::AgentConfig` (constructed from config fields)

### `LlmClient::generate(messages)`
- **Does**: Sends chat completion request to a normalized OpenAI-compatible endpoint (`.../v1/chat/completions`), returns the first choice's content string
- **Interacts with**: Any OpenAI-compatible endpoint

### `LlmClient::generate_with_model(messages, model)`
- **Does**: Same as `generate` but allows overriding the model (used for reflection with a different model); normalizes base URLs to OpenAI-compatible `/v1/chat/completions` when needed
- **Rationale**: Enables using a cheaper/faster model for decision-making vs. a stronger model for generation

### `LlmClient::generate_json<T>(messages, model)`
- **Does**: Generates a response and parses it as JSON type `T` via the shared robust parser (`parse_json`), including cleanup of `<think>` wrappers, markdown code fences, and bare JSON extraction
- **Interacts with**: `agent::reasoning` (for `DecisionResponse`), `agent::trajectory` (for persona analysis)

### `LlmClient::decide_to_respond(messages, decision_model)`
- **Does**: Asks the LLM whether the agent should respond to a post; returns `DecisionResponse { should_respond, reasoning }`
- **Interacts with**: `config::RespondTo.decision_model` for optional model override

### `LlmClient::evaluate_image(image_bytes, prompt, context)`
- **Does**: Preprocesses images (resize/compress), sends image + prompt to a vision model using OpenAI-style multimodal content (`image_url`), and returns `ImageEvaluation { satisfactory, reasoning, suggested_prompt_refinement }`. Includes a constrained inline-base64 fallback path for providers that reject multimodal payloads.
- **Interacts with**: `agent::image_gen` for evaluating ComfyUI outputs

### `LlmClient::parse_json<T>(response)`
- **Does**: Robust JSON parser that tries multiple candidates from noisy model output (raw text, post-`</think>` tail, fenced code blocks, balanced JSON extraction), and also handles double-encoded JSON strings
- **Rationale**: LLMs often wrap JSON in markdown or reasoning tags; this handles common output formats

### `Message`
- **Does**: Simple `{ role, content }` struct for chat messages
- **Interacts with**: Used by all generation methods and the agent's context building

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent::Agent` | `LlmClient::new(url, key, model)` and async `generate`/`generate_json` methods | Changing method signatures |
| `agent::reasoning` | `DecisionResponse` has `should_respond: bool` and `reasoning: String` | Changing `DecisionResponse` fields |
| `agent::image_gen` | `evaluate_image` returns `ImageEvaluation` | Changing `ImageEvaluation` fields |

## Notes
- Temperature hardcoded to 0.7, max_tokens to 2000 (1000 for vision). Not configurable.
- Vision requests now prefer OpenAI-compatible multimodal payloads (`content: [{type:text}, {type:image_url}]`) and downscale/compress images before upload to avoid context blowups from large desktop screenshots.
- If multimodal parsing/response handling fails, vision falls back to a strict-size inline-base64 path for compatibility.
- API key is sent as `Bearer` token only when non-empty (local models like Ollama need no key).
- Chat endpoint normalization accepts base URL forms like `http://host:port`, `http://host:port/v1`, or full `.../v1/chat/completions`.
- JSON extraction now tolerates markdown-wrapped ` ```json ... ``` ` payloads and quoted JSON payloads that some providers emit.
- HTTP client initialization now uses shared panic-safe construction from `http_client.rs`; default mode avoids system proxy discovery (`no_proxy`) for portability, with optional `PONDERER_ENABLE_SYSTEM_PROXY=1` override.
