# runtime_plugin.rs

## Purpose
Defines the generic tool proxy used for subprocess runtime plugins. Each declared plugin tool becomes one `Tool` implementation that forwards execution back through `RuntimePluginHost`.

## Components

### `RuntimePluginToolProxy`
- **Does**: Wraps one runtime plugin tool manifest (`name`, description, JSON schema, approval hint, semantic effects, category), exposes host-resolved policy metadata, binds authorization identity to plugin ID/version/process generation, and delegates execution plus the host-authored `ToolContext` to `RuntimePluginHost::invoke_tool`.
- **Interacts with**: `runtime_plugin_host.rs`, `tools/effect_policy.rs`, and `tools/mod.rs` registration/execution.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime_plugin_host.rs` | Proxy preserves the manifest-declared tool name and schema exactly | Renaming tool names or mutating schemas |
| `tools/mod.rs` | Proxy implements the normal `Tool` trait and can be registered like any built-in tool | Changing trait conformance |
| Host policy | Declared effects are preserved, while `requires_approval()` reports at least the host minimum | Returning the raw plugin boolean for a dangerous effect |
| Session approval | `authorization_provider()` distinguishes plugin ID, package version, and supervised process generation | Reusing a provider identity across plugin generations |

## Notes
- The proxy intentionally does not expose plugin internals to the agent loop; it is only a thin bridge from tool-calling to the runtime plugin host.
- Conversation identity, username, autonomy, working directory, invocation time,
  and deadline cross the bridge from trusted host context rather than plugin
  arguments.
- `effect_policy()` resolves from the raw manifest flag and effects directly to avoid recursive or plugin-controlled weakening.
- Registry registration adds a second monotonic generation, so both plugin restarts and any direct tool replacement invalidate prior session approval.
- Tests verify that `external.publish` remains approval- and quota-governed even when the plugin sends `requires_approval = false`.
