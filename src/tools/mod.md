# mod.rs

## Purpose
Defines the shared tool abstraction (`Tool` trait), typed tool I/O (`ToolOutput`, call/result structs), and `ToolRegistry` used by the agentic loop to discover and execute local capabilities.

## Components

### `Tool` trait
- **Does**: Declares tool metadata (name/description/JSON schema), execution contract, approval requirement, and category
- **Interacts with**: `tools/agentic.rs` function-calling loop

### `ToolRegistry`
- **Does**: Stores tools, builds OpenAI-format tool definitions, and executes calls with approval checks
- **Interacts with**: `main.rs` (tool registration), `agent/mod.rs` (shared registry), `tools/approval.rs`

### Tool modules
- **Does**: Exposes built-in tool namespaces:
  - `shell`, `files` for local operations
  - `comfy` for ComfyUI generation + Graphchan publishing
  - `vision` for local image evaluation, chat media publication, and optional screenshot capture
  - `agentic`, `approval`, `safety` for orchestration and policy

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | Module exports for all built-in tools and stable `ToolRegistry` API | Renaming/removing modules or registry methods |
| `tools/agentic.rs` | `tool_definitions` and `execute_call` behavior for function-calling loop | Changing payload shapes or return semantics |
| Tool implementations | `ToolOutput::to_llm_string()` remains usable for model feedback | Altering output encoding conventions |

## Notes
- Approval checks happen at registry execution time, not inside each tool.
- `ToolOutput::Json` is now a key channel for rich chat metadata (for example media payloads extracted later by `agent/mod.rs` and `ui/chat.rs`).
