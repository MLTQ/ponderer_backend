use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::config::AgentConfig;
use crate::plugin_contract::{
    is_supported_plugin_protocol_version, PluginKind, PluginManifest, PluginSettingsFieldManifest,
    PluginSettingsSchemaManifest, PluginSettingsTabManifest, RuntimePluginToolManifest,
    RuntimeProcessPluginPackageManifest, CURRENT_PLUGIN_MANIFEST_VERSION,
    SUPPORTED_PLUGIN_PROTOCOL_VERSIONS,
};

const DEFAULT_PLUGIN_DIR: &str = "plugins";
const DEFAULT_SETTINGS_SCHEMA_FILE: &str = "settings.schema.json";

/// Temporary host-owned compatibility slots for the three pre-contract packages
/// shipped before protocol-v1 manifests became mandatory.
///
/// Both the package ID and its direct-child directory name must match. Package
/// authors cannot opt into legacy runtime authority merely by omitting fields or
/// by borrowing one of these IDs from another directory.
const LEGACY_RUNTIME_PROCESS_PACKAGES: &[(&str, &str)] = &[
    ("browser-orb", "browser-orb"),
    ("image-orb", "image-orb"),
    ("voice-orb", "voice-orb"),
];

#[derive(Debug, Clone, Deserialize)]
struct PluginTypeProbe {
    plugin_type: String,
}

#[derive(Debug, Deserialize)]
struct ToolContractDocument {
    #[serde(default)]
    tools: Vec<RuntimePluginToolManifest>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeProcessLaunchSpec {
    pub command: Vec<String>,
    pub working_directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeProcessPluginBundle {
    manifest: PluginManifest,
    launch: RuntimeProcessLaunchSpec,
}

#[derive(Debug)]
pub struct RuntimeProcessPluginCatalog {
    directory: PathBuf,
    bundles: RwLock<HashMap<String, RuntimeProcessPluginBundle>>,
    diagnostics: RwLock<HashMap<PathBuf, String>>,
}

struct RuntimeProcessCatalogScan {
    bundles: HashMap<String, RuntimeProcessPluginBundle>,
    diagnostics: HashMap<PathBuf, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeProcessCatalogRefresh {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
}

impl RuntimeProcessCatalogRefresh {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

impl RuntimeProcessPluginCatalog {
    pub fn discover() -> Result<Self> {
        Self::discover_from_dir(ensure_plugin_dir()?)
    }

    pub fn discover_from_dir(path: PathBuf) -> Result<Self> {
        ensure_directory(&path)?;
        let scan = scan_runtime_process_plugins(&path)?;
        for (plugin_id, bundle) in &scan.bundles {
            tracing::info!("Loaded runtime plugin bundle '{}'", plugin_id);
            log_legacy_authority_compatibility(bundle);
        }
        for (plugin_path, error) in &scan.diagnostics {
            tracing::error!(
                "Runtime plugin package {:?} is invalid: {}",
                plugin_path,
                error
            );
        }
        Ok(Self {
            directory: path,
            bundles: RwLock::new(scan.bundles),
            diagnostics: RwLock::new(scan.diagnostics),
        })
    }

    /// Rescans the package directory and atomically publishes a valid new snapshot.
    pub fn refresh(&self) -> Result<RuntimeProcessCatalogRefresh> {
        let scan = scan_runtime_process_plugins(&self.directory)?;
        let current = self
            .bundles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut refresh = RuntimeProcessCatalogRefresh::default();
        for (plugin_id, bundle) in &scan.bundles {
            match current.get(plugin_id) {
                None => refresh.added.push(plugin_id.clone()),
                Some(previous) if previous != bundle => refresh.updated.push(plugin_id.clone()),
                Some(_) => {}
            }
        }
        for plugin_id in current.keys() {
            if !scan.bundles.contains_key(plugin_id) {
                refresh.removed.push(plugin_id.clone());
            }
        }
        drop(current);

        refresh.added.sort();
        refresh.updated.sort();
        refresh.removed.sort();
        for plugin_id in refresh.added.iter().chain(&refresh.updated) {
            if let Some(bundle) = scan.bundles.get(plugin_id) {
                log_legacy_authority_compatibility(bundle);
            }
        }
        *self
            .bundles
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = scan.bundles;

        let mut diagnostics = self
            .diagnostics
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (plugin_path, error) in &scan.diagnostics {
            if diagnostics.get(plugin_path) != Some(error) {
                tracing::error!(
                    "Runtime plugin package {:?} is invalid: {}",
                    plugin_path,
                    error
                );
            }
        }
        for plugin_path in diagnostics.keys() {
            if !scan.diagnostics.contains_key(plugin_path) {
                tracing::info!("Runtime plugin package {:?} is valid again", plugin_path);
            }
        }
        *diagnostics = scan.diagnostics;
        Ok(refresh)
    }

    pub fn manifests(&self) -> Vec<PluginManifest> {
        let mut manifests = self
            .bundles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|bundle| bundle.manifest.clone())
            .collect::<Vec<_>>();
        manifests.sort_by(|left, right| left.name.cmp(&right.name));
        manifests
    }

    pub fn plugin_ids(&self) -> Vec<String> {
        let mut ids = self
            .bundles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn get(&self, plugin_id: &str) -> Option<RuntimeProcessPluginBundle> {
        self.bundles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(plugin_id)
            .cloned()
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl Default for RuntimeProcessPluginCatalog {
    fn default() -> Self {
        Self {
            directory: plugin_dir_path(),
            bundles: RwLock::new(HashMap::new()),
            diagnostics: RwLock::new(HashMap::new()),
        }
    }
}

impl RuntimeProcessPluginBundle {
    pub(crate) fn load_from_dir(dir: &Path) -> Result<Self> {
        let plugin_file = dir.join("plugin.toml");
        let raw = fs::read_to_string(&plugin_file)
            .with_context(|| format!("Failed to read {:?}", plugin_file))?;
        let manifest_document: toml::Value =
            toml::from_str(&raw).with_context(|| format!("Failed to parse {:?}", plugin_file))?;
        let spec: RuntimeProcessPluginPackageManifest =
            toml::from_str(&raw).with_context(|| format!("Failed to parse {:?}", plugin_file))?;

        validate_authority_contract_mode(dir, &manifest_document, &spec.plugin)?;

        if spec.plugin.kind != PluginKind::RuntimeProcessBundle {
            anyhow::bail!(
                "Unsupported runtime plugin kind '{:?}' (expected runtime_process_bundle)",
                spec.plugin.kind
            );
        }
        if spec.plugin.manifest_version != CURRENT_PLUGIN_MANIFEST_VERSION {
            anyhow::bail!(
                "Unsupported plugin manifest version {} (host supports {})",
                spec.plugin.manifest_version,
                CURRENT_PLUGIN_MANIFEST_VERSION
            );
        }
        if !is_supported_plugin_protocol_version(spec.plugin.protocol_version) {
            anyhow::bail!(
                "Unsupported runtime plugin protocol version {} (host supports {:?})",
                spec.plugin.protocol_version,
                SUPPORTED_PLUGIN_PROTOCOL_VERSIONS
            );
        }
        if spec.command.is_empty() {
            anyhow::bail!("Runtime plugin command cannot be empty");
        }

        let settings_schema = load_settings_schema(dir, spec.settings_schema_file.as_deref())?;
        let settings_tab = if settings_schema.is_some() || spec.settings_tab_title.is_some() {
            Some(PluginSettingsTabManifest {
                id: format!("plugin.{}", spec.plugin.id),
                title: spec
                    .settings_tab_title
                    .clone()
                    .unwrap_or_else(|| spec.plugin.name.clone()),
                order: spec.settings_tab_order.unwrap_or(320),
            })
        } else {
            None
        };
        let command = resolve_command_tokens(dir, &spec.command)?;
        let tools =
            load_tool_contract(dir, spec.tool_contract_file.as_deref(), &spec.plugin.tools)?;

        let mut manifest = spec.plugin;
        manifest.tools = tools;
        if manifest.contributions.is_some()
            && manifest.tools.is_empty()
            && !manifest.provided_tools.is_empty()
        {
            anyhow::bail!(
                "Strict protocol-v1 plugin '{}' declares tool names without a structured static tool contract",
                manifest.id
            );
        }
        let structured_tool_names = manifest
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        if manifest.provided_tools.is_empty() {
            manifest.provided_tools = structured_tool_names;
        } else if !structured_tool_names.is_empty()
            && manifest.provided_tools.iter().collect::<HashSet<_>>()
                != structured_tool_names.iter().collect::<HashSet<_>>()
        {
            anyhow::bail!(
                "Structured tool contract names do not match provided_tools for plugin '{}'",
                manifest.id
            );
        }
        let mut structured_effects = Vec::new();
        let mut effects_by_id = HashMap::new();
        for effect in manifest.tools.iter().flat_map(|tool| &tool.effects) {
            if let Some(previous) = effects_by_id.get(&effect.id) {
                if previous != effect {
                    anyhow::bail!(
                        "Conflicting declarations for effect '{}' in plugin '{}' tool contract",
                        effect.id,
                        manifest.id
                    );
                }
                continue;
            }
            effects_by_id.insert(effect.id.clone(), effect.clone());
            structured_effects.push(effect.clone());
        }
        if manifest.declared_effects.is_empty() {
            manifest.declared_effects = structured_effects;
        } else {
            let declared_effect_ids = manifest
                .declared_effects
                .iter()
                .map(|effect| effect.id.as_str())
                .collect::<HashSet<_>>();
            if let Some(effect) = effects_by_id
                .keys()
                .find(|effect_id| !declared_effect_ids.contains(effect_id.as_str()))
            {
                anyhow::bail!(
                    "Tool contract effect '{}' is absent from declared_effects for plugin '{}'",
                    effect,
                    manifest.id
                );
            }
        }
        manifest.settings_tab = settings_tab;
        manifest.settings_schema = settings_schema;

        Ok(Self {
            manifest,
            launch: RuntimeProcessLaunchSpec {
                command,
                working_directory: dir.to_path_buf(),
            },
        })
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn manifest_with_tools(&self, tools: &[String]) -> PluginManifest {
        let mut manifest = self.manifest.clone();
        manifest.provided_tools = tools.to_vec();
        manifest
    }

    pub fn launch_spec(&self) -> &RuntimeProcessLaunchSpec {
        &self.launch
    }

    pub fn is_enabled(&self, config: &AgentConfig) -> bool {
        let configured = config
            .plugin_settings
            .get(self.id())
            .and_then(Value::as_object)
            .and_then(|settings| settings.get("enabled"))
            .and_then(Value::as_bool);
        if let Some(enabled) = configured {
            return enabled;
        }

        self.default_setting_value("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
    }

    fn default_setting_value(&self, key: &str) -> Option<Value> {
        self.manifest
            .settings_schema
            .as_ref()
            .and_then(|schema| schema.fields.iter().find(|field| field.key == key))
            .and_then(|field| field.default_value.clone())
    }
}

fn validate_authority_contract_mode(
    dir: &Path,
    document: &toml::Value,
    manifest: &PluginManifest,
) -> Result<()> {
    let declares_manifest_version =
        document.get("manifest_version").is_some() || document.get("schema_version").is_some();
    let declares_protocol_version = document.get("protocol_version").is_some()
        || document.get("runtime_protocol_version").is_some();
    let declares_contributions = document.get("contributions").is_some();

    if declares_manifest_version && declares_protocol_version && declares_contributions {
        return Ok(());
    }

    let is_pre_versioning_manifest =
        !declares_manifest_version && !declares_protocol_version && !declares_contributions;
    if is_pre_versioning_manifest && is_host_approved_legacy_package(dir, &manifest.id) {
        return Ok(());
    }

    anyhow::bail!(
        "Runtime plugin '{}' must explicitly declare manifest_version, protocol_version, and a static [contributions] authority contract; field omission is reserved for host-approved legacy package slots",
        manifest.id
    )
}

fn is_host_approved_legacy_package(dir: &Path, plugin_id: &str) -> bool {
    let Some(directory_name) = dir.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    LEGACY_RUNTIME_PROCESS_PACKAGES
        .iter()
        .any(|(allowed_id, allowed_directory)| {
            plugin_id == *allowed_id && directory_name == *allowed_directory
        })
}

fn log_legacy_authority_compatibility(bundle: &RuntimeProcessPluginBundle) {
    if bundle.manifest.contributions.is_none() {
        tracing::warn!(
            "Runtime plugin '{}' is using its temporary host-approved legacy authority adapter; migrate it to explicit protocol-v1 versions, [contributions], and static tool contracts",
            bundle.id()
        );
    }
}

pub type SharedRuntimeProcessPluginCatalog = Arc<RuntimeProcessPluginCatalog>;

pub fn plugin_dir_path() -> PathBuf {
    if let Ok(raw) = std::env::var("PONDERER_PLUGIN_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    AgentConfig::config_path()
        .parent()
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_PLUGIN_DIR)
}

pub fn ensure_plugin_dir() -> Result<PathBuf> {
    let path = plugin_dir_path();
    ensure_directory(&path)?;
    Ok(path)
}

fn scan_runtime_process_plugins(path: &Path) -> Result<RuntimeProcessCatalogScan> {
    ensure_directory(path)?;
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("Failed to read runtime plugin directory {:?}", path))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to read runtime plugin entry")?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut bundles = HashMap::new();
    let mut diagnostics = HashMap::new();
    for entry in entries {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }

        let plugin_file = plugin_dir.join("plugin.toml");
        if !plugin_file.is_file() {
            continue;
        }

        match is_runtime_process_plugin(&plugin_file) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                diagnostics.insert(plugin_dir.clone(), format!("{error:#}"));
                continue;
            }
        }

        match RuntimeProcessPluginBundle::load_from_dir(&plugin_dir) {
            Ok(bundle) => {
                if bundles.contains_key(bundle.id()) {
                    diagnostics.insert(
                        plugin_dir.clone(),
                        format!("duplicate runtime plugin id '{}'", bundle.id()),
                    );
                    continue;
                }
                bundles.insert(bundle.id().to_string(), bundle);
            }
            Err(error) => {
                diagnostics.insert(plugin_dir.clone(), format!("{error:#}"));
            }
        }
    }

    Ok(RuntimeProcessCatalogScan {
        bundles,
        diagnostics,
    })
}

fn is_runtime_process_plugin(plugin_file: &Path) -> Result<bool> {
    let raw = fs::read_to_string(plugin_file)
        .with_context(|| format!("Failed to read {:?}", plugin_file))?;
    let probe: PluginTypeProbe =
        toml::from_str(&raw).with_context(|| format!("Failed to parse {:?}", plugin_file))?;
    Ok(probe.plugin_type.trim() == "runtime_process")
}

fn load_settings_schema(
    dir: &Path,
    configured_name: Option<&str>,
) -> Result<Option<PluginSettingsSchemaManifest>> {
    let schema_path = match configured_name {
        Some(file_name) => Some(resolve_relative_file(dir, file_name)?),
        None => {
            let default_path = dir.join(DEFAULT_SETTINGS_SCHEMA_FILE);
            if default_path.is_file() {
                Some(resolve_relative_file(dir, DEFAULT_SETTINGS_SCHEMA_FILE)?)
            } else {
                None
            }
        }
    };

    let Some(schema_path) = schema_path else {
        return Ok(None);
    };

    let settings_schema: PluginSettingsSchemaManifest = serde_json::from_str(
        &fs::read_to_string(&schema_path)
            .with_context(|| format!("Failed to read {:?}", schema_path))?,
    )
    .with_context(|| format!("Failed to parse {:?}", schema_path))?;

    validate_schema(&settings_schema.fields)?;
    Ok(Some(settings_schema))
}

fn load_tool_contract(
    dir: &Path,
    configured_name: Option<&str>,
    inline_tools: &[RuntimePluginToolManifest],
) -> Result<Vec<RuntimePluginToolManifest>> {
    let tools = match configured_name {
        Some(file_name) => {
            if !inline_tools.is_empty() {
                anyhow::bail!(
                    "Plugin tool contracts must use either inline tools or tool_contract_file, not both"
                );
            }
            let contract_path = resolve_relative_file(dir, file_name)?;
            let document: ToolContractDocument = serde_json::from_str(
                &fs::read_to_string(&contract_path)
                    .with_context(|| format!("Failed to read {:?}", contract_path))?,
            )
            .with_context(|| format!("Failed to parse {:?}", contract_path))?;
            document.tools
        }
        None => inline_tools.to_vec(),
    };

    let mut names = HashSet::new();
    for tool in &tools {
        if tool.name.trim().is_empty() {
            anyhow::bail!("Plugin tool name cannot be empty");
        }
        if !names.insert(tool.name.as_str()) {
            anyhow::bail!("Duplicate plugin tool contract name '{}'", tool.name);
        }
    }
    Ok(tools)
}

fn validate_schema(fields: &[PluginSettingsFieldManifest]) -> Result<()> {
    let mut seen = HashMap::new();
    for field in fields {
        let count = seen.entry(field.key.as_str()).or_insert(0usize);
        *count += 1;
        if *count > 1 {
            anyhow::bail!("Duplicate plugin settings field key '{}'", field.key);
        }
    }
    Ok(())
}

fn resolve_relative_file(base: &Path, rel: &str) -> Result<PathBuf> {
    let trimmed = rel.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Plugin file reference cannot be empty");
    }
    let relative = Path::new(trimmed);
    if relative.is_absolute()
        || relative.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        anyhow::bail!("Plugin file reference must stay inside the package: {trimmed:?}");
    }
    let path = base.join(relative);
    let canonical_base = base
        .canonicalize()
        .with_context(|| format!("Failed to resolve plugin package {:?}", base))?;
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("Referenced file {:?} does not exist", path))?;
    if !canonical_path.starts_with(&canonical_base) || !canonical_path.is_file() {
        anyhow::bail!("Referenced file {:?} must be a package file", path);
    }
    Ok(canonical_path)
}

fn resolve_command_tokens(base: &Path, tokens: &[String]) -> Result<Vec<String>> {
    if tokens.is_empty() {
        anyhow::bail!("Runtime plugin command cannot be empty");
    }

    let mut resolved = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Runtime plugin command token cannot be empty");
        }

        if should_resolve_as_path(trimmed) {
            let candidate = base.join(trimmed);
            if index == 0 && !candidate.exists() {
                anyhow::bail!("Runtime plugin executable {:?} does not exist", candidate);
            }
            if candidate.exists() {
                resolved.push(candidate.to_string_lossy().to_string());
                continue;
            }
        }

        resolved.push(trimmed.to_string());
    }

    Ok(resolved)
}

fn should_resolve_as_path(token: &str) -> bool {
    token.starts_with('.')
        || token.starts_with('/')
        || token.contains(std::path::MAIN_SEPARATOR)
        || token.contains('/')
}

fn ensure_directory(path: &Path) -> Result<()> {
    if path.exists() {
        if !path.is_dir() {
            anyhow::bail!("Plugin path {:?} exists but is not a directory", path);
        }
        return Ok(());
    }

    fs::create_dir_all(path)
        .with_context(|| format!("Failed to create plugin directory {:?}", path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{
        PluginSettingsFieldKind, PluginSettingsFieldManifest, PluginSettingsSchemaManifest,
    };

    fn write_test_plugin(root: &Path, directory: &str, id: &str, version: &str) {
        let plugin_dir = root.join(directory);
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                r#"
                    manifest_version = 1
                    protocol_version = 1
                    id = "{id}"
                    name = "{id}"
                    version = "{version}"
                    description = "fixture"
                    plugin_type = "runtime_process"
                    command = ["python3"]

                    [contributions]
                    event_hooks = []
                    prompt_slots = []
                    poll_events = false
                "#
            ),
        )
        .unwrap();
    }

    #[test]
    fn runtime_bundle_uses_enabled_default_from_schema() {
        let bundle = RuntimeProcessPluginBundle {
            manifest: PluginManifest {
                manifest_version: CURRENT_PLUGIN_MANIFEST_VERSION,
                protocol_version: crate::plugin_contract::CURRENT_PLUGIN_PROTOCOL_VERSION,
                id: "qwen3-tts".to_string(),
                kind: PluginKind::RuntimeProcessBundle,
                name: "Qwen3 TTS".to_string(),
                version: "0.1.0".to_string(),
                description: "test".to_string(),
                provided_tools: Vec::new(),
                tools: Vec::new(),
                provided_skills: Vec::new(),
                requested_capabilities: Vec::new(),
                declared_effects: Vec::new(),
                contributions: None,
                settings_tab: None,
                settings_schema: Some(PluginSettingsSchemaManifest {
                    fields: vec![PluginSettingsFieldManifest {
                        key: "enabled".to_string(),
                        title: "Enabled".to_string(),
                        kind: PluginSettingsFieldKind::Boolean,
                        help: None,
                        required: false,
                        default_value: Some(Value::Bool(false)),
                        options: Vec::new(),
                    }],
                }),
            },
            launch: RuntimeProcessLaunchSpec {
                command: vec!["python3".to_string()],
                working_directory: PathBuf::from("."),
            },
        };

        assert!(!bundle.is_enabled(&AgentConfig::default()));
    }

    #[test]
    fn runtime_bundle_manifest_with_tools_replaces_provided_tools() {
        let bundle = RuntimeProcessPluginBundle {
            manifest: PluginManifest {
                manifest_version: CURRENT_PLUGIN_MANIFEST_VERSION,
                protocol_version: crate::plugin_contract::CURRENT_PLUGIN_PROTOCOL_VERSION,
                id: "qwen3-tts".to_string(),
                kind: PluginKind::RuntimeProcessBundle,
                name: "Qwen3 TTS".to_string(),
                version: "0.1.0".to_string(),
                description: "test".to_string(),
                provided_tools: Vec::new(),
                tools: Vec::new(),
                provided_skills: Vec::new(),
                requested_capabilities: Vec::new(),
                declared_effects: Vec::new(),
                contributions: None,
                settings_tab: None,
                settings_schema: None,
            },
            launch: RuntimeProcessLaunchSpec {
                command: vec!["python3".to_string()],
                working_directory: PathBuf::from("."),
            },
        };

        let manifest = bundle.manifest_with_tools(&["speak_text".to_string()]);
        assert_eq!(manifest.provided_tools, vec!["speak_text".to_string()]);
    }

    #[test]
    fn discover_from_dir_creates_missing_plugin_directory() {
        let path = std::env::temp_dir().join(format!(
            "ponderer-runtime-plugin-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);

        let catalog = RuntimeProcessPluginCatalog::discover_from_dir(path.clone())
            .expect("catalog discovery should create directory");

        assert!(path.is_dir());
        assert!(catalog.plugin_ids().is_empty());

        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn serde_defaults_remain_wire_compatible_for_legacy_manifests() {
        let spec: RuntimeProcessPluginPackageManifest = toml::from_str(
            r#"
                id = "legacy"
                name = "Legacy"
                version = "0.1.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3"]
            "#,
        )
        .expect("legacy plugin manifest should decode");

        assert_eq!(
            spec.plugin.manifest_version,
            CURRENT_PLUGIN_MANIFEST_VERSION
        );
        assert_eq!(
            spec.plugin.protocol_version,
            crate::plugin_contract::CURRENT_PLUGIN_PROTOCOL_VERSION
        );
    }

    #[test]
    fn refresh_reports_atomic_add_update_and_remove_changes() {
        let root = tempfile::tempdir().unwrap();
        let catalog = RuntimeProcessPluginCatalog::discover_from_dir(root.path().to_path_buf())
            .expect("empty catalog");

        write_test_plugin(root.path(), "alpha", "alpha", "0.1.0");
        assert_eq!(
            catalog.refresh().unwrap(),
            RuntimeProcessCatalogRefresh {
                added: vec!["alpha".to_string()],
                updated: Vec::new(),
                removed: Vec::new(),
            }
        );
        assert_eq!(catalog.get("alpha").unwrap().manifest().version, "0.1.0");

        write_test_plugin(root.path(), "alpha", "alpha", "0.2.0");
        assert_eq!(catalog.refresh().unwrap().updated, vec!["alpha"]);
        assert_eq!(catalog.get("alpha").unwrap().manifest().version, "0.2.0");

        fs::remove_dir_all(root.path().join("alpha")).unwrap();
        assert_eq!(catalog.refresh().unwrap().removed, vec!["alpha"]);
        assert!(catalog.get("alpha").is_none());
    }

    #[test]
    fn malformed_neighbor_does_not_hide_valid_plugin() {
        let root = tempfile::tempdir().unwrap();
        let malformed_dir = root.path().join("malformed");
        fs::create_dir_all(&malformed_dir).unwrap();
        fs::write(
            malformed_dir.join("plugin.toml"),
            "this is not = valid toml",
        )
        .unwrap();
        write_test_plugin(root.path(), "valid", "valid", "1.0.0");

        let catalog = RuntimeProcessPluginCatalog::discover_from_dir(root.path().to_path_buf())
            .expect("one malformed package should not abort discovery");

        assert_eq!(catalog.plugin_ids(), vec!["valid"]);
        assert_eq!(catalog.directory(), root.path());
    }

    #[test]
    fn json_tool_contract_populates_structured_and_compatibility_manifests() {
        let root = tempfile::tempdir().unwrap();
        let plugin_dir = root.path().join("structured");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("tools.json"),
            r#"{
                "tools": [{
                    "name": "fixture.read",
                    "description": "read fixture",
                    "parameters": {"type": "object", "properties": {}},
                    "effects": [{"id": "network.read"}]
                }]
            }"#,
        )
        .unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
                manifest_version = 1
                protocol_version = 1
                id = "structured"
                name = "Structured"
                version = "1.0.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3"]
                tool_contract_file = "tools.json"

                [contributions]
                event_hooks = []
                prompt_slots = []
                poll_events = false
            "#,
        )
        .unwrap();

        let catalog = RuntimeProcessPluginCatalog::discover_from_dir(root.path().to_path_buf())
            .expect("structured tool contract should load");
        let manifest = catalog.get("structured").unwrap().manifest().clone();
        assert_eq!(manifest.provided_tools, vec!["fixture.read"]);
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.tools[0].effects[0].id, "network.read");
        assert_eq!(manifest.declared_effects[0].id, "network.read");
    }

    #[test]
    fn duplicate_json_tool_contract_names_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let contract_path = root.path().join("tools.json");
        fs::write(
            &contract_path,
            r#"{
                "tools": [
                    {"name": "same", "description": "one"},
                    {"name": "same", "description": "two"}
                ]
            }"#,
        )
        .unwrap();

        let error = load_tool_contract(root.path(), Some("tools.json"), &[]).unwrap_err();
        assert!(error.to_string().contains("Duplicate plugin tool contract"));
    }

    #[test]
    fn strict_v1_package_requires_structured_tool_contracts() {
        let root = tempfile::tempdir().unwrap();
        let plugin_dir = root.path().join("strict");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
                manifest_version = 1
                protocol_version = 1
                id = "strict"
                name = "Strict"
                version = "1.0.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3"]
                provided_tools = ["strict.read"]

                [contributions]
                event_hooks = []
                prompt_slots = []
                poll_events = false
            "#,
        )
        .unwrap();

        let error = RuntimeProcessPluginBundle::load_from_dir(&plugin_dir).unwrap_err();
        assert!(error
            .to_string()
            .contains("structured static tool contract"));
    }

    #[test]
    fn non_allowlisted_package_cannot_downgrade_to_legacy_authority() {
        let root = tempfile::tempdir().unwrap();
        let plugin_dir = root.path().join("downgrade");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
                id = "downgrade"
                name = "Downgrade"
                version = "1.0.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3"]
            "#,
        )
        .unwrap();

        let error = RuntimeProcessPluginBundle::load_from_dir(&plugin_dir).unwrap_err();
        assert!(error.to_string().contains("host-approved legacy package"));
    }

    #[test]
    fn host_allowlist_admits_only_exact_legacy_id_and_directory_pair() {
        let root = tempfile::tempdir().unwrap();
        let plugin_dir = root.path().join("browser-orb");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
                id = "browser-orb"
                name = "Browser Orb"
                version = "0.1.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3"]
            "#,
        )
        .unwrap();

        let bundle = RuntimeProcessPluginBundle::load_from_dir(&plugin_dir)
            .expect("the exact host-owned legacy slot should remain compatible");
        assert_eq!(bundle.id(), "browser-orb");
        assert!(bundle.manifest().contributions.is_none());

        let borrowed_id_dir = root.path().join("not-browser-orb");
        fs::create_dir_all(&borrowed_id_dir).unwrap();
        fs::copy(
            plugin_dir.join("plugin.toml"),
            borrowed_id_dir.join("plugin.toml"),
        )
        .unwrap();
        let error = RuntimeProcessPluginBundle::load_from_dir(&borrowed_id_dir).unwrap_err();
        assert!(error.to_string().contains("host-approved legacy package"));

        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
                id = "borrowed"
                name = "Borrowed"
                version = "0.1.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3"]
            "#,
        )
        .unwrap();
        let error = RuntimeProcessPluginBundle::load_from_dir(&plugin_dir).unwrap_err();
        assert!(error.to_string().contains("host-approved legacy package"));
    }

    #[test]
    fn partially_versioned_allowlisted_package_must_finish_strict_migration() {
        let root = tempfile::tempdir().unwrap();
        let plugin_dir = root.path().join("voice-orb");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
                manifest_version = 1
                protocol_version = 1
                id = "voice-orb"
                name = "Voice Orb"
                version = "0.1.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3"]
            "#,
        )
        .unwrap();

        let error = RuntimeProcessPluginBundle::load_from_dir(&plugin_dir).unwrap_err();
        assert!(error.to_string().contains("static [contributions]"));
    }

    #[test]
    fn strict_package_requires_both_explicit_version_markers() {
        let root = tempfile::tempdir().unwrap();
        for (directory, version_fields) in [
            ("missing-manifest-version", "protocol_version = 1"),
            ("missing-protocol-version", "manifest_version = 1"),
        ] {
            let plugin_dir = root.path().join(directory);
            fs::create_dir_all(&plugin_dir).unwrap();
            fs::write(
                plugin_dir.join("plugin.toml"),
                format!(
                    r#"
                        {version_fields}
                        id = "{directory}"
                        name = "Incomplete"
                        version = "0.1.0"
                        description = "fixture"
                        plugin_type = "runtime_process"
                        command = ["python3"]

                        [contributions]
                        event_hooks = []
                        prompt_slots = []
                        poll_events = false
                    "#
                ),
            )
            .unwrap();

            let error = RuntimeProcessPluginBundle::load_from_dir(&plugin_dir).unwrap_err();
            assert!(error.to_string().contains("manifest_version"));
            assert!(error.to_string().contains("protocol_version"));
        }
    }

    #[test]
    fn referenced_contract_files_cannot_escape_the_package() {
        let root = tempfile::tempdir().unwrap();
        let plugin_dir = root.path().join("confined");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(root.path().join("outside-tools.json"), r#"{"tools": []}"#).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
                manifest_version = 1
                protocol_version = 1
                id = "confined"
                name = "Confined"
                version = "1.0.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3"]
                tool_contract_file = "../outside-tools.json"

                [contributions]
                event_hooks = []
                prompt_slots = []
                poll_events = false
            "#,
        )
        .unwrap();

        let error = RuntimeProcessPluginBundle::load_from_dir(&plugin_dir).unwrap_err();
        assert!(error.to_string().contains("stay inside the package"));
    }
}
