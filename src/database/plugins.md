# plugins.rs

## Purpose

Persists plugin-owned state and Ponderer's ordered plugin event ledger in SQLite.
It provides bounded, restart-safe delivery without allowing plugin processes to
write directly into the agent's autobiographical tables.

## Components

### `PluginStateRecord` and state methods

- **Does**: Put, get, list, and delete versioned JSON values in a `(plugin_id, key)` namespace.
- **Interacts with**: Plugin configure/restore flow and the `plugin_state` table.
- **Limits**: Schema versions must be positive; identifiers are bounded and
  control-free; one value is at most 256 KiB; one plugin owns at most 1,024 keys
  and 16 MiB of serialized state.
- **Batching**: Runtime callback mutations are validated against the final
  projected namespace and committed in one transaction; one invalid value rolls
  back the complete batch.

### `NewPluginEvent` / `PluginEventRecord`

- **Does**: Describe an event before and after durable sequence assignment.
- **Rationale**: `(source, source_event_id)` deduplicates source-local observations while `event_id` remains globally addressable.
- **Limit**: Serialized payloads are at most 256 KiB, enforced by both the API and SQLite triggers.

### `PluginEventPage` / `PluginEventDeadLetter`

- **Does**: Read bounded ordered pages while atomically moving malformed rows to `plugin_event_dead_letters`.
- **Rationale**: One damaged row must not poison every later replay; dead-letter payload snapshots and reasons are themselves bounded.

### `PluginEventDeliveryBatch` / `PluginEventDeliveryReceipt`

- **Does**: Atomically reads one subscription page and issues a durable, unguessable token tied to its exact watermark.
- **Interacts with**: `prepare_plugin_event_delivery` and `acknowledge_plugin_event_delivery`.
- **Rationale**: Preparation registers a zero cursor before reading, so even an unacknowledged first batch blocks compaction. Repeated reads return the same in-flight range and token while newer events wait for the next batch. A consumer cannot advance to an arbitrary future sequence, and one receipt row per consumer/subscription bounds delivery metadata.
- **Atomic side effects**: Lifecycle delivery can commit its state-mutation
  batch and advance the exact issued receipt in the same transaction. Replaying
  an already acknowledged receipt does not apply state twice.

### `PluginEventCursor`

- **Does**: Stores one consumer's monotonic position for an exact event-type subscription.
- **Interacts with**: Delivery acknowledgement and cursor-aware compaction.

### `PluginEventRetentionPolicy` / `PluginEventCompactionReport`

- **Does**: Deletes acknowledged events only when every registered cursor for
  that event type has passed them, applies a longer hard age bound to unconsumed
  events, and expires dead letters on a separate window.
- **Defaults**: Seven days after acknowledgement, 90 days for unconsumed live
  events, and 30 days for dead letters.
- **Rationale**: The long hard bound prevents removed or never-created
  subscriptions from pinning the database forever while giving unconsumed
  observations a much longer replay window.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Plugin supervisor | State survives restart and stays isolated by plugin ID | Changing namespace or overwrite semantics |
| Runtime adapters | Duplicate `(source, source_event_id)` inserts resolve to the original row | Removing source deduplication |
| Replay consumers | Only an issued receipt can advance its subscription cursor | Reintroducing raw-sequence acknowledgement |
| Operators | Bad rows remain inspectable in a bounded dead-letter store | Dropping corrupt rows without diagnostics |
| Compaction scheduler | Recent unacknowledged/unsubscribed events are retained; the documented 90-day hard bound applies | Deleting past a live cursor before hard expiry |

## Notes

- Event insertion, page quarantine, receipt issue, acknowledgement, and compaction each use SQLite transactions.
- Page and dead-letter listing limits are clamped to 1–1000 rows.
- Unsupported semantic schema versions are quarantined by `plugin_event_ledger.rs`; structural database corruption is quarantined here.
- Compaction reports ordinary acknowledged deletion separately from hard-expired
  unconsumed deletion so operators can observe retention pressure.
