# skills/mod.rs

## Purpose
Defines the event DTO consumed by the cognitive loop after protocol-v1 plugins
are polled. The historical `SkillEvent` name is retained for serialized and
internal compatibility; it is no longer an extension trait namespace.

## Components

### `SkillEvent`
- **Does**: Carries normalized external content as `NewContent { id, source, author, body, parent_ids }` for deduplication and reasoning.
- **Interacts with**: `runtime_plugin_host.rs`, `plugin_event_ledger.rs`, and the event-processing paths in `agent/mod.rs`.
- **Rationale**: Keeps the cognitive event representation stable while package transport and authority remain owned by the protocol-v1 host.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime_plugin_host.rs` | Poll responses normalize into `SkillEvent` values | Renaming fields or variants |
| Agent reasoning/orientation/journal | Events retain stable IDs, authors, bodies, and parent references | Changing enum shape or serialization |
| Durable event ledger | Event payloads round-trip through Serde | Removing `Serialize` / `Deserialize` |

## Notes
- External integrations must be discovered protocol-v1 subprocess packages.
- `SkillEvent` currently has only `NewContent`; richer transport events should be normalized deliberately or added as versioned variants.
- The removed `Skill`, `SkillContext`, `SkillResult`, and `SkillActionDef` types are an intentional source-breaking cleanup for any downstream in-process adapters.
