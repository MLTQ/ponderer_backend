# orientation.rs

## Purpose
Implements the Living Loop orientation engine: synthesizes presence, concerns, journal continuity, pending events, persona trajectory, and optional desktop-vision observations into a typed situational model (`Orientation`).

## Components

### `OrientationContext`
- **Does**: Bundles all orientation inputs (including optional `DesktopObservation`, recent action digest, previous OODA packet, latest Dream consolidation, and open durable intentions) and exposes formatter helpers for prompt construction
- **Interacts with**: `presence/mod.rs`, `agent/{concerns,journal,dream}.rs`, `intentions.rs`, `skills/mod.rs`, and database persistence

### `DesktopObservation`
- **Does**: Captures one orientation-time desktop vision summary (`captured_at`, screenshot path, concise summary text)
- **Interacts with**: `agent/mod.rs` screenshot capture/evaluation path and prompt context assembly

### `Orientation` and related types
- **Does**: Typed output model including user-state estimate, salience map, anomalies, pending thoughts, disposition, mood, and synthesis narrative; `from_snapshot` rehydrates the latest durable orientation after restart
- **Interacts with**: `agent/mod.rs` loop logging/events and `database.rs` orientation snapshot persistence

### `OrientationEngine`
- **Does**: Builds orientation prompts, requests structured JSON from LLM, parses to typed output, and falls back to heuristic orientation when model output is invalid
- **Interacts with**: `llm_client.rs` (`generate_json`)
- **Rationale**: Orientation should remain available even when local models are noisy or unavailable

### Untrusted source formatting
- **Does**: Quotes every supplied signal/history/plugin block as untrusted evidence and places the trusted JSON output contract after all data blocks
- **Interacts with**: `OrientationEngine` prompt construction and runtime plugin contributions
- **Rationale**: User, plugin, journal, concern, persona, and prior-model text must not become executable prompt instructions

### `OrientationEngine::build_orientation_prompt_with_contributions`
- **Does**: Appends bounded runtime-plugin context blocks to the base orientation prompt, currently via the `orientation.context` slot rendered under `## Plugin Context`.
- **Interacts with**: `runtime_plugin_host.rs` prompt-slot types and merge helpers.

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
- Fast-path signatures are intentionally bucketed (five-minute idle/time continuity plus coarse CPU/memory state) so stable presence does not spend an orientation LLM call every ambient tick.
- Desktop observations are optional and only present when the runtime orientation path supplies them.
- Timestamped desktop, journal, concern, and persona observations include `observed_at` and `age_seconds` cues.
- Current machine/presence/desktop/event/action evidence is ordered before Dream and persona material so fresh evidence remains prominent.
- Orientation prompts include `Recent Action Digest`, `Previous OODA Packet`, `Latest Dream Consolidation`, and `Open Intentions` sections so immediate action history and longer temporal continuity jointly inform the next orientation.
- Orientation prompt assembly now has a dedicated additive plugin slot (`orientation.context`) so future runtime plugins can supply context without replacing the base orientation prompt.
- Plugin slot text is treated as untrusted data rather than instruction authority inside orientation synthesis.
- `with_generation_observer` makes orientation model calls observable without giving this engine knowledge of websocket or UI types.
