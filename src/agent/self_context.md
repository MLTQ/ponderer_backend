# self_context.rs

## Purpose
Builds a compact, provenance-labelled bridge from durable experience into the next LLM context. It lets Ponderer carry a revisable sense of continuity across turns and restarts without presenting generated self-description as an authoritative identity.

## Components

### `TemporalSelfContext`
- **Does**: Holds the latest self-description, Dream consolidation, open intentions, active concerns, and orientation summary
- **Interacts with**: `agent/mod.rs` context hydration, `agent/dream.rs`, database persona/concern/intention/Dream stores
- **Rationale**: Gives every loop the same bounded/provenance framing while letting the orchestrator select sources appropriate to each loop's privacy boundary.

### `TemporalSelfContext::render`
- **Does**: Produces a bounded prompt section with explicit provenance, non-canonical framing, and named untrusted-source blocks whose lines preserve original newline boundaries
- **Interacts with**: private chat, scheduled work, skill-event, self-directive, and Dream prompt builders
- **Rationale**: Historical text can guide attention but must not outrank current evidence or operator intent

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Prompt builders | Empty source data renders as an empty string; non-empty data starts with `## Temporal Self-Context` | Injecting fabricated defaults or returning unbounded history |
| Context hydrator | Fields accept already-summarized strings and tolerate partial database availability | Making any source mandatory |
| Safety policy | Rendered history is explicitly advisory and revisable | Relabelling prior generated text as a system instruction or canonical identity |

## Notes
- The entire rendered context is capped at 4,000 characters.
- Each collection contributes at most eight items; individual items preserve line boundaries and are character-capped.
- Source lines are prefixed with `| ` so embedded boundary markers and imperative text remain visibly quoted data.
- Complete source blocks are added only while they fit the budget, preventing global truncation from leaving an unterminated trust boundary.
- Latest orientation, active concerns, and open intentions are rendered before Dream and self-description material so budget pressure retains fresher evidence.
- This module performs no I/O. Database selection and recency policy remain in the orchestrator.
- The orchestrator uses two hydration policies: rich agent-wide continuity for ambient/internal loops, and a private-chat variant limited to coarse timed orientation plus intentions explicitly scoped to that conversation. The renderer does not itself decide whether a source crosses a privacy boundary.
