# shell.rs

## Purpose
Provides the `shell` tool for command execution during agentic runs. It wraps `/bin/sh -c` with timeout/output controls and returns structured command results for model reasoning.

## Components

### `ShellTool`
- **Does**: Executes shell commands with configurable working directory and timeout, returning exit code + stdout/stderr.
- **Interacts with**: `ToolContext.working_directory`, `ToolRegistry` approval flow (`requires_approval = true`).

### Output truncation + timeout constants
- **Does**: Enforces bounded runtime (`MAX_TIMEOUT_SECS`) and output size (`MAX_OUTPUT_BYTES`) for safer autonomous usage.
- **Interacts with**: UI/tool-result rendering paths that display command output previews.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `tools/agentic.rs` | Tool name is `shell` with `command`, optional `working_directory`, optional `timeout_secs` | Renaming tool or schema fields |
| `tools/mod.rs` | `requires_approval()` remains true so autonomous runs can gate execution | Changing approval requirement semantics |
| Agent prompts | Output contains explicit exit code for retry/error handling | Removing or changing exit-code format |

## Notes
- Non-zero command exits still return `ToolOutput::Text` so the model can inspect stderr and recover.
- Tests use the shared `ToolContext` policy fields (`allowed_tools`, `disallowed_tools`) introduced in registry-level gating.
