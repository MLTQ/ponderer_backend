# protocol.rs

## Purpose
Defines the versioned JSON-over-stdio RPC, handshake, capability, configuration-state, tool declaration, invocation-context, and result DTOs. It is transport-neutral and shared with SDK/conformance implementations.

## Components

### RPC envelopes
- **Does**: Carry request identity, method/params, result/error, and negotiated protocol revision.
- **Interacts with**: `runtime_plugin_host.rs` framing and plugin SDK dispatch.

### `RuntimePluginHandshakeRequest` / `RuntimePluginHandshake`
- **Does**: Advertise host-supported versions and record the plugin-selected version plus its declared contributions.
- **Interacts with**: host startup negotiation and SDK initialization.

### Capability and tool manifests
- **Does**: Describe available hooks, prompt slots, polling, requested capabilities, JSON schemas, approval hints, and semantic effects.
- **Interacts with**: runtime tool proxies and future capability brokers.

### Tool invocation/result DTOs
- **Does**: Provide the narrow portable bridge between host tool calls and plugin outputs, including invocation time/scope and namespaced state mutations.
- **Interacts with**: `RuntimePluginHost::invoke_tool` and SDK tool handlers.

### Configuration DTOs
- **Does**: Restore the plugin's schema-versioned host state before configuration and carry state mutations back to the host.
- **Interacts with**: `database/plugins.rs`, runtime configuration, and the Python SDK state API.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Legacy plugins | Missing protocol/effect/capability fields decode as v1/empty | Removing serde defaults |
| Runtime host | Full tool JSON schemas are available after handshake | Changing parameter schema semantics |
| SDK | RPC envelopes remain one JSON object per line | Renaming envelope fields without protocol revision |
