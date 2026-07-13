# plugin_contract/mod.rs

## Purpose
Defines the single public contract surface for Ponderer plugin packages and runtimes. The module re-exports versioned manifest, RPC, event, prompt, effect, durable-state, and status DTOs while keeping each concern in a focused file.

## Components

### Contract re-exports
- **Does**: Presents one stable import path, `ponderer_backend::plugin_contract`, for all plugin-facing DTOs and version helpers.
- **Interacts with**: `plugin.rs`, `runtime_process_plugin.rs`, `runtime_plugin_host.rs`, the desktop API client, and external plugin SDKs.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Backend host | Manifest and runtime DTOs share one serde definition | Duplicating or privately shadowing a wire DTO |
| Desktop frontend | Public manifest/settings types can be imported from the backend crate | Removing re-exports |
| Plugin SDKs | Version constants and RPC payloads remain transport-neutral | Changing serialized fields without aliases/version negotiation |

## Notes
- Legacy names remain available through aliases and re-exports from their former modules during migration.
