# comfy.rs

## Purpose
Provides the media-generation tool for agentic chat workflows by executing configured ComfyUI workflows and returning renderable local asset metadata. Graphchan publishing now belongs to the external Graphchan-Orb runtime plugin.

## Components

### `GenerateComfyMediaTool`
- **Does**: Executes the configured ComfyUI workflow with prompt + optional input overrides, waits for completion, downloads generated assets, and returns structured JSON (`media` array) with local file paths and media metadata
- **Interacts with**: `config::AgentConfig` (`enable_image_generation`, `workflow_settings`, `comfyui.api_url`), `comfy_workflow::ComfyWorkflow`, `comfy_client::ComfyUIClient`

### Helper functions
- **Does**: Parse input overrides, apply prompt/seed defaults to workflows, infer MIME types, and format generated asset payloads for chat/UI consumption
- **Interacts with**: `agent/mod.rs` media metadata extraction via returned `ToolOutput::Json`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | Tools are constructible with `new()` and implement `Tool` trait | Renaming/removing tool types |
| `agent/mod.rs` | `generate_comfy_media` returns JSON with `media` entries (`path`, `media_kind`, `mime_type`) | Changing media JSON shape |
| `ui/chat.rs` | Media payload can include absolute/relative local paths for rendering | Removing `path` field from media entries |

## Notes
- `generate_comfy_media` loads current config on each execution, so saved settings updates apply without re-registering tools.
- Media kind comes from `GeneratedAssetInfo`; filename extension is used only to supply the MIME type.
- The obsolete media-kind inference test was removed with the built-in Graphchan posting tool rather than restoring an unused helper after the OrbWeaver extraction.
