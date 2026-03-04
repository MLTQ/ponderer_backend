# workflow_plugin.rs

## Purpose
Exposes installed workflow plugin bundles as one generic tool, `run_workflow_plugin`, using the built-in ComfyUI transport layer. This lets optional workflow-driven features (for example TTS) execute without bespoke Rust per plugin.

## Components

### `RunWorkflowPluginTool`
- **Does**: Lists installed workflow plugin ids in its JSON schema, loads the selected bundle at runtime, applies runtime inputs plus saved plugin settings, submits the resulting workflow to ComfyUI, and returns generated asset metadata.
- **Interacts with**: `workflow_plugin.rs`, `config::AgentConfig`, and `comfy_client.rs`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Agent tool loop | Tool name stays `run_workflow_plugin` and accepts `{plugin_id, inputs?, timeout_secs?, max_assets?}` | Renaming the tool or required parameters |
| Installed workflow plugins | Plugin ids exposed in the schema are executable if the bundle validated at startup | Changing lookup or execution semantics |

## Notes
- This is intentionally one generic tool instead of one generated tool per bundle, which keeps tool registration simple while still advertising installed plugin ids to the model.
