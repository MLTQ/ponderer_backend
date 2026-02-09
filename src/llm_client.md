# llm_client.rs

## Purpose
HTTP client for OpenAI-compatible chat completion APIs (Ollama, LM Studio, vLLM, OpenAI, Anthropic). Handles text generation, structured JSON extraction, response-decision queries, and vision/image evaluation.

## Components

### `LlmClient`
- **Does**: Wraps `reqwest::Client` with API URL, key, and model; provides async generation methods
- **Interacts with**: `agent::Agent` (all LLM calls go through this), `config::AgentConfig` (constructed from config fields)

### `LlmClient::generate(messages)`
- **Does**: Sends chat completion request to `{api_url}/chat/completions`, returns the first choice's content string
- **Interacts with**: Any OpenAI-compatible endpoint

### `LlmClient::generate_with_model(messages, model)`
- **Does**: Same as `generate` but allows overriding the model (used for reflection with a different model)
- **Rationale**: Enables using a cheaper/faster model for decision-making vs. a stronger model for generation

### `LlmClient::generate_json<T>(messages, model)`
- **Does**: Generates a response and parses it as JSON type `T`, with fallback extraction from markdown code blocks and raw JSON object detection
- **Interacts with**: `agent::reasoning` (for `DecisionResponse`), `agent::trajectory` (for persona analysis)

### `LlmClient::decide_to_respond(messages, decision_model)`
- **Does**: Asks the LLM whether the agent should respond to a post; returns `DecisionResponse { should_respond, reasoning }`
- **Interacts with**: `config::RespondTo.decision_model` for optional model override

### `LlmClient::evaluate_image(image_bytes, prompt, context)`
- **Does**: Sends image + prompt to a vision model, returns `ImageEvaluation { satisfactory, reasoning, suggested_prompt_refinement }`
- **Interacts with**: `agent::image_gen` for evaluating ComfyUI outputs

### `LlmClient::parse_json<T>(response)`
- **Does**: Robust JSON parser that strips `</think>` tags, markdown fences, and extracts bare JSON objects
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
- Vision support appends base64 image data inline in the message content rather than using the OpenAI vision API's `image_url` format. This may not work with all providers.
- API key is sent as `Bearer` token only when non-empty (local models like Ollama need no key).
