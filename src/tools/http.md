# http.rs

## Purpose
Provides a guarded web-fetch tool (`http_fetch`) for agentic workflows. It supports common HTTP verbs while enforcing safety defaults: localhost/private network blocking, outbound secret-leak checks, request timeout, and bounded response capture.

## Components

### `HttpFetchTool`
- **Does**: Executes GET/POST/PUT/DELETE requests with optional headers and body, returns status/headers/body preview JSON.
- **Interacts with**: `ToolRegistry` in `mod.rs`, provider loop in `agentic.rs`

### `validate_destination_safety(url, allow_private_hosts)` (private)
- **Does**: Blocks local/private destinations by hostname/IP and by DNS resolution when available.
- **Interacts with**: `tokio::net::lookup_host`

### `check_outbound_for_leaks(...)` (private)
- **Does**: Runs request URL/headers/body through leak detection before network send.
- **Interacts with**: `tools::safety::detect_leaks`

### `is_private_or_local_ip(ip)` (private)
- **Does**: Classifies loopback/private/link-local/multicast/reserved addresses as blocked.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| LLM tool-calling | Tool name `http_fetch` and schema fields (`url`, `method`, `headers`, `body_text`, `body_json`, `timeout_secs`) | Renaming tool or required fields |
| Safety posture | Private/local destinations are blocked unless `allow_private_hosts=true` | Weakening default destination checks |
| Agent loop/UI | JSON output includes request metadata and truncated response preview fields | Removing response shape keys used in reasoning/debugging |

## Notes
- Timeout defaults to 30 seconds and is capped at 30 seconds.
- Response capture defaults to 64KB and is capped at 512KB.
- Plain HTTP is allowed but emits a warning in output (`HTTPS preferred` behavior).
- HTTP client creation now uses shared panic-safe construction with timeout support (`http_client::build_http_client_with_timeout`) for portability.
