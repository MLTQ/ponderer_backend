# loose_autonomy.rs

## Purpose
Forms self-originated goals and parses bounded episode lifecycle reports for explicitly armed Loose mode. It keeps long-lived agency durable while ensuring one model invocation cannot become the agent's entire lifetime.

## Components

### `LooseGoalEngine`
- **Does**: Makes one tool-free structured model call to propose a concrete goal, motive, and first observable step from bounded lived context.
- **Interacts with**: `llm_client.rs`, generation telemetry, and `agent/mod.rs` intention creation.
- **Rationale**: Goal adoption is an explicit durable transition, rather than an incidental action taken during an idle prompt.

### `LooseGoalSeed`
- **Does**: Carries a normalized candidate that can become a `SelfAuthored` durable intention.
- **Interacts with**: `intentions.rs` `NewAgentIntention`.

### `LooseEpisodeDecision`
- **Does**: Represents `continue`, `completed`, `blocked`, or `abandoned` settlement after one bounded work episode.
- **Interacts with**: `agent/mod.rs` claim settlement and continuation cadence.

### `split_episode_report`
- **Does**: Removes the private `[intention_status]` JSON block from narrative output and parses its lifecycle decision.
- **Rationale**: The model expresses whether a multi-episode project remains alive without leaking control markup into journals or chat.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `agent/mod.rs` | Goal proposals are optional and episode reports fail closed to `None` | Marker names, decision variants, or proposal field semantics |
| Durable intentions | Summary, motivation, and first step are non-empty and bounded | Removing normalization or allowing empty seeds |

## Notes
- Lived context is quoted as untrusted evidence and cannot create authority merely by containing instructions.
- Goal formation has no tools; action begins only after the adopted intention is durably persisted and claimed.
- Missing or malformed lifecycle markup never terminally completes a goal.
