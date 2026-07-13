# state.rs

## Purpose

Defines protocol-v1 values used to restore and mutate a plugin's host-owned
durable state namespace.

## Components

### `PluginStateValue`

- **Does**: Carries one schema-versioned JSON value in a configuration snapshot.
- **Interacts with**: `database/plugins.rs`, runtime host configuration, and SDK
  `Plugin.state`.

### `PluginStateMutation`

- **Does**: Requests an upsert or deletion within the calling plugin's own
  namespace; plugin IDs are never accepted from the subprocess.
- **Interacts with**: event acknowledgements, poll responses, tool results, and
  host persistence.
- **Rationale**: Piggybacked mutations preserve the simple request/response
  stdio transport while keeping SQLite and namespace authority in the host.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Runtime host | Applies mutations only under the authenticated package ID | Accepting a plugin-supplied namespace |
| Python SDK | Missing schema versions mean v1 | Removing compatibility defaults |
| Plugin authors | State is restored before `configure` and acknowledged mutations survive restart | Treating process memory as durable state |
