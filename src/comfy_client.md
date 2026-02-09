# comfy_client.rs

## Purpose
HTTP client for the ComfyUI REST API. Queues image generation workflows, polls for completion, and downloads resulting images. Used by the agent's image generation subsystem.

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
- **Does**: Polls `get_history` every 1 second until the prompt completes or times out; returns the first `ImageInfo` found
- **Rationale**: ComfyUI is async; there is no webhook, so polling is required

### `ComfyUIClient::download_image(image_info)`
- **Does**: GETs `/view?filename=...&subfolder=...&type=...`, writes bytes to `generated_{filename}` in CWD, returns the path
- **Interacts with**: `agent::image_gen` (retrieves the file path for upload)

### `ComfyUIClient::test_connection()`
- **Does**: GETs `/history` to verify ComfyUI is reachable

### `ImageInfo` / `HistoryEntry` / `OutputNode` / `StatusInfo`
- **Does**: Deserialization structs for ComfyUI API responses

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent::image_gen` | `queue_prompt` -> `wait_for_completion` -> `download_image` pipeline | Changing return types or method signatures |
| `comfy_workflow.rs` | `queue_prompt` accepts `serde_json::Value` (the prepared workflow) | Changing the workflow input type |
| ComfyUI server | REST API at `/prompt`, `/history/{id}`, `/view` | ComfyUI API changes would break all methods |

## Notes
- Downloaded images are saved to the current working directory as `generated_{filename}` -- not a configurable output path.
- Polling interval is hardcoded at 1 second.
- No retry logic on transient failures; a single HTTP error aborts the operation.
