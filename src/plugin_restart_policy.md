# plugin_restart_policy.rs

## Purpose

Defines the bounded restart and circuit-breaker policy used by plugin supervisors. It is deliberately transport- and process-agnostic so every plugin runtime receives the same recovery behavior.

## Components

### `PluginRestartPolicy`

- **Does**: Configures exponential backoff, its upper bound, the circuit threshold and cooldown, and the stable-run interval that resets a failure streak.
- **Interacts with**: `PluginLifecycleMachine` in `plugin_lifecycle.rs`.
- **Rationale**: Keeping recovery policy outside the process host makes restart decisions deterministic and independently testable.

### `PluginRestartDecision`

- **Does**: Returns either a normal bounded delay or an open-circuit cooldown for a failure count.
- **Interacts with**: Failure transitions in `plugin_lifecycle.rs`.

### `PluginRestartPolicyError`

- **Does**: Rejects policy configurations that could create hot restart loops or unusable circuits.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `plugin_lifecycle.rs` | Failure counts below the threshold receive exponential backoff; the threshold failure opens the circuit | Changing threshold inclusivity or delay semantics |
| Future plugin supervisor | Backoff is capped and a run is stable only after `stable_run_duration` | Removing the cap or redefining the stable boundary |

## Notes

- The default policy retries after 1, 2, 4, and 8 seconds, then opens the circuit on the fifth consecutive failure for five minutes. The normal delay is capped at 30 seconds.
- Zero stable-run duration is allowed for deterministic tests and intentionally immediate recovery policies; production defaults require a minute of stable execution.
