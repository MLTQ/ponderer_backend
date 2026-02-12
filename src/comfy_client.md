# comfy_client.rs

## Purpose
HTTP client for the ComfyUI REST API. Queues workflows, polls for completion, and downloads generated assets. Supports both legacy image-only flow and generalized media outputs (image/audio/video/file).

## Components

### `ComfyUIClient`
- **Does**: Wraps `reqwest::Client` with a ComfyUI API URL; provides async methods for the workflow lifecycle
- **Interacts with**: `agent::image_gen` (called to generate images), `config::ComfyUIConfig` (URL comes from config)

### `ComfyUIClient::queue_prompt(workflow)`
- **Does**: POSTs a workflow JSON to `/prompt`, returns the `prompt_id` string
- **Interacts with**: `comfy_workflow::ComfyWorkflow::prepare_for_execution` (produces the workflow JSON)

### `ComfyUIClient::get_history(prompt_id)`
- **Does**: GETs `/history/{prompt_id}`, returns `Option<HistoryEntry>` with output nodes and status
- **Interacts with**: `wait_for_completion` (polls this internally)

### `ComfyUIClient::wait_for_completion(prompt_id, timeout_secs)`
- **Does**: Polls `get_history` every 1 second until the prompt completes or times out; returns the first image output (`ImageInfo`) for backward-compatible callers
- **Rationale**: ComfyUI is async; there is no webhook, so polling is required

### `ComfyUIClient::wait_for_completion_assets(prompt_id, timeout_secs)`
- **Does**: Polls until complete, then returns all discovered outputs as `GeneratedAssetInfo` with `media_kind`, output node ID, and download parameters
- **Interacts with**: `tools/comfy.rs` (`generate_comfy_media` tool)

### `ComfyUIClient::download_image(image_info)`
- **Does**: Legacy wrapper around `download_asset`, preserving the image-only method signature
- **Interacts with**: `agent::image_gen` (retrieves the file path for upload)

### `ComfyUIClient::download_asset(asset)`
- **Does**: GETs `/view?filename=...&subfolder=...&type=...`, writes bytes to a unique `generated_*` path in CWD, returns the path
- **Interacts with**: `tools/comfy.rs` media generation output pipeline

### `ComfyUIClient::test_connection()`
- **Does**: GETs `/history` to verify ComfyUI is reachable

### `ComfyOutputFile` / `GeneratedAssetInfo` / `HistoryEntry` / `OutputNode` / `StatusInfo`
- **Does**: Deserialization structs for ComfyUI API responses

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent::image_gen` | `queue_prompt` -> `wait_for_completion` -> `download_image` pipeline | Changing return types or method signatures |
| `tools/comfy.rs` | `wait_for_completion_assets` returns at least one downloadable asset or error | Breaking `GeneratedAssetInfo` fields or asset extraction behavior |
| `comfy_workflow.rs` | `queue_prompt` accepts `serde_json::Value` (the prepared workflow) | Changing the workflow input type |
| ComfyUI server | REST API at `/prompt`, `/history/{id}`, `/view` | ComfyUI API changes would break all methods |

## Notes
- Downloaded assets are saved to the current working directory as unique `generated_*` files -- not a configurable output path.
- Polling interval is hardcoded at 1 second.
- No retry logic on transient failures; a single HTTP error aborts the operation.
