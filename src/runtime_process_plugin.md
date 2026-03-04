# runtime_process_plugin.rs

## Purpose
Discovers filesystem-installed runtime-process plugin bundles from the shared `plugins/` directory and turns their `plugin.toml` plus optional settings schema into backend manifests and launch specs. This is the static bundle loader for subprocess-backed plugins such as a future qwen3-TTS service.

## Components

### `RuntimeProcessPluginCatalog`
- **Does**: Ensures `PONDERER_PLUGIN_DIR` (or `./plugins`) exists, then scans it for directories containing `plugin.toml`, filters to `plugin_type = "runtime_process"`, and loads each bundle into an in-memory catalog.
- **Interacts with**: `runtime.rs` bootstrap and `runtime_plugin_host.rs`.

### `plugin_dir_path` / `ensure_plugin_dir`
- **Does**: Resolve the shared plugin directory path (next to the executable/config by default, or `PONDERER_PLUGIN_DIR` when overridden) and create it on demand so fresh portable installs always have a local `plugins/` folder before discovery runs.
- **Interacts with**: `workflow_plugin.rs` and runtime bootstrap.

### `RuntimeProcessPluginBundle`
- **Does**: Holds the static plugin manifest plus resolved launch command/working directory, and computes whether the plugin should be enabled from `AgentConfig.plugin_settings`.
- **Interacts with**: `runtime_plugin_host.rs` startup and config reload logic.

### `RuntimeProcessLaunchSpec`
- **Does**: Stores the resolved subprocess command line and working directory used to launch the plugin.
- **Interacts with**: `runtime_plugin_host.rs` process spawning.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime.rs` | Discovery creates the shared `plugins/` directory if it is missing and skips non-runtime bundles silently | Stopping directory auto-creation or failing hard on mixed plugin types |
| `runtime_plugin_host.rs` | `RuntimeProcessPluginBundle::manifest_with_tools` can merge handshake-discovered tool names into the static manifest | Removing manifest cloning helpers |
| Future plugin bundles | `plugin.toml` uses `plugin_type = "runtime_process"` and `command = ["..."]` | Renaming those required fields |

## Notes
- Runtime bundles are visible in `/v1/plugins` even before they are enabled; tool names remain empty until a successful handshake populates them.
- The `enabled` setting is convention-based: if the plugin schema defines an `enabled` field with a default, that default controls startup when the user has not explicitly configured the plugin.
- If the plugin path exists but is a file instead of a directory, discovery fails fast so startup does not silently ignore a broken portable install.
- The default plugin location now matches config/database portability: it lives beside `ponderer_config.toml` and `ponderer_memory.db`, not the shell working directory.
