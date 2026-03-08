# database/orientation.rs

## Purpose
Living Loop foundation: orientation snapshot storage and pending thought queue management.

## Components

### `OrientationSnapshotRecord`
- **Does**: Typed record for stored orientation snapshots; includes `user_state`, `disposition`, `synthesis`, `salience_map`, `anomalies`, `pending_thoughts` (all as `serde_json::Value`), plus optional mood fields
- **Interacts with**: `agent::maybe_update_orientation`, Living Loop debugging views

### `PendingThoughtRecord`
- **Does**: Queued thought item with `priority`, `relates_to` list, and lifecycle timestamps (`surfaced_at`, `dismissed_at`)
- **Interacts with**: Future ambient/orientation loop surfaces and LL debugging views

### Orientation snapshot methods
- `save_orientation_snapshot` — inserts or replaces a snapshot, serializing JSON Value fields
- `get_recent_orientations` — retrieves the N most recent snapshots by timestamp desc; deserializes JSON fields with fallback to empty arrays

### Pending thought queue methods
- `queue_pending_thought` — inserts or replaces a pending thought record
- `get_unsurfaced_thoughts` — returns all unsurfaced, non-dismissed thoughts ordered by priority desc, created_at asc
- `mark_thought_surfaced` — stamps `surfaced_at` with current time
- `dismiss_thought` — stamps `dismissed_at` with current time

## Notes
- `salience_map`, `anomalies`, `pending_thoughts` in `OrientationSnapshotRecord` are nullable TEXT columns deserialized to JSON; fall back to `json!([])` if null
- The partial index `idx_pending_unsurfaced` on `(surfaced_at) WHERE surfaced_at IS NULL` speeds up `get_unsurfaced_thoughts`
