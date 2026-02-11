# reasoning.rs

## Purpose
Handles JSON-structured decision-making prompts for event analysis and legacy private-chat processing against OpenAI-compatible chat-completions backends.

## Components

### `ReasoningEngine`
- **Does**: Builds prompts, calls the LLM endpoint, and parses decisions into typed `Decision` variants
- **Interacts with**: `Agent` orchestration in `mod.rs`, `SkillEvent` input stream, and database-backed contexts

### `parse_decision` / `parse_chat_decision`
- **Does**: Converts extracted JSON into domain decisions (`Reply`, `UpdateMemory`, `NoAction`, `ChatReply`)
- **Interacts with**: Agent action routing paths

### `extract_json` and helpers
- **Does**: Robustly extracts JSON from noisy model output (code fences, comments, trailing commas, smart quotes, think tags)
- **Interacts with**: All decision parsing entry points

### `strip_thinking_tags`
- **Does**: Removes both `<think>...</think>` and `<thinking>...</thinking>` sections before JSON extraction
- **Interacts with**: Prevents thinking-tag leakage from breaking parser flow

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `mod.rs` | `Decision` enum variants and field names remain stable | Renaming/removing variants/fields |
| LLM prompt handlers | `extract_json` tolerates common formatting mistakes | Tightening parser behavior without migration |

## Notes
- Private operator chat now primarily uses the agentic tool loop, but this module remains active for event-analysis JSON decisions.
