# app.rs

## Purpose
Defines `AgentApp`, the top-level eframe application. It owns the agent handle, event receiver, all UI panels, chat state, and the tokio runtime used to dispatch async commands from the GUI thread.

## Components

### `AgentApp`
- **Does**: Central application state holding agent reference, event log, UI panels, avatar set, database handle, and chat history
- **Interacts with**: `Agent` (via `Arc<Agent>`), `AgentEvent`/`AgentVisualState` from `crate::agent`, `AgentDatabase`/`ChatMessage` from `crate::database`, all UI panel structs
- **Rationale**: Single owner of all GUI state; bridges sync egui rendering with async agent operations

### `AgentApp::new(event_rx, agent, config, database)`
- **Does**: Constructs the app, initializes all panels from config, defers avatar loading to first frame
- **Interacts with**: `SettingsPanel`, `CharacterPanel`, `ComfySettingsPanel`, `AgentConfig`

### `AgentApp::refresh_chat_history()`
- **Does**: Pulls the latest 50 chat messages from the database into `self.chat_history`
- **Interacts with**: `AgentDatabase::get_chat_history`

### `AgentApp::send_chat_message(content)`
- **Does**: Inserts an "operator" role message into the database, then refreshes history
- **Interacts with**: `AgentDatabase::add_chat_message`

### `AgentApp::load_avatars(ctx, config)`
- **Does**: Reads avatar paths from config and delegates to `AvatarSet::load`
- **Interacts with**: `AvatarSet`, `AgentConfig` fields `avatar_idle`, `avatar_thinking`, `avatar_active`

### `impl eframe::App for AgentApp` -- `update()`
- **Does**: Main render loop. Loads avatars on first frame, polls `event_rx` for `AgentEvent`s, renders header with sprite, toolbar buttons, event log or chat panel, user input bar, and all modal panels (settings, character, comfy workflow). Persists config and hot-reloads the agent on save.
- **Interacts with**: `sprite::render_agent_sprite`, `chat::render_event_log`, `chat::render_private_chat`, `SettingsPanel::render`, `CharacterPanel::render`, `ComfySettingsPanel::render`, `Agent::reload_config`, `Agent::toggle_pause`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Binary entry point | `AgentApp::new` signature with `Receiver<AgentEvent>`, `Arc<Agent>`, `AgentConfig`, `Option<Arc<AgentDatabase>>` | Changing constructor args breaks startup |
| `SettingsPanel` | `config` field is public and mutated by `CharacterPanel` saves | Making `config` private breaks cross-panel sync |
| `Agent` | `reload_config` and `toggle_pause` are async | Removing these methods breaks UI buttons |
| `AgentDatabase` | `get_chat_history(limit)` and `add_chat_message(role, content)` | Changing DB API signatures breaks chat |

## Notes
- A dedicated `tokio::runtime::Runtime` is created inside the app because eframe's render loop is synchronous. All async agent calls are dispatched through `self.runtime.spawn`.
- Chat history auto-refreshes every 2 seconds via `last_chat_refresh` timer.
- Avatar loading is deferred to the first `update()` call because `egui::Context` is not available at construction time.
