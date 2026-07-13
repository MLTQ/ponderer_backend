# dream.rs

## Purpose
Implements Ponderer's private Dream pass: a bounded, non-agentic LLM consolidation of recent lived context into a durable continuity artifact. Dream is designed to support an evolving sense of temporal continuity without prematurely turning personality into a scorecard or allowing reflection to act on the world.

## Components

### `DreamInput`
- **Does**: Carries already-summarized orientation, journal, concern, intention, action, prior-Dream, and current self-description context
- **Interacts with**: `agent/mod.rs`, the journal/concern/intention stores, and latest persona history
- **Rationale**: Keeps input selection and database access in the orchestrator while making consolidation independently testable

### `DreamConsolidation`
- **Does**: Stores a concise synthesis plus recurring patterns, unresolved tensions, temporal continuities, and future orientation cues
- **Interacts with**: `database/dream.rs` persistence and shared self-context prompt construction
- **Rationale**: This is a revisable continuity artifact, not a canonical identity definition

### `DreamEngine`
- **Does**: Makes one structured LLM call and validates, deduplicates, and bounds the resulting artifact
- **Interacts with**: `llm_client.rs`
- **Rationale**: The engine has no `ToolRegistry` or capability context, so private consolidation cannot produce external side effects

### Untrusted source formatting
- **Does**: Quotes each historical item inside a named `BEGIN_UNTRUSTED_SOURCE` / `END_UNTRUSTED_SOURCE` block and explicitly tells both system and user prompts to ignore embedded instructions
- **Interacts with**: orientation, journal, concern, intention, prior-Dream, action-digest, and persona text
- **Rationale**: Historical text may contain user/plugin/model prompt injection and is evidence, never authority

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent/mod.rs` | `consolidate` returns `Ok(None)` for an intentional no-op and one bounded artifact otherwise | Treating a model skip as an error or adding tool execution |
| `database/dream.rs` | `DreamConsolidation` remains serde-compatible and carries a unique ID/timestamp | Renaming persisted fields without migration |
| Prompt consumers | Dream artifacts are advisory, grounded, and explicitly non-canonical | Treating synthesis as a replacement system prompt |

## Notes
- Inputs are capped at 12 items per category and 600 characters per item.
- Outputs are capped at eight items per category, 400 characters per item, and 1,600 characters for synthesis.
- The prompt explicitly forbids personality scoring/formalization and asks the model to preserve uncertainty.
- Current orientation, actions, journal, concerns, and intentions precede prior Dream and self-description material so fresh evidence remains prominent.
- Embedded source lines are prefixed with `| `, preventing source text from closing its own trust-boundary marker.
- HTTP deadlines come from the shared bounded `LlmClient` transport.
