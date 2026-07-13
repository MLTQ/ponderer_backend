# version.rs

## Purpose
Owns plugin manifest and runtime protocol version constants plus compatibility helpers. Version `1` is also the implicit version for pre-versioning packages and runtime messages.

## Components

### Version constants
- **Does**: Identifies the current manifest/protocol revisions and the protocol versions accepted by this host.
- **Interacts with**: manifest serde defaults and runtime handshake negotiation.

### `negotiate_plugin_protocol_version`
- **Does**: Selects the highest host-supported version also offered by a peer.
- **Interacts with**: SDK handshake implementations and future multi-version compatibility adapters.

### `PluginHostDescriptor`
- **Does**: Identifies the host implementation during protocol negotiation without coupling plugins to backend internals.
- **Interacts with**: `RuntimePluginHandshakeRequest` in `protocol.rs`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Legacy packages/plugins | Missing version fields mean v1 | Changing default version values |
| Runtime host and SDK | Supported versions are explicit and deterministic | Negotiating a version not present in both sets |
