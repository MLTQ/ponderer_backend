# chat.rs

## Purpose
Provides two scrollable display panels: an event log showing agent activity and a private chat interface for operator-agent communication. Both are stateless rendering functions.

## Components

### `render_event_log(ui, events)`
- **Does**: Renders a scrollable list of `AgentEvent` variants with color-coded labels: observations (light blue), reasoning traces (gray, grouped), actions (green), and errors (red). `StateChanged` events are skipped (shown in header).
- **Interacts with**: `AgentEvent` enum from `crate::agent`

### `render_private_chat(ui, messages)`
- **Does**: Renders a chat-style message list from `ChatMessage` records. Operator messages are indented right with blue styling; agent messages are left-aligned with green styling. Shows a processing indicator for unprocessed operator messages.
- **Interacts with**: `ChatMessage` from `crate::database` (fields: `role`, `content`, `created_at`, `processed`)

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `app.rs` | Both functions take `&mut egui::Ui` + slice references | Changing signatures breaks `AgentApp::update` |
| `AgentEvent` | Variants: `Observation(String)`, `ReasoningTrace(Vec<String>)`, `ActionTaken { action, result }`, `Error(String)`, `StateChanged(...)` | Adding variants is safe; removing/renaming breaks match arms |
| `ChatMessage` | Fields `role`, `content`, `created_at` (with `.format()`), `processed` | Renaming fields breaks rendering |

## Notes
- Both functions reserve 60px at the bottom of available height for the input bar in `app.rs`.
- `stick_to_bottom(true)` keeps the scroll pinned to newest content.
