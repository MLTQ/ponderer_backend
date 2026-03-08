# database/journal.rs

## Purpose
Living Loop foundation: private journal entry CRUD and context formatting for the agent's internal journal.

## Components

### Journal methods on `AgentDatabase`
- `add_journal_entry` — inserts or replaces a `JournalEntry` with all Living Loop fields (type, content, trigger, user state, time of day, related concerns, mood valence/arousal)
- `get_recent_journal` — retrieves the N most recent journal entries ordered by timestamp desc
- `get_journal_for_context` — formats recent journal entries as a `## Recent Journal Notes` section, respecting an estimated token budget
- `search_journal` — full-text LIKE search over `content` and `trigger` fields; falls back to `get_recent_journal` if query is empty

## Contracts
| Dependent | Expects |
|-----------|---------|
| `agent::journal` | `JournalEntry`, `JournalContext`, `JournalEntryType`, `JournalMood` types accepted and returned |
| Living Loop ambient loop | `add_journal_entry` for orientation and event recording |

## Notes
- `journal_entries` table uses `entry_type TEXT` serialized via `JournalEntryType::as_db_str()` / `from_db()`
- `related_concerns` stored as a JSON array of string IDs
- `mood_valence` and `mood_arousal` are nullable REAL columns; paired — both present or both absent
- Search uses SQLite `LOWER(...) LIKE ?` pattern for case-insensitive substring matching
