# http_client.rs

## Purpose
Provides panic-safe `reqwest::Client` constructors used across backend modules. This isolates host-OS proxy-discovery edge cases so the backend can degrade gracefully in headless/macOS environments.

## Components

### `build_http_client`
- **Does**: Builds a default HTTP client with a 120-second request deadline and panic-safe fallback to `no_proxy`.
- **Interacts with**: `llm_client.rs`, `tools/agentic.rs`, `agent/reasoning.rs`, and `agent/trajectory.rs`.

### `build_http_client_with_timeout`
- **Does**: Builds the same panic-safe client with an explicit optional request timeout; `None` intentionally preserves reqwest's no-deadline behavior for callers that opt into it.
- **Interacts with**: `tools/http.rs`

### `DEFAULT_HTTP_REQUEST_TIMEOUT`
- **Does**: Defines the bounded 120-second deadline used by ordinary backend and LLM HTTP clients.
- **Rationale**: Prevents a hung model connection from blocking the always-on loop indefinitely while leaving enough time for local inference.

### `attempt_build` (private)
- **Does**: Applies timeout/proxy options and builds a concrete `reqwest::Client`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| HTTP-enabled modules | Returned `reqwest::Client` is usable even when default system proxy discovery panics | Removing fallback path or changing function names/signatures |
| Tool/runtime startup | Client init failures are logged and retried with `no_proxy` | Reintroducing unconditional `Client::new()` in startup paths |
| LLM callers | Ordinary clients eventually fail stalled requests instead of waiting forever | Removing or substantially shortening the default deadline |

## Notes
- By default, clients are built with `no_proxy` to avoid host-OS system proxy panics in restricted/headless environments.
- Set `PONDERER_ENABLE_SYSTEM_PROXY=1` (or `true`) to attempt system proxy discovery first, with `no_proxy` fallback on failure.
- Explicit callers of `build_http_client_with_timeout` retain control over their timeout, including `None` when an unbounded client is deliberate.
- This module centralizes a previously repeated resilience pattern.
