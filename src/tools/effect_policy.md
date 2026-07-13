# effect_policy.rs

## Purpose
Resolves semantic tool effects into host-owned minimum approval and rate-limit behavior. Plugin declarations may strengthen these minimums but can never weaken the policy associated with an effect ID.

## Components

### Effect ID constants
- **Does**: Defines the host-recognized vocabulary such as `network.read`, `filesystem.write`, `external.publish`, `identity.propose_change`, and the path-confined `plugin.draft.write` authoring effect.
- **Interacts with**: plugin tool manifests, runtime proxies, and future capability grants.

### `resolve_tool_effect_policy`
- **Does**: Normalizes/deduplicates declared effects and monotonically joins their host minimums with the tool's legacy approval flag.
- **Interacts with**: `Tool::effect_policy` and `ToolRegistry::execute_call`.
- **Rationale**: The host, not a plugin-supplied boolean, decides the minimum authority needed for a semantic side effect.

### `ToolApprovalMinimum`
- **Does**: Distinguishes no approval, approval for autonomous calls, and approval for every call.
- **Interacts with**: registry session approval and execution context.

### `ToolRateLimitClass`
- **Does**: Marks tools whose invocations consume the shared outward-action quota.
- **Interacts with**: `ToolInvocationRateLimit`.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Tool registry | Policy joins are monotonic; `false` declarations never remove a host minimum | Replacing joins with plugin-preferred values |
| Runtime proxy | Manifest effects are exposed unchanged for policy resolution | Dropping effect metadata |
| Agent quota | `external.publish` maps to `OutboundAction` independently of tool names | Changing that mapping |

## Notes
- Unknown or malformed effects conservatively require approval in autonomous contexts until explicitly classified.
- `plugin.draft.write` is approval-free only because the workbench forbids execution, activation, symlink traversal, and writes outside its bounded roots.
- `identity.propose_change` and `secrets.read` always require an approval unless the tool has an explicit session grant.
- Tool names never imply authority. Every integration that can act outside the
  process must declare its semantic effects in the static package contract.
