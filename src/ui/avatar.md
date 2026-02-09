# avatar.rs

## Purpose
Handles loading, animating, and selecting avatar images (static PNG/JPG or animated GIF) for the agent's visual representation. Provides per-state avatar selection (idle, thinking, active).

## Components

### `AvatarFrame` (private)
- **Does**: Holds a single `egui::TextureHandle` and its display `Duration` (zero for static images)

### `Avatar`
- **Does**: Represents a single avatar asset, either static (one frame) or animated GIF (multiple frames with timing)
- **Interacts with**: `egui::Context` for texture loading, `image` crate for decoding

### `Avatar::load(ctx, path) -> Result<Self, String>`
- **Does**: Dispatches to `load_static` or `load_animated_gif` based on file extension (png/jpg/jpeg vs gif)

### `Avatar::load_static(ctx, path)`
- **Does**: Opens image, converts to RGBA, uploads as a single egui texture

### `Avatar::load_animated_gif(ctx, path)`
- **Does**: Decodes all GIF frames via `image::codecs::gif::GifDecoder`, uploads each as a separate texture with per-frame duration. Defaults to 100ms for frames with zero delay.

### `Avatar::update()`
- **Does**: Advances `current_frame` when elapsed time exceeds the current frame's duration. No-op for static avatars.

### `Avatar::current_texture() -> &TextureHandle`
- **Does**: Returns the texture for the currently displayed frame

### `Avatar::is_animated() -> bool` / `Avatar::reset()`
- **Does**: Query animation state / reset to first frame

### `AvatarSet`
- **Does**: Container holding optional `Avatar` instances for three visual states: `idle`, `thinking`, `active`
- **Interacts with**: `AgentVisualState` from `crate::agent`

### `AvatarSet::load(ctx, idle_path, thinking_path, active_path)`
- **Does**: Loads each avatar from optional paths, logging warnings on failure

### `AvatarSet::get_for_state(state) -> Option<&mut Avatar>`
- **Does**: Maps `AgentVisualState` variants to avatars with fallback: `Idle`/`Paused` -> idle; `Thinking`/`Reading`/`Confused` -> thinking (or idle); `Writing`/`Happy` -> active (or idle)

### `AvatarSet::has_avatars() -> bool`
- **Does**: Returns true if at least one avatar is loaded

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `sprite.rs` | `AvatarSet::get_for_state`, `Avatar::update`, `Avatar::current_texture`, `Avatar::is_animated` | Changing these signatures breaks sprite rendering |
| `app.rs` | `AvatarSet::load`, `AvatarSet::has_avatars` | Changing load signature breaks avatar initialization |
| `AgentVisualState` | Variants: `Idle`, `Paused`, `Thinking`, `Reading`, `Confused`, `Writing`, `Happy` | Adding new variants requires updating `get_for_state` match |

## Notes
- All GIF frames are uploaded as separate GPU textures at load time. Large GIFs with many frames will consume proportional GPU memory.
- The `image` crate's `AnimationDecoder` trait is imported but only `GifDecoder` is used.
