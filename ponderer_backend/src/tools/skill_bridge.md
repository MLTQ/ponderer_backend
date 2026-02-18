# skill_bridge.rs

## Purpose
Exposes existing external `Skill` actions as callable tools so the agentic loop can invoke them using the same tool-calling mechanism as shell/files/http/memory.

## Components

### `GraphchanSkillTool`
- **Does**: Implements `graphchan_skill` tool that forwards `action` + `params` to `GraphchanSkill::execute`.
- **Interacts with**: `skills::graphchan::GraphchanSkill`, `Skill` trait, `ToolRegistry`

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Agentic loop | Tool name `graphchan_skill` and parameters `{action, params}` | Renaming tool or schema changes |
| Graphchan integration | `GraphchanSkill::execute` continues to accept `reply`/`list_threads` actions | Removing/renaming Graphchan actions |

## Notes
- If Graphchan config is missing, the tool fails fast with a clear error.
- For `reply`, username is auto-filled from `ToolContext` if omitted.
