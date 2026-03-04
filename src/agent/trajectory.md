# trajectory.rs

## Purpose
Implements persona trajectory inference and snapshot capture. It analyzes historical persona snapshots, predicts direction across guiding-principle dimensions, and persists self-reflection snapshots for long-term companion continuity.

## Components

### `TrajectoryEngine`
- **Does**: Calls the LLM to infer narrative trajectory from snapshot history and returns typed `TrajectoryAnalysis`.
- **Interacts with**: `agent/mod.rs` periodic persona-evolution flow, `database.rs` persona snapshot storage.

### `TrajectoryEngine::infer_trajectory`
- **Does**: Handles empty-history fallback, builds the trajectory prompt, calls LLM, parses structured response.
- **Interacts with**: `parse_trajectory_response`, `extract_json`.

### `TrajectoryEngine::build_trajectory_prompt_with_contributions`
- **Does**: Extends the trajectory prompt builder with a bounded `persona_evolution.considerations` addendum section (`=== PLUGIN CONSIDERATIONS ===`) while leaving the required JSON response contract unchanged.
- **Interacts with**: `runtime_plugin_host.rs` prompt-slot types and merge helpers.

### `capture_persona_snapshot`
- **Does**: Prompts the LLM for current self-description/trait scores/new dimensions and returns a `PersonaSnapshot`.
- **Interacts with**: `agent/mod.rs` snapshot cadence and persistence.

### JSON helpers (`extract_json`, `clean_json_string`, etc.)
- **Does**: Recovers valid JSON from noisy model output (fences/comments/trailing commas/think tags).
- **Interacts with**: Both trajectory parsing and snapshot parsing.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent/mod.rs` | `TrajectoryEngine::new` + `infer_trajectory` signatures and `TrajectoryAnalysis` fields | Renaming/removing methods or analysis fields |
| Persona persistence flow | `capture_persona_snapshot` returns valid `PersonaSnapshot` with IDs/timestamps | Changing snapshot field semantics |

## Notes
- Personality dimensions are intentionally open-ended: guiding principles seed expected keys, while LLM can propose new dimensions.
- HTTP client initialization now uses shared panic-safe construction (`http_client::build_http_client`) for startup portability.
- `normalize_chat_url()` (module-private) strips double-`/v1` paths: if `api_url` already ends with `/v1`, it appends `/chat/completions` directly; if it ends with `/chat/completions` it passes through unchanged; otherwise it appends `/v1/chat/completions`. Both `call_llm` and `capture_persona_snapshot` use it instead of naïve `format!("{}/v1/chat/completions", ...)` to avoid 404 errors when the configured URL already includes the API version prefix.
- Persona trajectory prompts now have one additive plugin slot for evolution-time considerations, which lets runtime plugins influence reflection context without taking ownership of the trajectory JSON schema.
