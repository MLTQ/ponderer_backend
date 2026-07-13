# mod.rs

## Purpose
Defines the shared tool abstraction (`Tool` trait), typed tool I/O (`ToolOutput`, call/result structs), and `ToolRegistry` used by the agentic loop to discover and execute local capabilities.

## Components

### `Tool` trait
- **Does**: Declares tool metadata (name/description/JSON schema), execution contract, approval requirement, and category
- **Interacts with**: `tools/agentic.rs` function-calling loop

### `ToolRegistry`
- **Does**: Stores tools, builds OpenAI-format tool definitions, and executes calls with approval checks, per-context allow/deny filtering, and optional rolling side-effect quota reservation immediately before invocation. Maintains a `session_approved` set of tool names that bypass the autonomous-mode block for the rest of the session.
- **Interacts with**: `main.rs` (tool registration), `agent/mod.rs` (shared registry + context policies), `tools/approval.rs`

### `ToolRegistry::grant_session_approval`
- **Does**: Inserts a tool name into the session-approved set so subsequent autonomous calls to that tool skip the `NeedsApproval` gate.
- **Interacts with**: `agent/mod.rs` `Agent::grant_session_tool_approval` and `server.rs` `POST /v1/agent/tools/:name/approve`

### `ToolContext`
- **Does**: Carries execution metadata (`working_directory`, `username`, optional `conversation_id`, `autonomous`), tool-scope controls (`allowed_tools`, `disallowed_tools`), and an optional process-shared `ToolInvocationRateLimit` for outward side effects.
- **Interacts with**: `ToolRegistry::tool_definitions_for_context`, `ToolRegistry::execute_call`, `tools/agentic.rs`

### `ToolInvocationRateLimit`
- **Does**: Atomically reserves a rolling-window slot immediately before a configured tool executes and retains it for the full window even if the response is an error.
- **Interacts with**: `agent/mod.rs` process-wide Graphchan quota and `ToolRegistry::execute_call`.
- **Rationale**: A visibility-only check before an agentic pass can be exceeded by multiple calls in that pass or by concurrent autonomous contexts. A timeout or lost response is causally ambiguous, so it must not refund quota for a remote side effect that may already have happened.

### Tool modules
- **Does**: Exposes built-in tool namespaces:
  - `shell`, `files` for local operations
  - `http` for guarded web/API fetch
  - `memory` for persistent note search/write
  - `scheduled_jobs` for recurring schedule CRUD inside the tool loop
  - `comfy` for delegating generation to the configured workflow plugin
  - `runtime_plugin` for proxying subprocess runtime-plugin tools into the normal tool loop
  - `workflow_plugin` for running filesystem-installed Comfy workflow bundles through one generic tool
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
- Session approvals (`grant_session_approval`) override the autonomous-mode NeedsApproval gate for the lifetime of the process; they are not persisted across restarts.
- Tool availability can now be restricted per run context before the model sees function defs and again at execution time.
- The process-shared outbound limiter enforces quota at invocation time across concurrent autonomous contexts. Durable rolling-window recovery across backend restart remains separate persistence work.
- `ToolOutput::Json` is now a key channel for rich chat metadata (for example media payloads extracted later by `agent/mod.rs` and `ui/chat.rs`).
