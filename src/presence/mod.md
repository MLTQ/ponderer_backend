# mod.rs

## Purpose
Defines the Living Loop presence sampler. `PresenceMonitor` gathers low-level system/user signals into a typed `PresenceState` used by orientation synthesis, with graceful degradation when platform probes are unavailable.

## Components

### `PresenceMonitor`
- **Does**: Samples idle time, system load, and top active processes; tracks session timing and last interaction
- **Interacts with**: `agent/orientation.rs` context assembly and `agent/mod.rs` loop integration

### `PresenceState`
- **Does**: Snapshot of user/system state with idle/session durations, local time context, load, and active process list
- **Interacts with**: Future orientation synthesis inputs

### `TimeContext::now`
- **Does**: Derives coarse temporal flags (weekend, late-night, deep-night, work-hours) from local clock, with panic-safe fallback to UTC components when local clock APIs fail on host OS
- **Interacts with**: Future rhythm/disposition logic

### `SystemLoad` / `InterestingProcess` / `ProcessCategory`
- **Does**: Typed envelope for CPU/memory/GPU/process signals with heuristic process categorization
- **Interacts with**: Orientation heuristics and LLM prompt context

### `duration_seconds` (private serde helper)
- **Does**: Serializes `std::time::Duration` as seconds for JSON compatibility
- **Interacts with**: `PresenceState` serde derives

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Future orientation engine | `PresenceState` fields stay stable and serializable | Renaming/removing core fields |
| Future ambient loop | `PresenceMonitor::sample()` returns useful signals even when optional probes fail | Hard-failing on missing platform tools (`ioreg`, `xprintidle`, `nvidia-smi`) |

## Notes
- Idle-time probing currently supports macOS (`ioreg`) and Linux (`xprintidle`) with fallback to interaction-derived timing.
- Process and load sampling use shell probes (`ps`, `sysctl`/`nproc`) so phase-2 works without adding non-cached runtime dependencies.
- GPU metrics are opportunistic via `nvidia-smi`; missing command or unsupported hardware yields `None` values.
- Process categorization uses keyword heuristics and can be refined in later phases.
- Local time sampling now guards against platform panic edge-cases (observed in some macOS/headless contexts) and degrades to UTC-based time flags rather than crashing the backend.
