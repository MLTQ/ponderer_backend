use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::config::AgentConfig;
use crate::plugin::{
    BackendPluginKind, BackendPluginManifest, PluginSettingsFieldManifest,
    PluginSettingsSchemaManifest, PluginSettingsTabManifest,
};

const DEFAULT_PLUGIN_DIR: &str = "plugins";
const DEFAULT_SETTINGS_SCHEMA_FILE: &str = "settings.schema.json";

#[derive(Debug, Clone, Deserialize)]
struct PluginTypeProbe {
    plugin_type: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RuntimeProcessPluginFile {
    id: String,
    name: String,
    version: String,
    description: String,
    plugin_type: String,
    command: Vec<String>,
    #[serde(default)]
    settings_schema_file: Option<String>,
    #[serde(default)]
    settings_tab_title: Option<String>,
    #[serde(default)]
    settings_tab_order: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct RuntimeProcessLaunchSpec {
    pub command: Vec<String>,
    pub working_directory: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RuntimeProcessPluginBundle {
    manifest: BackendPluginManifest,
    launch: RuntimeProcessLaunchSpec,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeProcessPluginCatalog {
    bundles: HashMap<String, RuntimeProcessPluginBundle>,
}

impl RuntimeProcessPluginCatalog {
    pub fn discover() -> Result<Self> {
        Self::discover_from_dir(ensure_plugin_dir()?)
    }

    pub fn discover_from_dir(path: PathBuf) -> Result<Self> {
        ensure_directory(&path)?;

        let mut bundles = HashMap::new();
        for entry in fs::read_dir(&path)
            .with_context(|| format!("Failed to read runtime plugin directory {:?}", path))?
        {
            let entry = entry.context("Failed to read runtime plugin entry")?;
            let plugin_dir = entry.path();
            if !plugin_dir.is_dir() {
                continue;
            }

            let plugin_file = plugin_dir.join("plugin.toml");
            if !plugin_file.is_file() {
                continue;
            }

            if !is_runtime_process_plugin(&plugin_file)? {
                continue;
            }

            match RuntimeProcessPluginBundle::load_from_dir(&plugin_dir) {
                Ok(bundle) => {
                    tracing::info!("Loaded runtime plugin bundle '{}'", bundle.id());
                    bundles.insert(bundle.id().to_string(), bundle);
                }
                Err(error) => {
                    tracing::error!(
                        "Failed to load runtime plugin bundle from {:?}: {}",
                        plugin_dir,
                        error
                    );
                }
            }
        }

        Ok(Self { bundles })
    }

    pub fn manifests(&self) -> Vec<BackendPluginManifest> {
        let mut manifests = self
            .bundles
            .values()
            .map(|bundle| bundle.manifest.clone())
            .collect::<Vec<_>>();
        manifests.sort_by(|left, right| left.name.cmp(&right.name));
        manifests
    }

    pub fn plugin_ids(&self) -> Vec<String> {
        let mut ids = self.bundles.keys().cloned().collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn get(&self, plugin_id: &str) -> Option<&RuntimeProcessPluginBundle> {
        self.bundles.get(plugin_id)
    }
}

impl RuntimeProcessPluginBundle {
    fn load_from_dir(dir: &Path) -> Result<Self> {
        let plugin_file = dir.join("plugin.toml");
        let raw = fs::read_to_string(&plugin_file)
            .with_context(|| format!("Failed to read {:?}", plugin_file))?;
        let spec: RuntimeProcessPluginFile =
            toml::from_str(&raw).with_context(|| format!("Failed to parse {:?}", plugin_file))?;

        if spec.plugin_type.trim() != "runtime_process" {
            anyhow::bail!(
                "Unsupported runtime plugin type '{}' (expected 'runtime_process')",
                spec.plugin_type
            );
        }
        if spec.command.is_empty() {
            anyhow::bail!("Runtime plugin command cannot be empty");
        }

        let settings_schema = load_settings_schema(dir, spec.settings_schema_file.as_deref())?;
        let settings_tab = if settings_schema.is_some() || spec.settings_tab_title.is_some() {
            Some(PluginSettingsTabManifest {
                id: format!("plugin.{}", spec.id),
                title: spec
                    .settings_tab_title
                    .clone()
                    .unwrap_or_else(|| spec.name.clone()),
                order: spec.settings_tab_order.unwrap_or(320),
            })
        } else {
            None
        };

        let manifest = BackendPluginManifest {
            id: spec.id.clone(),
            kind: BackendPluginKind::RuntimeProcessBundle,
            name: spec.name.clone(),
            version: spec.version.clone(),
            description: spec.description.clone(),
            provided_tools: Vec::new(),
            provided_skills: Vec::new(),
            settings_tab,
            settings_schema,
        };

        Ok(Self {
            manifest,
            launch: RuntimeProcessLaunchSpec {
                command: resolve_command_tokens(dir, &spec.command)?,
                working_directory: dir.to_path_buf(),
            },
        })
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn manifest(&self) -> &BackendPluginManifest {
        &self.manifest
    }

    pub fn manifest_with_tools(&self, tools: &[String]) -> BackendPluginManifest {
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
                Some(default_path)
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
    let path = base.join(trimmed);
    if !path.is_file() {
        anyhow::bail!("Referenced file {:?} does not exist", path);
    }
    Ok(path)
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

    #[test]
    fn runtime_bundle_uses_enabled_default_from_schema() {
        let bundle = RuntimeProcessPluginBundle {
            manifest: BackendPluginManifest {
                id: "qwen3-tts".to_string(),
                kind: BackendPluginKind::RuntimeProcessBundle,
                name: "Qwen3 TTS".to_string(),
                version: "0.1.0".to_string(),
                description: "test".to_string(),
                provided_tools: Vec::new(),
                provided_skills: Vec::new(),
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
            manifest: BackendPluginManifest {
                id: "qwen3-tts".to_string(),
                kind: BackendPluginKind::RuntimeProcessBundle,
                name: "Qwen3 TTS".to_string(),
                version: "0.1.0".to_string(),
                description: "test".to_string(),
                provided_tools: Vec::new(),
                provided_skills: Vec::new(),
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
}
