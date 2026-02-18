# avatar.rs

## Purpose
Loads and animates avatar assets (static PNG/JPG and animated GIF) and maps them to frontend visual states.

## Components

### `Avatar`
- **Does**: Owns texture frames and timing for one avatar asset.

### `AvatarSet`
- **Does**: Holds optional idle/thinking/active avatars and resolves the best avatar for the current frontend visual state.
- **Interacts with**: `crate::api::AgentVisualState`.

### `AvatarSet::get_for_state(state)`
- **Does**: Maps state variants to avatar slots with idle fallback.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `sprite.rs` | `get_for_state`, `update`, `current_texture`, `is_animated` behavior remains stable | Signature or behavior changes affect rendering |
| `app.rs` | `AvatarSet::load` and `has_avatars` contract remains stable | Changes break avatar initialization flow |
| `api.rs` | Visual-state variants align with mapping branches | Variant drift breaks selection mapping |

## Notes
- GIF frames are fully decoded/uploaded at load time; large animations increase GPU memory use.
