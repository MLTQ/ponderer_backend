# workflow_plugin.rs

## Purpose
Loads filesystem-backed workflow plugin bundles that target the built-in ComfyUI engine. A bundle contributes declarative settings metadata, a raw workflow JSON file, and explicit node-input bindings so optional media features can be installed without recompiling the frontend.

## Components

### `WorkflowPluginCatalog`
- **Does**: Reuses the shared plugin-directory bootstrap (creating `PONDERER_PLUGIN_DIR` or an executable-local `plugins/` folder if needed), then discovers `plugin_type = "comfy_workflow"` bundles, keeps them indexed by plugin id, and exposes plugin manifests for `/v1/plugins`.
- **Interacts with**: `runtime.rs` bootstrap, `runtime_process_plugin.rs`, and `tools/workflow_plugin.rs`.

### `WorkflowPluginBundle`
- **Does**: Holds one loaded plugin bundle (manifest, workflow JSON, bindings) and can prepare an executable workflow by applying saved plugin settings plus runtime inputs.
- **Interacts with**: `config::AgentConfig` (`plugin_settings`) and `ComfyUIClient` callers.

### `WorkflowPluginBindings` / `WorkflowPluginBinding`
- **Does**: Declares explicit mappings from plugin settings or runtime inputs to Comfy workflow node inputs.
- **Interacts with**: bundle validation at load time and execution-time workflow mutation.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime.rs` | `discover()` ensures the shared plugin directory exists, then returns an empty catalog when no workflow bundles are installed | Stopping directory auto-creation or making empty directories fatal |
| `tools/workflow_plugin.rs` | `prepare_workflow()` applies saved settings + runtime inputs to valid workflow slots | Changing binding semantics or return type |
| Plugin bundle authors | `plugin.toml`, `settings.schema.json`, `bindings.json`, and `workflow.json` are validated consistently | Renaming required manifest fields or binding keys |

## Notes
- This intentionally avoids dynamic code loading; plugin bundles are data-only and ride on the existing ComfyUI transport layer.
- Binding validation is strict so miswired bundles fail at load time, not mid-generation.
- Mixed plugin directories are supported now: non-Comfy bundles in `plugins/` are ignored by this loader instead of being logged as errors.
