# database/chat.rs

## Purpose
All chat-related types and `AgentDatabase` methods for private operator-agent communication, conversation management, turn lifecycle, tool call recording, OODA packet storage, and action digests.

## Components

### Constants
- `DEFAULT_CHAT_SESSION_ID` / `DEFAULT_CHAT_CONVERSATION_ID` / `TELEGRAM_CONVERSATION_ID` — well-known conversation/session identifiers
- `CHAT_TOOL_BLOCK_START/END`, `CHAT_THINKING_BLOCK_START/END`, etc. — tag delimiters used by helpers to strip raw metadata from stored messages

### `ChatMessage`
- **Does**: Represents a single operator or agent message in a conversation, with `processed` flag for unread tracking and optional `turn_id` linkage
- **Interacts with**: Agent poll loop, UI chat panel

### `ChatTurnPhase`
- **Does**: Enum encoding the persisted lifecycle state of conversations and turns (`idle`, `processing`, `completed`, `awaiting_approval`, `failed`); provides `as_db_str` / `from_db` conversion
- **Interacts with**: `chat_conversations.runtime_state`, `chat_turns.phase_state`

### `ChatSession` / `ChatConversation` / `ChatConversationSummary`
- **Does**: Top-level grouping containers for multi-thread desktop usage; `ChatConversation` includes `runtime_state` and `active_turn_id` for live status display; `ChatConversationSummary` stores compacted long-context snapshots
- **Interacts with**: UI conversation picker, agent loop compaction

### `ChatTurn` / `ChatTurnToolCall`
- **Does**: Per-turn records with full lifecycle fields (`decision`, `status`, `error`, `tool_call_count`, `prompt_text`, `system_prompt_text`) and per-tool call lineage
- **Interacts with**: `agent::process_chat_messages`, turn history/debug views

### `OodaTurnPacketRecord`
- **Does**: Compact Observe/Orient/Decide/Act summary per completed turn, stored for baton-style context carryover
- **Interacts with**: `agent::maybe_update_orientation`, orientation context hydration

### Chat message methods
- `add_chat_message` / `add_chat_message_in_conversation` / `add_chat_message_in_turn` — write messages with optional turn binding
- `get_unprocessed_operator_messages` / `mark_message_processed` — poll loop message queue
- `get_chat_history` / `get_chat_history_for_conversation` / `get_chat_history_slice_for_conversation` — history retrieval with optional conversation scope and offset windowing
- `count_chat_messages_for_conversation` — used for compaction threshold checks

### Conversation management methods
- `create_chat_conversation` / `list_chat_conversations` / `get_chat_conversation` / `delete_chat_conversation` / `update_chat_conversation_title`
- `upsert_chat_conversation_summary` / `get_chat_conversation_summary`
- `get_chat_context` / `get_chat_context_for_conversation` — format message history for LLM context, stripping raw metadata via helpers

### Turn lifecycle methods
- `begin_chat_turn` / `complete_chat_turn` / `fail_chat_turn` — state transitions
- `set_chat_turn_prompt` / `set_chat_turn_prompt_bundle` / `get_chat_turn_prompt` / `get_chat_turn_prompt_bundle` — prompt inspection storage
- `record_chat_turn_tool_call` / `list_chat_turns_for_conversation` / `list_chat_turn_tool_calls`

### OODA and action digest methods
- `save_ooda_turn_packet` / `get_latest_ooda_turn_packet` / `get_latest_ooda_turn_packet_for_conversation` / `get_recent_ooda_turn_packets_for_conversation_before`
- `get_recent_action_digest` / `get_recent_action_digest_for_conversation` — emit bounded human-readable turn history summaries

## Contracts
| Dependent | Expects |
|-----------|---------|
| `agent::process_chat_messages` | Turn lifecycle methods, message write/read, conversation state |
| `ui::app` | `ChatConversation.runtime_state`, conversation list/create/history APIs |
| `server.rs` | Conversation fetch, turns/tool-calls list, summary fetch |
