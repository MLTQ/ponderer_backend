# files.rs

## Purpose
Implements filesystem-facing tools (`read_file`, `write_file`, `list_directory`, `patch_file`) used by the agentic loop. The file centralizes path resolution, output shaping, and guardrails like size/entry limits.

## Components

### `ReadFileTool`
- **Does**: Reads text files with optional `offset`/`limit` slicing and line-number formatting, or reports binary file size.
- **Interacts with**: `ToolContext.working_directory` for relative path resolution.

### `WriteFileTool`
- **Does**: Writes text content to a file and creates parent directories when needed.
- **Interacts with**: Tool approval policy in `mod.rs` (`requires_approval = true`).

### `ListDirectoryTool`
- **Does**: Lists directory contents with optional recursion and compact size metadata.
- **Interacts with**: Shared formatting helpers and directory traversal limits.

### `PatchFileTool`
- **Does**: Applies targeted text replacement operations for in-place file edits.
- **Interacts with**: Agent edit workflows that need precise patching instead of full rewrites.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `tools/agentic.rs` | Tool names and JSON schemas stay stable for function-calling | Renaming tools or schema fields |
| `tools/mod.rs` | File tools return `ToolOutput::Text/Error` with clear execution status | Changing output conventions |
| Agent prompts | `list_directory`/`read_file` are available for discovery before edits | Removing read/list tools |

## Notes
- `MAX_READ_BYTES` and `MAX_LIST_ENTRIES` cap expensive operations.
- Tests build `ToolContext` with default allow/deny lists to match registry policy changes.
