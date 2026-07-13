# generation_telemetry.rs

## Purpose
Defines the transport-neutral observability contract for every model generation. It assigns each output a stable identity and cognitive source, derives token novelty when providers omit logprobs, and emits lifecycle/sample events without depending on the desktop UI.

## Components

### `GenerationSource` / `GenerationOutcome`
- **Does**: Classify why a model was invoked and how its request ended.
- **Interacts with**: agent orchestration, backend WebSocket mapping, and frontend path labels.

### `GenerationObserver`
- **Does**: Binds a source and optional conversation to an event sink, starts generation sessions, and instruments complete non-streaming text.
- **Interacts with**: `llm_client.rs`, `tools/agentic.rs`, and direct legacy model callers.

### `GenerationSession`
- **Does**: Emits one start event, zero or more metric batches, and exactly one terminal event; dropping an unfinished session reports failure.
- **Interacts with**: streaming and non-streaming model transports.

### `TokenNoveltyTracker`
- **Does**: Converts provider tokens or arbitrary text fragments into normalized novelty samples using token frequency, bigram novelty, logprob surprisal, and optional entropy.
- **Interacts with**: `GenerationObserver::observe_complete_text` and the agentic SSE parser.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent/mod.rs` | Generation events can be wrapped in `AgentEvent` and forwarded synchronously | Changing event lifecycle or source semantics |
| `tools/agentic.rs` | Provider and synthetic samples use one scoring implementation | Forking tracker behavior between transports |
| Desktop frontend | Every output has a generation ID and source before metric samples arrive | Removing IDs or coalescing separate requests |

## Notes
- Telemetry is observational only and never persists model content.
- Non-streaming callers emit their metrics after completion; streaming callers emit incrementally.
- Failed requests can emit lifecycle events without samples, allowing clients to ignore empty paths while retaining truthful diagnostics.
