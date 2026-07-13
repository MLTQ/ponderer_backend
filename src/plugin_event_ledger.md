# plugin_event_ledger.rs

## Purpose

Turns runtime-plugin observations and agent lifecycle notifications into a durable,
ordered temporal stream before they reach cognition or plugin subprocesses.

## Components

### `PluginEventLedger`

- **Does**: Records host lifecycle events, deduplicates plugin-polled events by
  `(plugin_id, source_event_id)`, reconstructs pending `SkillEvent`s, and advances
  an explicit consumer cursor only after the caller accepts a batch.
- **Interacts with**: `database/plugins.rs`, `plugin_contract/events.rs`,
  `runtime_plugin_host.rs`, and the agent's external-event processing path.
- **Rationale**: Process memory cannot represent elapsed time across a crash.
  The ledger makes observation precede reaction and gives replay a stable
  boundary.

### `RecordedPluginEvent`

- **Does**: Reports the durable row and whether this call inserted it or found a
  source-scoped duplicate.
- **Rationale**: A plugin may return the same remote item on every poll; duplicate
  observations must not become duplicate experiences.

### `PluginSkillEventBatch`

- **Does**: Carries replayed events, quarantine count, watermark, and the durable
  receipt that authorizes acknowledgement.
- **Rationale**: Separating read from acknowledgement prevents a crash between
  persistence and cognitive ingestion from silently losing an event; binding the
  watermark to a random token prevents accidental future cursor jumps.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Runtime plugin host | Plugin output is recorded before a `SkillEvent` is returned | Returning raw, unrecorded poll responses |
| Agent event loop | Passes the delivered batch to `acknowledge_skill_events` only after accepting it | Accepting caller-authored sequence numbers |
| Plugin authors | Source event IDs need only be unique within one plugin | Treating IDs as globally unique |
| Durable self model | Plugin observations remain evidence, not canonical persona state | Writing plugin payloads directly into persona tables |

## Notes

- The initial consumer is `host.agent` and the subscription is
  `plugin.poll.new_content`; both names are stable persistence keys.
- Structurally corrupt rows are quarantined during database paging. Poll-event
  rows with unsupported schema versions or invalid typed payloads are
  dead-lettered here, and the issued watermark still permits replay to continue.
- Lifecycle delivery is at-least-once through per-plugin, per-subscription
  cursors. Each callback receives a stable ledger event ID for plugin-side
  effect deduplication; its host-owned state changes and receipt advance commit
  together.
- If a plugin callback succeeds locally but the host cannot accept its durable
  events, state, or receipt, the supervisor stops that process generation. Its
  replacement restores the last durable snapshot before replaying the pending
  callback.
- This module performs synchronous SQLite work behind the existing database
  mutex; event pages are intentionally bounded.
- A 90-day hard retention bound applies even to unconsumed rows, preventing a
  removed or never-registered subscription from leaking the ledger forever;
  ordinarily acknowledged rows compact after seven days.
