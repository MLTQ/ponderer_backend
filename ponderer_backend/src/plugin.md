# plugin.rs

## Purpose
Defines the backend plugin contract so external modules can register tools and skills during backend bootstrap without any frontend coupling.

## Components

### `BackendPluginManifest`
- **Does**: Describes plugin identity and declared capabilities (`provided_tools`, `provided_skills`).
- **Interacts with**: `runtime.rs` plugin loading and runtime diagnostics/introspection.

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
