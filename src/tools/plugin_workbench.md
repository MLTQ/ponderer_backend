# plugin_workbench.rs

## Purpose

Exposes the constrained plugin-authoring workbench to the model as one typed
tool while preserving the authority boundary between writing code and running
code.

## Components

### `PluginWorkbenchTool`

- **Does**: Offers `list`, `create_python`, `read`, `write`, `validate`, and
  `stage` actions backed by `PluginWorkbench`.
- **Interacts with**: `plugin_workbench.rs`, `ToolRegistry`, semantic effect
  policy, and the agent's normal function-calling loop.
- **Rationale**: A single structured interface gives the model a repair loop
  without granting arbitrary filesystem paths or native process execution.

### `plugin.draft.write` effect

- **Does**: Identifies writes that are confined to inert drafts and immutable,
  disabled staged packages.
- **Interacts with**: `tools/effect_policy.rs`, which classifies this effect as
  safe for autonomous authoring.
- **Rationale**: Constrained authoring is the intended delegated capability;
  activation, new secrets, sensors, and outward effects remain separate grants.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Agent tool loop | Every action returns structured JSON or a bounded error | Returning opaque subprocess output |
| Capability policy | `plugin.draft.write` does not imply execution or activation | Adding an `activate`/`run` action to this tool |
| Plugin workbench | All path confinement, size limits, validation, and immutable staging remain enforced below the tool layer | Reimplementing or bypassing those guards here |

## Notes

- The tool intentionally has no delete, execute, install-active, grant, or
  enable action.
- Staging a draft is useful progress but never claims that untrusted native code
  is sandboxed.
