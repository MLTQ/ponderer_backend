# mod.rs

## Purpose
Defines the shared tool abstraction (`Tool` trait), typed tool I/O (`ToolOutput`, call/result structs), and `ToolRegistry` used by the agentic loop to discover and execute local capabilities.

## Components

### `Tool` trait
- **Does**: Declares tool metadata (name/description/JSON schema), execution contract, legacy approval requirement, semantic effects, host-resolved effect policy, provider authorization identity, and category.
- **Interacts with**: `tools/agentic.rs` function-calling loop

### `ToolRegistry`
- **Does**: Stores tools, builds OpenAI-format tool definitions, and executes calls with approval checks, per-context allow/deny filtering, and optional rolling side-effect quota reservation immediately before invocation. Captures a complete authorization fingerprint (provider, contract, effect policy, and registry generation) for each registration and binds any session grant to that exact fingerprint.
- **Interacts with**: `main.rs` (tool registration), `agent/mod.rs` (shared registry + context policies), `tools/approval.rs`

### `ToolRegistry::grant_session_approval`
- **Does**: Records the current registration's authorization fingerprint so subsequent calls skip the `NeedsApproval` gate only for that exact registered tool instance.
- **Interacts with**: `agent/mod.rs` `Agent::grant_session_tool_approval` and `server.rs` `POST /v1/agent/tools/:name/approve`
- **Rationale**: A replacement plugin must not inherit authority by reusing an approved tool name and effect policy. Registration and approval state share one lock, and both replacement and deregistration clear the old grant atomically.

### `ToolContext`
- **Does**: Carries execution metadata (`working_directory`, `username`, optional `conversation_id`, `autonomous`), the explicitly scoped `auto_approve_local` Loose-mode flag, tool-scope controls (`allowed_tools`, `disallowed_tools`), and an optional process-shared `ToolInvocationRateLimit` for outward side effects.
- **Interacts with**: `ToolRegistry::tool_definitions_for_context`, `ToolRegistry::execute_call`, `tools/agentic.rs`

### `ToolInvocationRateLimit`
- **Does**: Atomically reserves a rolling-window slot immediately before an `OutboundAction` tool (or a legacy configured name) executes and retains it for the full window even if the response is an error.
- **Interacts with**: `effect_policy.rs`, `agent/mod.rs` process-wide outward-action quota, and `ToolRegistry::execute_call`.
- **Rationale**: A visibility-only check before an agentic pass can be exceeded by multiple calls in that pass or by concurrent autonomous contexts. A timeout or lost response is causally ambiguous, so it must not refund quota for a remote side effect that may already have happened.

### Tool modules
- **Does**: Exposes built-in tool namespaces:
  - `shell`, `files` for local operations
  - `http` for guarded web/API fetch
  - `memory` for persistent note search/write
  - `plugin_workbench` for confined draft creation, iterative repair, validation, and disabled staging
  - `scheduled_jobs` for recurring schedule CRUD inside the tool loop
  - `runtime_plugin` for proxying subprocess runtime-plugin tools into the normal tool loop
  - `vision` for local image evaluation, chat media publication, optional screenshot capture, and optional camera snapshots
  - `agentic`, `approval`, `safety` for orchestration and policy

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | Module exports for all built-in tools and stable `ToolRegistry` API | Renaming/removing modules or registry methods |
| `tools/agentic.rs` | `tool_definitions_for_context` and `execute_call` enforce `ToolContext` scope rules | Changing context-policy fields or filtering semantics |
| Tool implementations | `ToolOutput::to_llm_string()` remains usable for model feedback | Altering output encoding conventions |

## Notes
- Approval checks happen at registry execution time, not inside each tool.
- `auto_approve_local` bypasses only `Autonomous` approval on filesystem/shell categories or tools declaring exclusively known local filesystem/process/draft effects; `Always`, unknown, network-write, identity/secrets, and semantic outbound actions retain host gates and quotas.
- Semantic effect minimums are resolved in `effect_policy.rs`; a plugin's `requires_approval = false` cannot override a host minimum.
- Session approvals (`grant_session_approval`) override the matching effect-policy gate only for the exact registered fingerprint; unknown tools are not pre-approved, every replacement/deregistration invalidates the grant even when the contract is unchanged, and grants are not persisted across restarts.
- Tool availability can now be restricted per run context before the model sees function defs and again at execution time.
- The process-shared outbound limiter enforces quota at invocation time across concurrent autonomous contexts. `for_outbound_effects` supports name-independent enforcement while the fixed-name constructor remains a compatibility adapter. Durable rolling-window recovery across backend restart remains separate persistence work.
- An outward-action quota of zero is fail-closed: it disables tools with the `OutboundAction` policy instead of meaning unlimited.
- `ToolOutput::Json` is now a key channel for rich chat metadata (for example media payloads extracted later by `agent/mod.rs` and `ui/chat.rs`).
- `ToolContext::generation_observer` lets model-using tools inherit the caller's telemetry lane without coupling tools to the UI event bus.
