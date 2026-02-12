# chat.rs

## Purpose
Provides two scrollable display panels: an event log showing agent activity and a private chat interface for operator-agent communication. Chat rendering also supports inline, collapsible tool-call/thinking details plus media payloads embedded in agent message content.

## Components

### `render_event_log(ui, events)`
- **Does**: Renders a scrollable list of `AgentEvent` variants with color-coded labels: observations (light blue), reasoning traces (gray, grouped), tool progress (khaki), actions (green), and errors (red). `StateChanged` and `ChatStreaming` events are skipped (rendered elsewhere).
- **Interacts with**: `AgentEvent` enum from `crate::agent`

### `render_private_chat(ui, messages, streaming_preview, media_cache)`
- **Does**: Renders a chat-style message list from `ChatMessage` records. Operator messages are snapped to the right; agent messages are left-aligned. Shows a processing indicator for unprocessed operator messages, collapsible tool/thinking details when present, inline media cards (image/audio/video/file), and an inline live preview bubble while provider tokens stream.
- **Interacts with**: `ChatMessage` from `crate::database` (fields: `role`, `content`, `created_at`, `processed`)

### `parse_chat_payload(content)`
- **Does**: Parses optional `[tool_calls]...[/tool_calls]`, `[thinking]...[/thinking]`, and `[media]...[/media]` JSON payloads, strips legacy `<think>/<thinking>` tags, and returns cleaned display text + structured metadata.
- **Interacts with**: Agent-side message formatter in `agent/mod.rs`

### `ChatMediaCache`
- **Does**: Caches decoded image textures by local file path so image previews can render efficiently inside chat bubbles without re-decoding every frame
- **Interacts with**: `render_media_panel`, `image` crate, `egui::Context::load_texture`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `app.rs` | `render_private_chat` takes `&mut ChatMediaCache` and message slices | Changing signature breaks `AgentApp::update` |
| `AgentEvent` | Variants: `Observation(String)`, `ReasoningTrace(Vec<String>)`, `ToolCallProgress { ... }`, `ActionTaken { action, result }`, `Error(String)`, `StateChanged(...)`, `ChatStreaming { ... }` | Removing/renaming breaks match arms |
| `ChatMessage` | Fields `role`, `content`, `created_at` (with `.format()`), `processed` | Renaming fields breaks rendering |
| `agent/mod.rs` | Metadata delimiters and JSON shape for tool/thinking/media blocks | Changing block names or payload fields without parser updates |

## Notes
- `stick_to_bottom(true)` keeps the scroll pinned to newest content.
- Private chat reserves ~140px for the composer section rendered by `app.rs`; this avoids the scroll area consuming all height.
- Each chat message row allocates full panel width before layout, so operator bubbles consistently anchor to the right edge even in narrow windows.
- Each bubble is rendered inside a fixed-width slot (bounded to panel width) before drawing content, preventing right-aligned operator messages from drifting off-screen in small windows.
- When both metadata sets exist, Thinking and Tool Calls sections render in side-by-side columns to avoid overlap.
- Long unbroken tokens (paths/JSON/chunks without spaces) are force-wrapped based on bubble width so text cannot expand message bubbles past the window.
- Streaming preview intentionally shows raw in-flight text (including internal narration markers) before final post-processing/persistence.
- Tool and thinking detail blocks are intentionally hidden by default behind `egui::CollapsingHeader` sections to keep chat readable.
- Image media entries attempt inline previews from local paths; non-image media shows typed file cards with path + MIME details.
