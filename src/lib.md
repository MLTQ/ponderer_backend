# lib.rs

## Purpose
Defines the backend crate surface for Ponderer. This crate is the extraction boundary for all non-UI capabilities so it can be run standalone or consumed by any frontend client.

## Components

### Module exports
- **Does**: Re-exports backend domain modules (`agent`, `config`, `database`, `intentions`, `tools`, `skills`, `plugin`, `workflow_plugin`, `runtime_process_plugin`, `runtime_plugin_host`, `process_registry`, `scheduled_jobs`, etc.) and `runtime` bootstrap.
- **Interacts with**: desktop frontend binary (`src/main.rs`) and future backend service entrypoint(s).

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `src/main.rs` | Backend modules available under `ponderer_backend::*` | Renaming/removing exported modules |
| Future `ponderer_backend serve` | Runtime bootstrap and domain modules remain backend-only with no UI dependency | Introducing frontend/UI modules here |
| External backend extensions | `plugin` module exposes stable trait/contracts for tool+skill registration | Removing or reshaping `plugin` contracts |
| Agent orientation/runtime loops | `intentions` exposes durable work lifecycle and provenance types without requiring the monolithic agent module | Removing lifecycle variants or changing field meanings |

## Notes
- `lib.rs` is intentionally thin; runtime composition lives in `runtime.rs`.
- This crate is the canonical location for backend logic going forward.
