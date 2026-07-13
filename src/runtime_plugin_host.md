# runtime_plugin_host.rs

## Purpose
Hosts subprocess-backed runtime plugins. It owns the JSON-RPC-over-stdio transport, lifecycle/config dispatch, prompt-slot contribution collection, and the generic tool-bridge contract that lets external plugin processes participate in the normal Ponderer loop.

## Components

### RPC envelope types
- **Does**: Compatibility-reexports the canonical versioned RPC types from `plugin_contract`; the host writes and validates their protocol revision.
- **Interacts with**: process-backed plugin transports and SDKs.

### `RuntimePluginHandshake` / `RuntimePluginCapabilities`
- **Does**: Describe plugin identity and requested capabilities, while `RuntimePluginToolManifest` describes full tool schemas/effects. Startup negotiates protocol and validates runtime ID, version, tool schemas/effects, hooks, prompt slots, polling, and requested authority against static package declarations before registration.
- **Interacts with**: runtime plugin startup and tool exposure.

### `RuntimePluginLifecycleEvent`
- **Does**: Enumerates typed lifecycle events that Ponderer may emit to plugins (`persona_evolved`, `orientation_updated`, `message_finalized`, `reflection_completed`, `settings_changed`).
- **Interacts with**: `agent/mod.rs` lifecycle boundaries.

### Prompt contribution types
- **Does**: `PromptContributionSlot`, `PromptContributionKind`, `PromptContribution`, `PromptContributionContext`, `RuntimePluginPromptQuery`, and `RuntimePluginPromptResponse` model bounded plugin prompt injections tied to named slots.
- **Interacts with**: prompt builders across `agent/*`.

### `RuntimePluginHost`
- **Does**: Reconciles refreshable package discovery and desired config with generation-safe process lifecycle, launches/configures subprocesses, restores and persists namespaced state, registers proxy tools, durably dispatches events, collects prompt contributions, forwards scoped tool invocations, and applies host-neutral media compatibility defaults from each plugin's own settings.
- **Interacts with**: `runtime.rs`, `runtime_process_plugin.rs`, `plugin_lifecycle.rs`, `plugin_restart_policy.rs`, `tools/runtime_plugin.rs`, and `agent/mod.rs`.
- **Rationale**: Transport failures deregister stale tools and enter supervised backoff/circuit recovery rather than leaving plugins permanently dead or repeatedly surfacing broken pipes.

### `RuntimePluginHost::statuses` / `manifests`
- **Does**: Projects live desired/actual lifecycle, process/protocol metadata, retry diagnostics, and handshake-discovered tools over the current catalog snapshot.
- **Interacts with**: `PluginRuntimeStatus` and future API status surfaces.

### Desired-state reconciliation
- **Does**: Atomically refreshes discovery, notices exited processes, handles package replacement/removal, performs only required starts/stops/config changes, and records transient versus terminal startup failures.
- **Interacts with**: The pause-independent control task in `runtime.rs`.
- **Rationale**: Successfully configured settings suppress duplicate RPCs. A
  failed reconfiguration stops that generation; supervised startup then restores
  durable state and either accepts the input or records a bounded startup
  failure/circuit until the package or settings change.

### Runtime RPC timeout policy
- **Does**: Bounds the entire stdio transaction for every RPC, including request writes, newline write, flush, and response reads.
- **Policy**: Prompt contributions use 250ms, normal control/event/poll RPCs use 10s, and potentially expensive tool invocations use 300s.
- **Rationale**: Keeps latency-sensitive prompt assembly responsive while allowing bounded media/browser tools to run substantially longer than control-plane calls.

### Runtime tool result types
- **Does**: `RuntimePluginToolInvocation`, `RuntimePluginToolResult`, and related enums define the narrow bridge between subprocess RPC and `ToolOutput`.
- **Interacts with**: `tools/runtime_plugin.rs` and future external plugin implementations.

### Durable state and event delivery
- **Does**: Restores each plugin's host-owned state during configure, applies
  schema-versioned mutation batches atomically after callbacks, records polled observations
  before cognition, and acknowledges receipt-bound batches only after the agent
  accepts them.
- **Poll checkpoint ordering**: Poll state mutations are committed only after
  every event in that response has been appended successfully. A partial append
  leaves the durable cursor unchanged and stops the process. Protocol v1 uses
  restart plus configure-from-durable-state as its callback rollback boundary,
  so the replacement process can replay and deduplicate the complete remote
  batch.
- **Replay**: Lifecycle subscriptions replay their unacknowledged ledger rows at
  process startup and require the plugin to acknowledge the exact event ID.
  Each callback gets its own receipt boundary; its host-owned state mutations
  and receipt advancement share one transaction. Delivery is at-least-once, so
  plugin-side effects still need to deduplicate by the supplied ledger event ID.
- **Settings privacy**: `plugin.configure` is the authoritative, plugin-scoped
  settings delivery. `settings_changed` is sent directly only when that same
  plugin declares the hook in its validated static/runtime contribution
  contract; settings payloads never enter the shared lifecycle ledger.
- **Interacts with**: `database/plugins.rs`, `plugin_event_ledger.rs`, and
  `agent/mod.rs`.

### `render_prompt_slot_addendum`
- **Does**: Filters prompt contributions to one slot, sorts deterministically, clamps both per-block and total slot budgets, and renders auditable `[Plugin: …]` blocks.
- **Interacts with**: prompt assembly helpers in `agent/mod.rs` and future prompt builders.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Future runtime plugins | RPC envelopes retain legacy fields plus defaultable `protocol_version`; package authority bounds handshake tools/capabilities/effects; method names remain stable | Renaming RPC fields/methods or allowing runtime authority to exceed a nonempty package declaration |
| Prompt builders | `render_prompt_slot_addendum` remains additive-only, bounded, and deterministic | Changing ordering, labels, or cap semantics |
| `agent/mod.rs` | `RuntimePluginHost::dispatch_event` and `collect_prompt_contributions` are callable even when no plugins are installed | Making no-plugin calls fail |
| Runtime control plane | Hung or exited plugin transport becomes a lifecycle failure with bounded recovery and stale tool removal | Removing RPC deadlines, generation checks, or restart scheduling |

## Notes
- Runtime plugin failures are isolated: startup, event, prompt, and reconciliation failures are logged per plugin and do not fail cognition.
- Protocol v1 does not claim a distributed two-phase commit with plugin process
  memory. After a callback response, any host rejection, decode failure, or
  durable state/receipt failure withdraws that generation's tools and restarts
  it from the host-owned snapshot. Plugin-authored external effects remain
  at-least-once and must deduplicate with supplied event/invocation identifiers.
- Tool invocations receive host-authored conversation/user/loop scope plus
  invocation and deadline timestamps; plugins cannot choose this context.
- JSON tool results may omit per-media `auto_play`. For audio entries only, the
  host fills the missing value from that plugin's successfully configured
  `auto_play_generated_media` setting, falling back to the legacy
  `auto_play_generated_audio` key. Explicit result values win, and no plugin or
  tool names participate in the decision.
- Proxy registration carries plugin ID, declared version, and supervised process
  generation into the registry authorization fingerprint, preventing a restarted
  or replaced package from inheriting a prior session approval by tool name.
- Tool proxies are only registered for plugins that return full `tools` manifests in the handshake; legacy `capabilities.tools` names alone are used for metadata only.
- Pre-versioning plugins remain protocol v1 because missing request/response/handshake version fields default to v1; new SDKs negotiate explicitly during `plugin.handshake`.
- The stdio transport now tolerates a bounded amount of non-JSON stdout noise before the first valid RPC response, which helps when third-party runtimes emit banners or environment chatter during startup.
- Runtime plugins should be initialized from a long-lived Tokio runtime (the dedicated agent loop runtime) so plugin stdio/process resources are polled on the same runtime for their full lifetime.
- Transport-layer failures remove the exact loaded generation, deregister its tools, and schedule bounded restart; an idle child exit is detected by periodic `try_wait` even without a new RPC.
- RPC deadline expiry is classified as a transport failure. Loaded plugins are deactivated and their proxy tools deregistered; startup failures also kill the partially initialized subprocess.
- The client mutex is released before failure deactivation so cleanup cannot deadlock while reacquiring the failed plugin transport.
- Plugin subprocess `stderr` is now piped into structured tracing logs (`target=runtime_plugin_stderr`) instead of being discarded, which makes model/runtime failures diagnosable without changing plugin transport behavior.
- Child processes use kill-on-drop so cancellation during startup/stop cannot orphan a subprocess.
- Reconciliation repairs interrupted `Starting`/`Stopping` records on the next tick, and tool registration records ownership before its await point so a cancelled control generation can safely finish registration later without stealing another plugin's tool name.
- An explicit static contribution contract opts a package into strict v1
  authority: runtime tools require exact structured contracts, and runtime
  capabilities/effects cannot appear from empty declarations. Manifests without
  `[contributions]` retain warning-based compatibility during migration.
- Changed-settings failure withdraws the live process and enters supervised
  recovery so ambiguous process-local configuration cannot survive. A repeated
  non-transport rejection becomes a terminal startup failure until operator
  input or the package changes; package/handshake identity violations are also
  terminal.
