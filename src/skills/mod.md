# skills/mod.rs

## Purpose
Defines the `Skill` trait and supporting types that form the plugin interface for external system integrations. Skills give the agent capabilities to poll for events and execute actions on external platforms.

## Components

### `Skill` (trait)
- **Does**: Async trait with four methods: `name()`, `description()`, `poll(ctx)`, `execute(action, params)`, and `available_actions()`
- **Interacts with**: `agent::Agent` (calls `poll` each cycle, calls `execute` for agent-chosen actions), `main.rs` (skills instantiated and passed to Agent)
- **Rationale**: Decouples the agent loop from specific integrations; new platforms can be added by implementing this trait

### `Skill::poll(ctx)`
- **Does**: Called each agent cycle; returns `Vec<SkillEvent>` representing new content since last poll
- **Interacts with**: `SkillContext` (provides the agent's username for filtering own posts)

### `Skill::execute(action, params)`
- **Does**: Performs a named action with JSON parameters; returns `SkillResult::Success` or `SkillResult::Error`
- **Interacts with**: `agent::actions` (agent decides which action to call based on LLM output)

### `Skill::available_actions()`
- **Does**: Returns `Vec<SkillActionDef>` describing all actions the skill supports, used to build LLM prompts

### `SkillEvent`
- **Does**: Enum with `NewContent { id, source, author, body, parent_ids }` variant representing incoming content from an external system
- **Interacts with**: `agent::Agent` poll loop (processes events, decides whether to respond)

### `SkillResult`
- **Does**: Enum with `Success { message }` and `Error { message }` variants

### `SkillContext`
- **Does**: Struct with `username` field, passed to `poll` so skills can filter out the agent's own content

### `SkillActionDef`
- **Does**: Describes an action for prompt generation: `name`, `description`, `params_description`
- **Interacts with**: `agent::reasoning` (injected into LLM prompts so the agent knows what actions are available)

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `main.rs` | `Box<dyn Skill>` is `Send + Sync` | Removing `Send + Sync` bounds from trait |
| `agent::Agent` | `poll` returns `Vec<SkillEvent>`; `execute` takes `(&str, &serde_json::Value)` | Changing method signatures |
| `skills::graphchan` | Must implement `Skill` trait | Changing any trait method signature |
| `agent::reasoning` | `available_actions()` returns `Vec<SkillActionDef>` with `name`, `description`, `params_description` | Changing `SkillActionDef` fields |

## Notes
- Currently only one skill implementation exists: `graphchan::GraphchanSkill`.
- `SkillEvent` only has the `NewContent` variant; additional variants (e.g., `Reaction`, `DirectMessage`) would need to be added for richer integrations.
- The `async_trait` crate is used since Rust's native async traits were not stable when this was written.
