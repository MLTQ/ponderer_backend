# vision.rs

## Purpose
Provides vision/media companion tools for the agentic loop: local image evaluation with a vision-capable model, media publication into private chat metadata, and optional screenshot capture (explicitly gated by settings).

## Components

### `EvaluateLocalImageTool`
- **Does**: Reads a local image file, calls the configured model's vision path, and returns structured evaluation JSON plus a media payload entry for chat rendering
- **Interacts with**: `llm_client::LlmClient::evaluate_image`, `config::AgentConfig` (`llm_api_url`, `llm_model`, `llm_api_key`)

### `PublishMediaToChatTool`
- **Does**: Validates local file paths and emits a `media` array in `ToolOutput::Json` so private chat can display image/audio/video/file cards
- **Interacts with**: `agent/mod.rs` media extraction and `ui/chat.rs` media rendering

### `CaptureScreenTool`
- **Does**: Captures a desktop screenshot to disk and returns it as media metadata
- **Interacts with**: `config::AgentConfig.enable_screen_capture_in_loop` opt-in gate, OS screenshot commands (`screencapture`, `gnome-screenshot`/`scrot`/`grim`, `powershell`)

### Helper functions
- **Does**: Path normalization, MIME/media kind inference, API URL normalization, and command execution wrappers

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | Tool types are constructible via `new()` and registered under stable names | Renaming tool names |
| `agent/mod.rs` | Tool JSON includes `media` entries with `path`, `media_kind`, `mime_type`, `source` | Changing media payload shape |
| `ui/settings.rs` | Screenshot tool honors `enable_screen_capture_in_loop` as hard gate | Removing/renaming gate field |

## Notes
- `evaluate_local_image` currently uses the existing inline-base64 vision request style from `llm_client`; provider support can vary.
- `capture_screen` is intentionally opt-in and defaults to disabled for privacy.
