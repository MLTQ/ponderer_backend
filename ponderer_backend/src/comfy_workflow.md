# comfy_workflow.rs

## Purpose
Parses and manages ComfyUI workflow graphs. Supports importing workflows from PNG metadata or JSON files, auto-detecting controllable nodes (prompts, samplers, latent sizes), and preparing workflows for execution with agent-provided inputs.

## Components

### `ComfyWorkflow`
- **Does**: Holds a parsed workflow graph with metadata: raw JSON, controllable nodes map, output node ID, and optional preview image path
- **Interacts with**: `comfy_client::ComfyUIClient::queue_prompt` (via `prepare_for_execution` output)

### `ComfyWorkflow::from_png(path)`
- **Does**: Reads a PNG file, extracts ComfyUI workflow from `tEXt` chunks (keywords "workflow" or "prompt"), and parses it
- **Rationale**: ComfyUI embeds workflow metadata in generated PNGs, enabling round-trip workflow sharing

### `ComfyWorkflow::from_json_file(path)`
- **Does**: Reads and parses a plain JSON workflow file

### `ComfyWorkflow::prepare_for_execution(inputs)`
- **Does**: Clones the workflow JSON and applies agent-provided input values to `agent_modifiable` fields in controllable nodes
- **Interacts with**: `agent::image_gen` (passes prompt text and seed overrides)

### `detect_controllable_nodes(workflow)`
- **Does**: Scans workflow JSON for known node types (`CLIPTextEncode`, `KSampler`, `KSamplerAdvanced`, `EmptyLatentImage`) and extracts their controllable inputs
- **Rationale**: Auto-discovery means users can import arbitrary workflows without manual annotation

### `find_output_node(workflow)`
- **Does**: Finds the first `SaveImage` or `PreviewImage` node ID in the workflow

### `extract_comfy_workflow_from_png(png_bytes)`
- **Does**: Manual PNG chunk parser that reads `tEXt` chunks looking for workflow/prompt data

### `InputType`
- **Does**: Enum of supported input types: `Text`, `Int`, `Float`, `Bool`, `Seed`

### `ControllableInput`
- **Does**: Describes a single controllable parameter with name, type, default, `agent_modifiable` flag, and description
- **Rationale**: Only `CLIPTextEncode.text` and `KSampler.seed` default to `agent_modifiable: true`; others are operator-only

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent::image_gen` | `ComfyWorkflow::from_json_file` or `from_png` returns a valid workflow; `prepare_for_execution` returns `serde_json::Value` | Changing `prepare_for_execution` signature |
| `comfy_client.rs` | Output of `prepare_for_execution` is a valid ComfyUI workflow JSON | Producing malformed workflow JSON |
| PNG files | Standard PNG chunk format with `tEXt` chunks containing UTF-8 JSON | Non-standard PNG encoding |

## Notes
- The PNG parser is manual (no dependency on a PNG library) -- it walks chunks byte-by-byte.
- Only three node class types are recognized for auto-detection. Custom/ControlNet/LoRA nodes are ignored.
- `agent_modifiable` defaults: only prompt text and seed are agent-modifiable; steps, CFG, dimensions, and denoise are locked.
