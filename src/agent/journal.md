# journal.rs

## Purpose
Implements the private inner-life journal system for the Living Loop. It defines journal entry types and provides `JournalEngine` to generate authentic, concise inner-monologue entries from orientation/context signals with strict JSON parsing.

## Components

### `JournalEntry`
- **Does**: Represents one journal note with type, text, context, related concerns, and optional mood values
- **Interacts with**: `database.rs` journal CRUD methods and `Agent::maybe_write_journal_entry` in `mod.rs`

### `JournalEntryType`
- **Does**: Enumerates journal note categories and provides DB string conversion helpers (`as_db_str`, `from_db`)
- **Interacts with**: SQLite persistence in `database.rs`

### `JournalContext`
- **Does**: Carries generation context such as trigger, estimated user state, and time-of-day label
- **Interacts with**: Future orientation/journal prompt templates

### `JournalMood`
- **Does**: Stores lightweight affect values captured with an entry (`valence`, `arousal`)
- **Interacts with**: Orientation synthesis and trend analysis

### `JournalEngine`
- **Does**: Builds a journal prompt from orientation/recent entries/concerns/events, calls LLM JSON generation, and returns optional `JournalEntry` when the model chooses to write
- **Interacts with**: `llm_client.rs` (`generate_json`), `orientation.rs`, `concerns.rs`, `skills/mod.rs`
- **Rationale**: Keeps journal generation isolated from loop orchestration so loop code only applies gating and persistence policy

### `journal_skip_reason`
- **Does**: Centralizes rate-limit gating logic (`disposition=journal`, unchanged disposition skip, minimum interval)
- **Interacts with**: `Agent::maybe_write_journal_entry` in `mod.rs`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `database.rs` | Stable `JournalEntryType` DB string mappings and `JournalEntry` serialization | Renaming enum variants or conversion outputs |
| `agent/mod.rs` | `JournalEngine::maybe_generate_entry` returns `Ok(None)` for skip/no-write conditions instead of hard errors | Changing skip semantics to throw errors |
| `agent/mod.rs` | `journal_skip_reason` encodes same-disposition and interval gating in one place | Diverging gating logic from helper behavior |

## Notes
- Prompt explicitly asks for inner monologue and varied wording to reduce repetitive report-style entries.
- Parsing is tolerant: malformed model output causes a skip rather than crashing the loop.
