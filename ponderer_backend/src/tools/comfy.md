# comfy.rs

## Purpose
Provides media-oriented tools for agentic chat workflows: generating assets through ComfyUI and publishing posts to Graphchan with optional media references/embeds.

## Components

### `GenerateComfyMediaTool`
- **Does**: Executes the configured ComfyUI workflow with prompt + optional input overrides, waits for completion, downloads generated assets, and returns structured JSON (`media` array) with local file paths and media metadata
- **Interacts with**: `config::AgentConfig` (`enable_image_generation`, `workflow_settings`, `comfyui.api_url`), `comfy_workflow::ComfyWorkflow`, `comfy_client::ComfyUIClient`

### `PostToGraphchanTool`
- **Does**: Posts content to a Graphchan thread, optionally attaching media references or data-URI embeds for small files
- **Interacts with**: `config::AgentConfig` (`graphchan_api_url`), Graphchan `POST /threads/{id}/posts` endpoint

### Helper functions
- **Does**: Parse input overrides, apply prompt/seed defaults to workflows, infer media kind/mime type, and format media payloads for chat/UI consumption
- **Interacts with**: `agent/mod.rs` media metadata extraction via returned `ToolOutput::Json`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | Tools are constructible with `new()` and implement `Tool` trait | Renaming/removing tool types |
| `agent/mod.rs` | `generate_comfy_media` / `post_to_graphchan` can return JSON with `media` entries (`path`, `media_kind`, `mime_type`) | Changing media JSON shape |
| `ui/chat.rs` | Media payload can include absolute/relative local paths for rendering | Removing `path` field from media entries |

## Notes
- `generate_comfy_media` loads current config on each execution, so saved settings updates apply without re-registering tools.
- Graphchan media handling is additive: body text is always posted; media can be appended as local references or data URIs.
- Data URI embeds are capped to avoid oversized posts.
