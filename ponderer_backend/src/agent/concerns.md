# concerns.rs

## Purpose
Implements the concern lifecycle system for the Living Loop: concern records, signal ingestion from chat interactions, mention-based reactivation, salience decay, and concern-prioritized context generation for downstream reasoning.

## Components

### `Concern`
- **Does**: Represents one tracked concern with timestamps, salience, typed category, private notes, and linked memory keys
- **Interacts with**: `database.rs` concern CRUD methods and `ConcernsManager` lifecycle operations

### `ConcernType`
- **Does**: Encodes concern domains (project, household, system health, interest, reminder, conversation)
- **Interacts with**: JSON persistence in SQLite and future concern-update logic

### `Salience`
- **Does**: Priority tier for attention budgeting and includes DB mapping helpers (`as_db_str`, `from_db`)
- **Interacts with**: `database.rs` filtering (`get_active_concerns`) and future decay/pruning

### `ConcernContext`
- **Does**: Captures origin and historical update context for a concern
- **Interacts with**: concern lifecycle updates and debug introspection

### `ConcernSignal`
- **Does**: Structured concern signal parsed from engaged-loop assistant responses (`[concerns]...[/concerns]`)
- **Interacts with**: `agent/mod.rs` response parser + `ConcernsManager::ingest_signals`

### `ConcernsManager`
- **Does**: Handles concern creation/touch updates, mention reactivation, salience decay (`7d/30d/90d`), and priority context building for memory retrieval
- **Interacts with**: `database.rs`, `agent/mod.rs` loop integrations
- **Rationale**: Centralizes lifecycle policy so concern behavior stays deterministic and testable

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `database.rs` | Stable `Salience` DB string mappings and serializable `ConcernType` | Renaming variants or changing serde tagging |
| `agent/mod.rs` | `ConcernsManager::ingest_signals` returns created/touched concerns from structured chat metadata | Changing signal schema or ingest semantics |
| `agent/mod.rs` | `ConcernsManager::apply_salience_decay` uses inactivity thresholds (7d monitoring, 30d background, 90d dormant) | Altering thresholds without updating policy docs/tests |
| `agent/mod.rs` | `ConcernsManager::build_priority_context` yields concise concern-first context strings | Removing context builder used in prompt assembly |

## Notes
- Dormant concerns are treated as archived/stale for active loop attention.
- Mention-based touch intentionally reactivates dormant concerns back to `active`.
- Low-confidence concern signals are filtered out to reduce noise.
