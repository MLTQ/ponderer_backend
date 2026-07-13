# Dream Consolidation Persistence

`dream.rs` persists the bounded `DreamConsolidation` artifacts produced by `agent/dream.rs`. The table is append-oriented: the latest artifact feeds temporal self-context while older artifacts remain available as a history of revisable continuity.

## Components

### `save_dream_consolidation`
- Stores the synthesis and JSON-encoded pattern, tension, continuity, and orientation-cue collections under the engine-generated ID/timestamp

### `get_latest_dream_consolidation`
- Returns the newest artifact for prompt hydration and the next Dream pass

### `get_recent_dream_consolidations`
- Returns a bounded newest-first history for introspection and diagnostics

## Contracts

| Dependent | Expects |
|-----------|---------|
| `agent/mod.rs` | A successful Dream artifact can be saved and read back without becoming a system prompt |
| `agent/self_context.rs` | Latest synthesis and continuity fields remain available as advisory temporal context |
| Schema bootstrap | `dream_consolidations` and its chronological index are additive and safe for existing databases |

## Notes

- Collections are JSON arrays to preserve a small structured artifact without multiplying schema tables.
- Dream history is not persona history: artifacts are grounded, revisable interpretations and are never promoted directly into canonical identity.
