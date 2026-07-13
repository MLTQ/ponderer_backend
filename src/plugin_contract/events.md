# events.rs

## Purpose
Defines plugin-to-host and host-to-plugin prompt, lifecycle, polling, acknowledgement, and event-effect DTOs. Canonical prompt slot names use dotted namespaces while legacy snake-case spellings remain readable.

## Components

### Prompt contribution DTOs
- **Does**: Model bounded, attributable plugin context/instruction blocks and their invocation context.
- **Interacts with**: prompt assembly in `agent/*` and `RuntimePluginHost`.

### `RuntimePluginLifecycleEvent`
- **Does**: Carries typed host lifecycle facts to subscribed plugins.
- **Interacts with**: agent lifecycle boundaries and SDK event handlers.

### Poll/event DTOs
- **Does**: Carry externally observed content, durable event acknowledgements, piggybacked host-state mutations, and concise state-change effects.
- **Interacts with**: runtime polling and the agent event loop.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Existing Python plugins | Both dotted and legacy snake-case prompt slot names decode | Removing aliases |
| Prompt builders | Contributions retain plugin attribution, priority, and bounds | Dropping attribution or max-character semantics |
| Agent event loop | Poll events retain stable IDs, source, author, body, and parent IDs | Reinterpreting those fields |
