# http_client.rs

## Purpose
Provides panic-safe `reqwest::Client` constructors used across backend modules. This isolates host-OS proxy-discovery edge cases so the backend can degrade gracefully in headless/macOS environments.

## Components

### `build_http_client`
- **Does**: Builds a default HTTP client with panic-safe fallback to `no_proxy`.
- **Interacts with**: `llm_client.rs`, `tools/agentic.rs`, `tools/comfy.rs`, `skills/graphchan.rs`, `agent/reasoning.rs`, `agent/trajectory.rs`, `comfy_client.rs`

### `build_http_client_with_timeout`
- **Does**: Same as `build_http_client` but applies an optional request timeout before build.
- **Interacts with**: `tools/http.rs`

### `attempt_build` (private)
- **Does**: Applies timeout/proxy options and builds a concrete `reqwest::Client`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| HTTP-enabled modules | Returned `reqwest::Client` is usable even when default system proxy discovery panics | Removing fallback path or changing function names/signatures |
| Tool/runtime startup | Client init failures are logged and retried with `no_proxy` | Reintroducing unconditional `Client::new()` in startup paths |

## Notes
- By default, clients are built with `no_proxy` to avoid host-OS system proxy panics in restricted/headless environments.
- Set `PONDERER_ENABLE_SYSTEM_PROXY=1` (or `true`) to attempt system proxy discovery first, with `no_proxy` fallback on failure.
- This module centralizes a previously repeated resilience pattern.
