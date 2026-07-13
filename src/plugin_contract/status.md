# status.rs

## Purpose
Defines transport-safe plugin effect declarations and runtime health snapshots. Operational supervisors may use richer internal state, then project it into these DTOs for APIs and diagnostics.

## Components

### `PluginEffectDeclaration`
- **Does**: Gives a semantic side effect a stable ID, human description, and approval hint.
- **Interacts with**: static manifests and runtime tool manifests.

### `PluginRuntimeState`
- **Does**: Describes externally meaningful lifecycle states without exposing process-host implementation details.
- **Interacts with**: `PluginRuntimeStatus` and plugin status APIs.
- **Rationale**: Keeps `unavailable`, `backoff`, and `circuit_open` distinct so desired-state reconciliation remains observable.

### `PluginRuntimeStatus`
- **Does**: Carries desired/actual state, restart health, negotiated protocol, process identity, state-change time, and diagnostic timestamps.
- **Interacts with**: runtime supervisors, backend APIs, and frontend status surfaces.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Policy broker | Effects use semantic IDs rather than inferring authority from tool names | Reinterpreting an existing effect ID |
| API clients | New status fields default safely when absent | Removing serde defaults or renaming fields without aliases |

## Notes
- Timestamps are RFC 3339 strings at this boundary so alternate plugin hosts need not share Ponderer's clock type.
