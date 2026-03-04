# plugin.rs

## Purpose
Defines the backend plugin contract plus the shared manifest/schema types used to describe plugin-driven settings tabs. This is the backend/frontend handshake for both built-in integrations and filesystem-discovered workflow bundles.

## Components

### `BackendPluginManifest`
- **Does**: Describes plugin identity, plugin kind, declared capabilities (`provided_tools`, `provided_skills`), optional settings-tab metadata, and optional inline settings schema for frontend settings composition.
- **Interacts with**: `runtime.rs` plugin loading and runtime diagnostics/introspection.

### `PluginSettingsTabManifest`
- **Does**: Declares the frontend-visible settings tab (`id`, `title`, `order`) exposed by a plugin.
- **Interacts with**: frontend plugin discovery via `/v1/plugins` and `ui/settings.rs` tab rendering.

### `BackendPluginKind`
- **Does**: Distinguishes built-in code plugins, data-only workflow bundles, and subprocess runtime bundles.
- **Interacts with**: runtime discovery and future UI affordances.

### Settings schema manifests
- **Does**: `PluginSettingsSchemaManifest`, `PluginSettingsFieldManifest`, and related enums/options describe a small vocabulary of form fields (`boolean`, `text`, `multiline`, `number`, `select`, `path`, `secret`) that the frontend can render generically.
- **Interacts with**: `workflow_plugin.rs` bundle loading and `ui/plugin_settings_form.rs`.

### `BackendPlugin`
- **Does**: Trait for plugin hooks: provide manifest, optionally register tools, optionally build skill instances.
- **Interacts with**: `runtime.rs` (`BackendRuntimeBuilder`) and backend extension crates.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime.rs` | Plugin trait remains object-safe and callable at bootstrap | Changing trait method signatures |
| External backend extensions | `register_tools` receives `ToolRegistry` + config, `build_skills` can return `Vec<Box<dyn Skill>>` | Removing hooks or changing ownership requirements |

## Notes
- Default trait implementations are no-op so plugins can provide only tools or only skills.
- Plugin loading order is deterministic: built-ins first, then user-supplied plugins.
- `settings_tab` and `settings_schema` are declarative metadata only; plugins do not render UI directly.
