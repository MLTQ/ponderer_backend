# runtime_plugin.rs

## Purpose
Defines the generic tool proxy used for subprocess runtime plugins. Each declared plugin tool becomes one `Tool` implementation that forwards execution back through `RuntimePluginHost`.

## Components

### `RuntimePluginToolProxy`
- **Does**: Wraps one runtime plugin tool manifest (`name`, description, JSON schema, approval policy, category) and delegates execution to `RuntimePluginHost::invoke_tool`.
- **Interacts with**: `runtime_plugin_host.rs` and `tools/mod.rs` registration.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime_plugin_host.rs` | Proxy preserves the manifest-declared tool name and schema exactly | Renaming tool names or mutating schemas |
| `tools/mod.rs` | Proxy implements the normal `Tool` trait and can be registered like any built-in tool | Changing trait conformance |

## Notes
- The proxy intentionally does not expose plugin internals to the agent loop; it is only a thin bridge from tool-calling to the runtime plugin host.
