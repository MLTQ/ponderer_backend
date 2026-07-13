# config.rs

## Purpose
Defines all configuration for the Ponderer agent, including LLM connection, identity, polling behavior, agentic loop iteration controls, private-chat execution mode (`agentic` vs `direct`) and autonomous turn budgets, deterministic loop-breaker heat controls, living-loop toggles, heartbeat scheduling, per-loop tool capability profiles, plugin-owned settings blobs, screen/camera privacy gates, character card fields, and self-reflection settings. Supports loading from TOML files with environment variable fallback.

## Components

### `AgentConfig`
- **Does**: Top-level config struct holding core runtime settings plus namespaced `plugin_settings`; integration endpoints and model-specific options belong to their plugin schemas.
- **Interacts with**: `main.rs` (loaded at startup), `agent::Agent` (drives behavior), `ui::app::AgentApp` (display/edit)

### `AgentConfig::load()`
- **Does**: Loads config by scanning both executable-directory and current-working-directory TOML files, preferring the most recently modified among `ponderer_config.toml` and `agent_config.toml`, then falls back to environment variables
- **Rationale**: Supports portable binary deployment while still tolerating launch-context differences and legacy config filenames

### `AgentConfig::save()`
- **Does**: Serializes config to `ponderer_config.toml` via `toml::to_string_pretty`
- **Interacts with**: UI settings panel for persisting changes

### `AgentConfig::from_env()`
- **Does**: Populates core config from environment variables (`LLM_API_URL`, `LLM_MODEL`, `LLM_API_KEY`, agent-loop controls, memory controls, sensor gates, and `AGENT_NAME`). Plugin-specific environment/config migration belongs to each package.
- **Rationale**: Legacy support for env-var-only configuration

### `RespondTo`
- **Does**: Controls response behavior (`response_type`: "all" or "selective") with optional separate `decision_model`
- **Interacts with**: `agent::reasoning` for deciding whether to reply

### `CapabilityProfileConfig` / `CapabilityProfileOverride`
- **Does**: Declares optional per-loop tool policy overrides (`private_chat`, `scheduled`, `background`, `self_directed`, `skill_events`, `heartbeat`, `ambient`, `dream`) for allowlist/denylist replacement
- **Interacts with**: `agent::capability_profiles` policy resolver used by loop-level `ToolContext` construction

### `normalize_private_chat_mode`
- **Does**: Canonicalizes configured/private-chat mode values to `agentic` or `direct` with safe fallback to `agentic`.
- **Interacts with**: config load/env parsing, agent runtime mode selection, and the `private_chat_mode` tool.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | `AgentConfig::load()` returns valid config | Changing `load()` return type |
| `agent::Agent` | Core loop, LLM, memory, reflection, capability-profile, and local-sensor fields retain their meanings | Renaming/removing live core fields |
| `tools/vision.rs` | `enable_screen_capture_in_loop` and `enable_camera_capture_tool` must be present and default false | Removing/renaming these privacy gate fields |
| Plugin packages | `plugin_settings[plugin_id]` stores arbitrary JSON objects keyed by plugin id | Removing or changing `plugin_settings` serialization |
| TOML file | Serde field names and aliases (`agent_name` -> `username`, `check_interval_seconds` -> `poll_interval_secs`) | Removing serde aliases breaks existing config files |

## Notes
- Config save path targets an executable-root directory; for Cargo `target/*/deps` runs, the `deps` parent is used so state lives in `target/{debug|release}` instead of hash subfolders.
- Load discovery scans that executable-root directory plus working directory candidates and picks the newest valid file.
- When both `ponderer_config.toml` and `agent_config.toml` exist, the newest file wins to avoid stale-file precedence surprises.
- `database_path` is normalized to an executable-directory absolute runtime path on load, and converted back to a portable relative path when saving TOML.
- `plugin_settings` is intentionally schema-agnostic at the config layer; validation lives in plugin manifests and runtime bundle loaders.
- Default LLM is `llama3.2` at `localhost:11434` (Ollama).
- Living-loop continuity is active by default: ambient orientation, journal/concerns, and bounded Dream are enabled for new configs and for older config files that omit those fields. Explicit `false` values remain respected.
- Private sensors and formal persona evolution remain opt-in: screen/camera access and `enable_self_reflection` still default false.
- Heartbeat defaults: disabled, 30-minute interval, checklist path `HEARTBEAT.md`.
- Agentic loop defaults: max 10 tool-calling iterations per turn, with optional config to disable the limit entirely.
- Private-chat mode default is `agentic`; `direct` is a single-turn mode that still permits tool calls and now uses the same tool-iteration setting path as normal chat.
- Private-chat turn defaults: model-decided continuation (`disable_chat_turn_limit=true`, `disable_background_subtask_turn_limit=true`). Optional safety caps remain configurable at 4 foreground turns and 8 background turns when re-enabled.
- Loop-breaker defaults: `loop_heat_threshold=20`, `loop_similarity_threshold=0.92`, `loop_signature_window=24`, `loop_heat_cooldown=1`.
- Memory evolution defaults: disabled, 24-hour interval, built-in replay trace set.
- Capability profile overrides default to empty, so loop policies fall back to code-defined defaults.
- Unattended scheduled, background, and self-directed profiles are independently configurable and always resolve to autonomous execution semantics.
- Screen and camera capture tools default to disabled and must be explicitly enabled in settings.
- Integration-specific legacy TOML keys are ignored during deserialization; their replacements live in plugin settings schemas.
