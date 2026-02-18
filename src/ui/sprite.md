# sprite.rs

## Purpose
Renders the header agent visual indicator (avatar when available, emoji fallback otherwise) using frontend runtime state from API events/status.

## Components

### `render_agent_sprite(ui, state, avatars)`
- **Does**: Renders animated avatar frames for the current `AgentVisualState` or falls back to emoji.
- **Interacts with**: `AvatarSet::get_for_state`, `crate::api::AgentVisualState`.

### `render_agent_emoji(ui, state)`
- **Does**: Maps each visual state to a color-coded emoji.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `app.rs` | `render_agent_sprite` signature stability | Signature change breaks header rendering |
| `api.rs` | `AgentVisualState` variants used here remain available | Variant rename/removal breaks mapping |
| `avatar.rs` | Avatar public methods used for rendering remain stable | API changes break animated avatar path |
