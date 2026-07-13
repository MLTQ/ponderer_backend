# manifest.rs

## Purpose
Defines the canonical plugin identity, capability summary, and schema-driven settings contract shared by backend and frontend. Legacy backend names remain aliases while new code can use `PluginManifest` and `PluginKind`.

## Components

### `PluginManifest`
- **Does**: Describes versioned plugin identity, runtime kind, structured per-tool contracts, requested capabilities, package effect summary, optional static contribution authority, and settings UI metadata. `provided_tools` remains a name-only compatibility projection.
- **Interacts with**: catalogs, `/v1/plugins`, runtime handshakes, and the desktop settings UI.

### `PluginKind` / `BackendPluginKind`
- **Does**: Identifies built-in and subprocess packages while accepting the historical `runtime_process` alias.
- **Interacts with**: package discovery and UI diagnostics.

### Settings manifests
- **Does**: Define the small portable field vocabulary used to build generic plugin settings forms.
- **Interacts with**: schema loaders and `ui/plugin_settings_form.rs`.

### `RuntimeProcessPluginPackageManifest`
- **Does**: Flattens the canonical `PluginManifest` together with subprocess launch/settings-file fields and an optional JSON tool-contract file used by `plugin.toml` packages.
- **Interacts with**: `runtime_process_plugin.rs` discovery and compatibility parsing.
- **Rationale**: Keeps package identity/version/capability fields in the same DTO used by the API instead of maintaining a second private manifest shape.

### `PluginContributionManifest`
- **Does**: Statically authorizes the exact lifecycle hooks, prompt slots, and
  event-polling capability a runtime handshake may expose.
- **Rationale**: An installed-but-disabled package can be inspected and approved
  without executing its code; runtime registration must match that package
  authority exactly.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Legacy API clients | Absent manifest/protocol/capability fields decode as v1/empty | Removing defaults |
| Legacy API imports | `BackendPluginManifest` and `BackendPluginKind` remain valid aliases | Removing aliases |
| Settings UI | Field kinds and JSON default values retain their meanings | Changing serialized field-kind names |
| Existing runtime packages | `plugin_type = "runtime_process"` flattens into `PluginManifest.kind` and missing versions mean v1 | Removing aliases/defaults |
