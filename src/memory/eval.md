# eval.rs

## Purpose
Provides an offline, deterministic replay harness for benchmarking memory backends against trace datasets. Produces quantitative metrics and machine-readable JSON reports to support ALMA-lite memory design promotion decisions.

## Components

### `MemoryEvalTraceSet` / `MemoryEvalTrace`
- **Does**: Defines replay input format (named trace set with steps + checks per trace)
- **Interacts with**: `load_trace_set` for JSON ingestion

### `default_replay_trace_set()`
- **Does**: Provides a built-in deterministic replay dataset used when no external trace file is configured
- **Interacts with**: heartbeat-scheduled memory evolution runner in `agent/mod.rs`

### `MemoryEvalStep`
- **Does**: Encodes deterministic memory mutations (`write`, `delete`)
- **Interacts with**: `apply_step` and `MemoryBackend`

### `MemoryEvalCheck`
- **Does**: Encodes assertions/queries (`get`, `query`) used for scoring
- **Interacts with**: `evaluate_get_check`, `evaluate_query_check`

### `EvalBackendKind`
- **Does**: Declares candidate backend IDs and builders (`kv_v1`, `fts_v2`, `episodic_v3`, `null_v0`)
- **Interacts with**: `evaluate_trace_set` candidate loop
- **Rationale**: Keeps backend selection explicit and deterministic for reproducible replay runs

### `evaluate_trace_set(traces, candidates)`
- **Does**: Runs all traces against each candidate backend, computes metrics, and picks a winner
- **Interacts with**: `evaluate_candidate`, `MemoryBackend`, `MemoryEvalReport`

### `MemoryShadowComparison` / `evaluate_shadow_against_kv(traces, candidate)`
- **Does**: Executes two-backend shadow comparison (baseline `kv_v1` + selected candidate) and returns deltas plus safety regression signal
- **Interacts with**: `evaluate_trace_set`; promotion policy consumers that require explicit baseline-vs-candidate metrics

### `MemoryEvalReport` / `MemoryEvalMetrics`
- **Does**: Captures per-candidate metrics (recall, get pass-rate, latency, storage estimates) and winner selection
- **Interacts with**: `to_json_pretty`, `write_report_json`

### `load_trace_set(path)` / `write_report_json(report, path)`
- **Does**: File I/O for machine-readable replay input and output artifacts
- **Interacts with**: Heartbeat-scheduled `Agent::maybe_run_memory_evolution` plus archive/promotion persistence

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `Agent::maybe_run_memory_evolution` | `evaluate_trace_set` is deterministic for identical inputs | Non-deterministic ranking/scoring |
| Database design archive | `MemoryEvalReport` is serializable JSON | Renaming/removing report fields |
| Candidate backend work (`Ponderer-cpf.1.5`) | `evaluate_shadow_against_kv` emits baseline-safe comparison data | Removing non-regression fields or baseline pairing behavior |

## Notes
- Query scoring uses deterministic lexical matching with stable tie-breakers (score, key, timestamp).
- `null_v0` is an intentional lower-bound baseline for sanity checks.
- Replay DBs are in-memory per trace to isolate runs and keep results reproducible.
