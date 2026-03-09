# runtime_plugin_host.rs

## Purpose
Hosts subprocess-backed runtime plugins. It owns the JSON-RPC-over-stdio transport, lifecycle/config dispatch, prompt-slot contribution collection, and the generic tool-bridge contract that lets external plugin processes participate in the normal Ponderer loop.

## Components

### RPC envelope types
- **Does**: `RuntimePluginRpcRequest`, `RuntimePluginRpcResponse`, and `RuntimePluginRpcError` define the wire format for stdio/local RPC with runtime plugins.
- **Interacts with**: future process-backed plugin transports.

### `RuntimePluginHandshake` / `RuntimePluginCapabilities`
- **Does**: Describe plugin identity and declared capabilities (tool names, event hooks, prompt slots), while `RuntimePluginToolManifest` describes full tool schemas for proxy registration.
- **Interacts with**: runtime plugin startup and tool exposure.

### `RuntimePluginLifecycleEvent`
- **Does**: Enumerates typed lifecycle events that Ponderer may emit to plugins (`persona_evolved`, `orientation_updated`, `message_finalized`, `reflection_completed`, `settings_changed`).
- **Interacts with**: `agent/mod.rs` lifecycle boundaries.

### Prompt contribution types
- **Does**: `PromptContributionSlot`, `PromptContributionKind`, `PromptContribution`, `PromptContributionContext`, `RuntimePluginPromptQuery`, and `RuntimePluginPromptResponse` model bounded plugin prompt injections tied to named slots.
- **Interacts with**: prompt builders across `agent/*`.

### `RuntimePluginHost`
- **Does**: Discovers enabled runtime bundles, launches subprocesses, performs handshake/configure calls, registers proxy tools, dispatches lifecycle events, collects prompt contributions, and forwards tool invocations.
- **Interacts with**: `runtime.rs`, `runtime_process_plugin.rs`, `tools/runtime_plugin.rs`, and `agent/mod.rs`.
- **Rationale**: Caches the last seen `ToolRegistry` so transport failures can deactivate a dead plugin and deregister stale proxy tools instead of repeatedly surfacing broken-pipe errors.

### Runtime tool result types
- **Does**: `RuntimePluginToolInvocation`, `RuntimePluginToolResult`, and related enums define the narrow bridge between subprocess RPC and `ToolOutput`.
- **Interacts with**: `tools/runtime_plugin.rs` and future external plugin implementations.

### `render_prompt_slot_addendum`
- **Does**: Filters prompt contributions to one slot, sorts deterministically, clamps both per-block and total slot budgets, and renders auditable `[Plugin: …]` blocks.
- **Interacts with**: prompt assembly helpers in `agent/mod.rs` and future prompt builders.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Future runtime plugins | RPC envelopes stay `{id, method, params}` and `{id, ok, result?, error?}`; method names (`plugin.handshake`, `plugin.configure`, `plugin.handle_event`, `plugin.get_prompt_contributions`, `plugin.invoke_tool`) remain stable | Renaming core RPC fields or method names |
| Prompt builders | `render_prompt_slot_addendum` remains additive-only, bounded, and deterministic | Changing ordering, labels, or cap semantics |
| `agent/mod.rs` | `RuntimePluginHost::dispatch_event`, `collect_prompt_contributions`, and config reapply are callable even when no plugins are installed | Making no-plugin calls fail |

## Notes
- Runtime plugin failures are isolated: startup, event, or prompt-call failures are logged per plugin and do not fail the whole agent loop.
- Tool proxies are only registered for plugins that return full `tools` manifests in the handshake; legacy `capabilities.tools` names alone are used for metadata only.
- The stdio transport now tolerates a bounded amount of non-JSON stdout noise before the first valid RPC response, which helps when third-party runtimes emit banners or environment chatter during startup.
- Runtime plugins should be initialized from a long-lived Tokio runtime (the dedicated agent loop runtime) so plugin stdio/process resources are polled on the same runtime for their full lifetime.
- Transport-layer plugin failures (broken pipe, closed stdout, process exit) now deactivate that plugin instance immediately, preventing stale stdio handles from producing repeated broken-pipe tool errors.
- Plugin subprocess `stderr` is now piped into structured tracing logs (`target=runtime_plugin_stderr`) instead of being discarded, which makes model/runtime failures diagnosable without changing plugin transport behavior.
