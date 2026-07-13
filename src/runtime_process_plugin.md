# runtime_process_plugin.rs

## Purpose
Discovers filesystem-installed runtime-process plugin bundles from the shared `plugins/` directory and turns their `plugin.toml` plus optional settings schema into backend manifests and launch specs. This is the static bundle loader for subprocess-backed plugins such as a future qwen3-TTS service.

## Components

### `RuntimeProcessPluginCatalog`
- **Does**: Ensures `PONDERER_PLUGIN_DIR` (or `./plugins`) exists, scans it for runtime-process packages, and atomically refreshes an interior catalog while reporting added, updated, and removed IDs.
- **Interacts with**: `runtime.rs` bootstrap and `runtime_plugin_host.rs`.
- **Rationale**: Atomic replacement keeps readers on the last complete snapshot if a directory scan fails; malformed neighboring bundles are isolated rather than aborting discovery.

### `RuntimeProcessCatalogRefresh`
- **Does**: Describes the sorted package identities added, materially updated, or removed by one successful rescan.
- **Interacts with**: `RuntimePluginHost` desired-state reconciliation.

### `plugin_dir_path` / `ensure_plugin_dir`
- **Does**: Resolve the shared plugin directory path (next to the executable/config by default, or `PONDERER_PLUGIN_DIR` when overridden) and create it on demand so fresh portable installs always have a local `plugins/` folder before discovery runs.
- **Interacts with**: runtime bootstrap and the live plugin host.

### `RuntimeProcessPluginBundle`
- **Does**: Holds the canonical contract manifest plus resolved launch command/working directory, and computes whether the plugin should be enabled from `AgentConfig.plugin_settings`.
- **Interacts with**: `runtime_plugin_host.rs` startup and config reload logic.
- **Rationale**: `plugin.toml` is decoded through `plugin_contract::RuntimeProcessPluginPackageManifest`, eliminating the former private duplicate of identity/version/settings fields.

### `RuntimeProcessLaunchSpec`
- **Does**: Stores the resolved subprocess command line and working directory used to launch the plugin.
- **Interacts with**: `runtime_plugin_host.rs` process spawning.

### JSON tool-contract loading
- **Does**: Loads `tool_contract_file` documents shaped as `{ "tools": [...] }`, validates unique nonempty names and consistent effect declarations, and fills structured tools plus compatibility name/effect summaries.
- **Confinement**: Settings and tool-contract references must resolve to regular
  files inside the package; absolute paths, parent traversal, and escaping
  symlinks are rejected.
- **Interacts with**: The Python SDK `load_tool_contract` helper and handshake authority validation.
- **Rationale**: Static discovery and runtime registration consume one schema/effect source instead of trusting a second runtime-authored contract.

## Contracts

| Dependent | Expects | Breaking changes |
|-----------|---------|------------------|
| `runtime.rs` | Discovery creates the shared `plugins/` directory if it is missing and skips non-runtime bundles silently | Stopping directory auto-creation or failing hard on mixed plugin types |
| `runtime_plugin_host.rs` | Reads return cloned bundles from an atomic snapshot; refresh reports package changes; `manifest_with_tools` merges handshake-discovered tools | Returning borrowed data across refresh or changing refresh identity semantics |
| Future plugin bundles | `plugin.toml` uses `plugin_type = "runtime_process"`, explicit manifest/protocol versions, `[contributions]`, and `command = ["..."]`; `tool_contract_file` is mutually exclusive with inline `tools` | Renaming required fields or weakening strict admission |
| Browser/Image/Voice migration | Only the exact `browser-orb`, `image-orb`, and `voice-orb` ID/direct-child-directory pairs may load a wholly pre-versioning manifest | Expanding or making the compatibility allowlist package-configurable |

## Notes
- Runtime bundles are visible in `/v1/plugins` even before they are enabled;
  packages with a static tool contract expose schemas/effects immediately, and
  startup rejects a runtime handshake that differs from that authority.
- Every package outside the host-compiled legacy allowlist must declare both v1
  version fields and an explicit `[contributions]` table. If it exposes tools,
  their complete schemas/effects must be inline or supplied by
  `tool_contract_file`; name-only declarations are rejected.
- The `enabled` setting is convention-based: if the plugin schema defines an `enabled` field with a default, that default controls startup when the user has not explicitly configured the plugin.
- If the plugin path exists but is a file instead of a directory, discovery fails fast so startup does not silently ignore a broken portable install.
- The default plugin location now matches config/database portability: it lives beside `ponderer_config.toml` and `ponderer_memory.db`, not the shell working directory.
- Serde still defaults absent version fields so old wire documents can be read,
  but catalog admission does not treat those defaults as package authority.
- The temporary legacy adapter is selected by the host, not the package: only a
  wholly pre-versioning manifest in the exact `browser-orb`, `image-orb`, or
  `voice-orb` package slot is admitted. A partial migration is rejected; add
  `manifest_version = 1`, `protocol_version = 1`, `[contributions]`, and exact
  static tool schemas/effects together.
- Legacy warnings are emitted on initial discovery and when an allowlisted
  package is added or updated, rather than on every one-second rescan.
- Directory entries are sorted before loading, so duplicate IDs resolve deterministically to the first package while producing an error log.
- A failed root-directory scan preserves the prior catalog. A successful scan may omit malformed individual bundles, making their prior IDs appear removed so the supervisor can stop stale code safely.
- Invalid-package diagnostics are remembered and logged only when they first appear or change; a successful repair is logged once. This prevents one malformed package from producing an error every one-second refresh.
