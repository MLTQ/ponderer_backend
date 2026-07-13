# plugin_workbench.rs

## Purpose

Provides a constrained, non-executing authoring area where Ponderer or an
operator can draft, inspect, validate, and immutably stage protocol-v1 plugin
packages without implicitly granting or activating them.

## Components

### `PluginWorkbench`

- **Does**: Creates SDK-based Python scaffolds, provides path-confined draft file
  I/O, validates canonical manifests, and copies valid drafts into a versioned
  package store with `enabled=false` metadata.
- **Scaffold**: Starts with an explicit empty `[contributions]` contract so any
  later hook, prompt slot, or poller must be deliberately added to package
  authority, plus an empty canonical `tools.json` referenced by the manifest so
  added SDK tools must acquire an exact static schema/effect declaration.
- **Interacts with**: `plugin_contract`, the Python SDK, the registered
  `plugin_workbench` tool, and the plugin manager's install/enable flow.
- **Rationale**: Self-directed authoring should be useful before it is
  authoritative. Drafting code and activating native execution are deliberately
  separate acts.

### `PluginDraftValidation`

- **Does**: Returns the parsed canonical manifest plus accumulated errors and
  warnings instead of failing at the first semantic problem. It also runs the
  production package loader, so referenced settings/tool contracts, strict
  static authority, and launch-path structure are checked before staging.
- **Rationale**: Structured validation gives an LLM enough feedback to repair its
  own draft over multiple iterations.

### `StagedPluginPackage`

- **Does**: Records the immutable package ID/version/path and explicitly marks
  the staged package disabled.
- **Rationale**: Staging is not installation authority. A later manager step must
  resolve requested capabilities and enable a package.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| Model-authored workflows | Draft paths cannot escape the workbench or traverse symlinks | Accepting absolute paths or `..` traversal |
| Package manager | Staged versions are immutable and carry `.ponderer-stage.json` with `enabled=false` | Overwriting a staged version or auto-enabling it |
| Plugin SDK | New Python drafts import `ponderer_plugin_sdk` and speak protocol v1 | Reintroducing handwritten RPC scaffolds |
| Operator policy | No method executes a draft or grants capabilities | Adding implicit run/enable behavior |

## Notes

- Defaults are `<config-dir>/plugin-workbench` and
  `<config-dir>/plugins/store`; environment variables can relocate either.
- Staging rejects symlinks and excludes virtual environments, bytecode caches,
  egg metadata, and Finder metadata.
- Per-file, per-draft, draft-count, staged-package-count, and total-store quotas
  bound an always-on author from filling the host disk; display metadata is
  bounded before scaffold rendering.
- Staging copies into a unique temporary directory and atomically renames it,
  so a losing concurrent stage can never delete the winning immutable package.
- The copied snapshot is validated again before publication, closing the
  validation/copy race for all mutations made through the workbench.
- Workbench packages must explicitly declare both contract versions and
  `[contributions]`; removing those fields cannot downgrade a draft into the
  permissive pre-versioning compatibility path.
- The workbench is an authoring primitive, not a sandbox. Native conformance
  execution remains gated until a real sandbox adapter exists.
