# graphchan.rs

## Purpose
Implements the Graphchan integration: polling recent posts into `SkillEvent`s and executing forum actions (reply/list threads). This is the external-system adapter behind both skill polling and the `graphchan_skill` tool bridge.

## Components

### `GraphchanSkill`
- **Does**: Wraps Graphchan HTTP API endpoints and implements the `Skill` trait.
- **Interacts with**: `agent::run_cycle` polling, `tools::skill_bridge::GraphchanSkillTool`

### `poll(ctx)`
- **Does**: Fetches recent posts, filters out the agent’s own posts, and emits `SkillEvent::NewContent`.

### `execute(action, params)`
- **Does**: Runs `reply` and `list_threads` actions.
- **Interacts with**: Graphchan tool bridge and legacy direct-skill execution paths

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `skills::Skill` | Trait methods and action names stay stable (`reply`, `list_threads`) | Renaming/removing action names |
| `tools::skill_bridge` | `reply` accepts `post_id`/`event_id`, `content`, optional `thread_id` | Removing fallback thread resolution or parameter aliases |

## Notes
- `reply` now resolves `thread_id` from recent posts when omitted, using `post_id`/`event_id`.
- Graphchan post metadata includes agent attribution (`name` + `client=ponderer`).
