# orientation.rs

## Purpose
Implements the Living Loop orientation engine: synthesizes presence, concerns, journal continuity, pending events, and persona trajectory into a typed situational model (`Orientation`).

## Components

### `OrientationContext`
- **Does**: Bundles all orientation inputs and exposes formatter helpers for prompt construction
- **Interacts with**: `presence/mod.rs`, `agent/{concerns,journal}.rs`, `skills/mod.rs`, `database.rs` persona snapshots

### `Orientation` and related types
- **Does**: Typed output model including user-state estimate, salience map, anomalies, pending thoughts, disposition, mood, and synthesis narrative
- **Interacts with**: `agent/mod.rs` loop logging/events and `database.rs` orientation snapshot persistence

### `OrientationEngine`
- **Does**: Builds orientation prompts, requests structured JSON from LLM, parses to typed output, and falls back to heuristic orientation when model output is invalid
- **Interacts with**: `llm_client.rs` (`generate_json`)
- **Rationale**: Orientation should remain available even when local models are noisy or unavailable

### `context_signature`
- **Does**: Produces a stable coarse signature of orientation inputs for fast-path skip of redundant LLM calls
- **Interacts with**: `agent/mod.rs` loop cache (`last_orientation_signature`)

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent/mod.rs` | `OrientationEngine::orient` returns a usable `Orientation` (heuristic fallback on failure) | Returning hard errors for parse noise |
| `database.rs` | `Orientation` fields serialize cleanly into snapshot records | Renaming/removing core output fields |
| UI event handling | `Disposition`/`UserStateEstimate` remain stable enough for readable status summaries | Changing enum semantics without updating renderers |

## Notes
- `orient` currently tolerates model format drift by falling back to heuristic orientation.
- Fast-path signatures are intentionally bucketed (idle/cpu/memory/time) to reduce unnecessary model calls.
