# sprite.rs

## Purpose
Renders the agent's visual indicator in the header bar. Uses avatar images when available, falling back to color-coded emoji for each agent state.

## Components

### `render_agent_sprite(ui, state, avatars)`
- **Does**: If an `AvatarSet` is provided and contains an avatar for the current state, updates animation, renders it as a 64x64 image, and requests repaint for animated avatars. Otherwise delegates to `render_agent_emoji`.
- **Interacts with**: `AvatarSet::get_for_state`, `Avatar::update`, `Avatar::current_texture`, `Avatar::is_animated`

### `render_agent_emoji(ui, state)` (private)
- **Does**: Maps each `AgentVisualState` variant to an emoji and color, then renders it at 48pt size:
  - `Idle` -> gray sleeping face
  - `Reading` -> light blue book
  - `Thinking` -> yellow thinking face
  - `Writing` -> light green writing hand
  - `Happy` -> green smiling face
  - `Confused` -> orange confused face
  - `Paused` -> light red pause icon

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `app.rs` | `render_agent_sprite(ui, state, Option<&mut AvatarSet>)` signature | Changing params breaks header rendering |
| `AgentVisualState` | All 7 variants matched exhaustively | Adding a variant causes compiler error here |
| `avatar.rs` | `AvatarSet` and `Avatar` public API | Changing avatar API breaks sprite rendering |
