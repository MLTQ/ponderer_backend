# database/helpers.rs

## Purpose
Private utility functions used across the database submodules. All functions are `pub(super)` so they are accessible within the `database` module but not exposed to the rest of the crate.

## Components

### `short_conversation_tag`
- **Does**: Truncates a conversation ID to 12 characters for compact tagging in activity log lines
- **Interacts with**: `get_working_memory_context_for_conversation` in `memory.rs`

### `filter_activity_log_for_conversation`
- **Does**: Filters activity log content to lines relevant to a given conversation tag, suppressing cross-thread noise; returns `None` if no lines pass the filter
- **Interacts with**: `memory.rs` conversation-scoped working memory context builder

### `summarize_chat_message_for_context`
- **Does**: Strips raw metadata blocks (`[tool_calls]`, `[thinking]`, `[media]`, `[turn_control]`, `[concerns]`) from message content and replaces them with compact summary tags; calls `extract_tagged_blocks` and the `summarize_*` family
- **Interacts with**: `chat.rs` context formatters and action digest builder

### `extract_tagged_blocks`
- **Does**: Iteratively extracts all blocks delimited by a start/end tag pair, returning (remaining text, list of block contents)
- **Interacts with**: `summarize_chat_message_for_context`

### `summarize_tool_call_blocks`
- **Does**: Counts tool calls across all extracted blocks, collects up to 3 unique names, counts errors, emits a compact `tools=N (names) errors=E` string
- **Interacts with**: `summarize_chat_message_for_context`

### `summarize_media_blocks`
- **Does**: Counts media items and unique `media_kind` values, emits compact `media=N (kinds)` string
- **Interacts with**: `summarize_chat_message_for_context`

### `summarize_thinking_blocks`
- **Does**: Counts thinking items or blocks hidden, emits compact `thinking=N hidden` string
- **Interacts with**: `summarize_chat_message_for_context`

### `summarize_turn_control_blocks`
- **Does**: Parses the last `[turn_control]` block's `decision`/`status` fields, emits compact `turn=decision/status` string
- **Interacts with**: `summarize_chat_message_for_context`

### `summarize_concern_blocks`
- **Does**: Counts concern items in blocks, emits compact `concerns=N` string
- **Interacts with**: `summarize_chat_message_for_context`

### `compact_whitespace`
- **Does**: Collapses all whitespace runs into single spaces via `split_whitespace().join(" ")`
- **Interacts with**: `summarize_chat_message_for_context`

### `truncate_for_db_digest`
- **Does**: Truncates a string to `max_chars` characters, appending `...` if truncated; char-safe (not byte-indexed)
- **Interacts with**: Throughout the database module for length-limiting stored and formatted strings

### `outcome_to_db`
- **Does**: Maps `PromotionOutcome` enum to its database string representation (`"promote"` or `"hold"`)
- **Interacts with**: `memory.rs` promotion decision persistence
