# orientation.rs

## Purpose
Implements the Living Loop orientation engine: synthesizes presence, concerns, journal continuity, pending events, persona trajectory, and optional desktop-vision observations into a typed situational model (`Orientation`).

## Components

### `OrientationContext`
- **Does**: Bundles all orientation inputs (including optional `DesktopObservation`, recent action digest, and previous OODA packet summary) and exposes formatter helpers for prompt construction
- **Interacts with**: `presence/mod.rs`, `agent/{concerns,journal}.rs`, `skills/mod.rs`, `database.rs` persona snapshots

### `DesktopObservation`
- **Does**: Captures one orientation-time desktop vision summary (`captured_at`, screenshot path, concise summary text)
- **Interacts with**: `agent/mod.rs` screenshot capture/evaluation path and prompt context assembly

### `Orientation` and related types
- **Does**: Typed output model including user-state estimate, salience map, anomalies, pending thoughts, disposition, mood, and synthesis narrative
- **Interacts with**: `agent/mod.rs` loop logging/events and `database.rs` orientation snapshot persistence

### `OrientationEngine`
- **Does**: Builds orientation prompts, requests structured JSON from LLM, parses to typed output, and falls back to heuristic orientation when model output is invalid
- **Interacts with**: `llm_client.rs` (`generate_json`)
- **Rationale**: Orientation should remain available even when local models are noisy or unavailable

### `context_signature`
- **Does**: Produces a stable coarse signature of orientation inputs (including desktop-observation summary digest plus recent-action / prior-OODA context digests) for fast-path skip of redundant LLM calls
- **Interacts with**: `agent/mod.rs` loop cache (`last_orientation_signature`)

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent/mod.rs` | `OrientationEngine::orient` returns a usable `Orientation` (heuristic fallback on failure) | Returning hard errors for parse noise |
| `database.rs` | `Orientation` fields serialize cleanly into snapshot records | Renaming/removing core output fields |
| UI event handling | `Disposition`/`UserStateEstimate` remain stable enough for readable status summaries | Changing enum semantics without updating renderers |

## Notes
- `orient` tolerates model format drift by falling back to heuristic orientation; parse failures are logged at debug level to reduce operator-facing warning noise.
- Orientation JSON parsing accepts common alias field names (`salience_map`, `pending_actions`, `mood_estimate`, etc.) and mixed schema shapes (string or object forms for `user_state`, `mood`, and list entries), reducing parse failures with weaker/local models.
- Fast-path signatures are intentionally bucketed (idle/cpu/memory/time) to reduce unnecessary model calls.
- Desktop observations are optional and only present when the runtime orientation path supplies them.
- Orientation prompts now include explicit `Recent Action Digest` and `Previous OODA Packet` sections when available to keep orientation grounded in immediate turn history.
