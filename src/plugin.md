# plugin.rs

## Purpose
Provides compatibility re-exports for plugin manifest DTOs. Canonical manifest,
settings, and wire definitions live in `plugin_contract/`; executable extensions
use protocol-v1 subprocess packages rather than Rust trait objects.

## Components

### `BackendPluginManifest` / `PluginManifest`
- **Does**: Re-exports canonical manifest DTOs under both current and historical names.
- **Interacts with**: runtime discovery, API responses, and older internal imports.

### `PluginSettingsTabManifest`
- **Does**: Declares the frontend-visible settings tab (`id`, `title`, `order`) exposed by a plugin.
- **Interacts with**: frontend plugin discovery via `/v1/plugins` and `ui/settings.rs` tab rendering.

### `BackendPluginKind` / `PluginKind`
- **Does**: Re-exports the canonical package-kind enum under both names.
- **Interacts with**: runtime discovery and UI affordances.

### Settings schema manifests
- **Does**: Re-export the canonical schema DTOs from `plugin_contract::manifest`.
- **Interacts with**: legacy import paths, package discovery, and the generic settings UI.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Legacy imports | Plugin DTO names remain available under `crate::plugin::*` | Removing compatibility re-exports |
| Runtime host and API | Re-exported DTOs are the exact canonical contract types | Wrapping or forking the canonical DTOs |

## Notes
- `settings_tab` and `settings_schema` are declarative metadata only; packages do not render UI directly.
- Core runtime, host, and API code imports canonical DTO names from
  `crate::plugin_contract`; this module exists only for downstream source
  compatibility.
- The former `BackendPlugin` trait was intentionally removed. Keeping a second in-process extension path would bypass package validation, effect declarations, lifecycle supervision, and durable event receipts.
