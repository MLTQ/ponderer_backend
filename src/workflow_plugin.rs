use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::config::AgentConfig;
use crate::plugin::{
    BackendPluginKind, BackendPluginManifest, PluginSettingsFieldManifest,
    PluginSettingsSchemaManifest, PluginSettingsTabManifest,
};
use crate::runtime_process_plugin::ensure_plugin_dir;

const DEFAULT_SETTINGS_SCHEMA_FILE: &str = "settings.schema.json";

#[derive(Debug, Clone, Deserialize)]
struct PluginTypeProbe {
    plugin_type: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkflowPluginFile {
    id: String,
    name: String,
    version: String,
    description: String,
    plugin_type: String,
    #[serde(default)]
    engine: Option<String>,
    workflow_file: String,
    bindings_file: String,
    #[serde(default)]
    settings_schema_file: Option<String>,
    #[serde(default)]
    settings_tab_title: Option<String>,
    #[serde(default)]
    settings_tab_order: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowPluginBinding {
    pub source: String,
    pub node_id: String,
    pub input_name: String,
    #[serde(default = "default_binding_required")]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowPluginBindings {
    #[serde(default)]
    pub settings: Vec<WorkflowPluginBinding>,
    #[serde(default)]
    pub runtime: Vec<WorkflowPluginBinding>,
}

#[derive(Debug, Clone)]
pub struct WorkflowPluginBundle {
    manifest: BackendPluginManifest,
    workflow_json: Value,
    bindings: WorkflowPluginBindings,
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowPluginCatalog {
    bundles: HashMap<String, WorkflowPluginBundle>,
}

impl WorkflowPluginCatalog {
    pub fn discover() -> Result<Self> {
        Self::discover_from_dir(ensure_plugin_dir()?)
    }

    pub fn discover_from_dir(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            tracing::info!("Workflow plugin directory {:?} does not exist", path);
            return Ok(Self::default());
        }

        let mut bundles = HashMap::new();
        for entry in fs::read_dir(&path)
            .with_context(|| format!("Failed to read workflow plugin directory {:?}", path))?
        {
            let entry = entry.context("Failed to read workflow plugin entry")?;
            let plugin_dir = entry.path();
            if !plugin_dir.is_dir() {
                continue;
            }

            let plugin_file = plugin_dir.join("plugin.toml");
            if !plugin_file.is_file() {
                continue;
            }

            if !is_workflow_plugin(&plugin_file)? {
                continue;
            }

            match WorkflowPluginBundle::load_from_dir(&plugin_dir) {
                Ok(bundle) => {
                    tracing::info!("Loaded workflow plugin bundle '{}'", bundle.id());
                    bundles.insert(bundle.id().to_string(), bundle);
                }
                Err(error) => {
                    tracing::error!(
                        "Failed to load workflow plugin bundle from {:?}: {}",
                        plugin_dir,
                        error
                    );
                }
            }
        }

        Ok(Self { bundles })
    }

    pub fn len(&self) -> usize {
        self.bundles.len()
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

    pub fn get(&self, plugin_id: &str) -> Option<&WorkflowPluginBundle> {
        self.bundles.get(plugin_id)
    }
}

impl WorkflowPluginBundle {
    fn load_from_dir(dir: &Path) -> Result<Self> {
        let plugin_file = dir.join("plugin.toml");
        let raw = fs::read_to_string(&plugin_file)
            .with_context(|| format!("Failed to read {:?}", plugin_file))?;
        let spec: WorkflowPluginFile =
            toml::from_str(&raw).with_context(|| format!("Failed to parse {:?}", plugin_file))?;

        if spec.plugin_type.trim() != "comfy_workflow" {
            anyhow::bail!(
                "Unsupported workflow plugin type '{}' (expected 'comfy_workflow')",
                spec.plugin_type
            );
        }
        if let Some(engine) = &spec.engine {
            if engine.trim() != "comfyui" {
                anyhow::bail!("Unsupported workflow plugin engine '{}'", engine);
            }
        }

        let workflow_path = resolve_relative_file(dir, &spec.workflow_file)?;
        let workflow_json: Value = serde_json::from_str(
            &fs::read_to_string(&workflow_path)
                .with_context(|| format!("Failed to read {:?}", workflow_path))?,
        )
        .with_context(|| format!("Failed to parse {:?}", workflow_path))?;

        let bindings_path = resolve_relative_file(dir, &spec.bindings_file)?;
        let bindings: WorkflowPluginBindings = serde_json::from_str(
            &fs::read_to_string(&bindings_path)
                .with_context(|| format!("Failed to read {:?}", bindings_path))?,
        )
        .with_context(|| format!("Failed to parse {:?}", bindings_path))?;

        let schema_path = resolve_relative_file(
            dir,
            spec.settings_schema_file
                .as_deref()
                .unwrap_or(DEFAULT_SETTINGS_SCHEMA_FILE),
        )?;
        let settings_schema: PluginSettingsSchemaManifest = serde_json::from_str(
            &fs::read_to_string(&schema_path)
                .with_context(|| format!("Failed to read {:?}", schema_path))?,
        )
        .with_context(|| format!("Failed to parse {:?}", schema_path))?;

        validate_bindings(&workflow_json, &bindings, &settings_schema.fields)?;

        let manifest = BackendPluginManifest {
            id: spec.id.clone(),
            kind: BackendPluginKind::WorkflowBundle,
            name: spec.name.clone(),
            version: spec.version.clone(),
            description: spec.description.clone(),
            provided_tools: vec!["run_workflow_plugin".to_string()],
            provided_skills: Vec::new(),
            settings_tab: Some(PluginSettingsTabManifest {
                id: format!("plugin.{}", spec.id),
                title: spec
                    .settings_tab_title
                    .clone()
                    .unwrap_or_else(|| spec.name.clone()),
                order: spec.settings_tab_order.unwrap_or(300),
            }),
            settings_schema: Some(settings_schema),
        };

        Ok(Self {
            manifest,
            workflow_json,
            bindings,
        })
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn prepare_workflow(
        &self,
        config: &AgentConfig,
        runtime_inputs: &Map<String, Value>,
    ) -> Result<Value> {
        let mut workflow = self.workflow_json.clone();

        for binding in &self.bindings.settings {
            let Some(value) = self.resolve_setting_value(config, &binding.source) else {
                if binding.required {
                    anyhow::bail!(
                        "Plugin '{}' is missing required setting '{}'",
                        self.id(),
                        binding.source
                    );
                }
                continue;
            };
            apply_binding(&mut workflow, binding, value)?;
        }

        for binding in &self.bindings.runtime {
            let Some(value) = runtime_inputs.get(&binding.source) else {
                if binding.required {
                    anyhow::bail!(
                        "Plugin '{}' is missing required runtime input '{}'",
                        self.id(),
                        binding.source
                    );
                }
                continue;
            };
            apply_binding(&mut workflow, binding, value.clone())?;
        }

        Ok(workflow)
    }

    fn resolve_setting_value(&self, config: &AgentConfig, key: &str) -> Option<Value> {
        let configured = config
            .plugin_settings
            .get(self.id())
            .and_then(Value::as_object)
            .and_then(|values| values.get(key))
            .cloned();
        if configured.is_some() {
            return configured;
        }

        self.manifest
            .settings_schema
            .as_ref()
            .and_then(|schema| schema.fields.iter().find(|field| field.key == key))
            .and_then(|field| field.default_value.clone())
    }
}

pub type SharedWorkflowPluginCatalog = Arc<WorkflowPluginCatalog>;

fn default_binding_required() -> bool {
    true
}

fn is_workflow_plugin(plugin_file: &Path) -> Result<bool> {
    let raw = fs::read_to_string(plugin_file)
        .with_context(|| format!("Failed to read {:?}", plugin_file))?;
    let probe: PluginTypeProbe =
        toml::from_str(&raw).with_context(|| format!("Failed to parse {:?}", plugin_file))?;
    Ok(probe.plugin_type.trim() == "comfy_workflow")
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

fn validate_bindings(
    workflow_json: &Value,
    bindings: &WorkflowPluginBindings,
    fields: &[PluginSettingsFieldManifest],
) -> Result<()> {
    for binding in bindings.settings.iter().chain(bindings.runtime.iter()) {
        if workflow_input_slot(workflow_json, &binding.node_id, &binding.input_name).is_none() {
            anyhow::bail!(
                "Binding '{}' -> {}.{} does not match a workflow input",
                binding.source,
                binding.node_id,
                binding.input_name
            );
        }
    }

    for binding in &bindings.settings {
        if !fields.iter().any(|field| field.key == binding.source) {
            anyhow::bail!(
                "Setting binding '{}' does not match any settings schema field",
                binding.source
            );
        }
    }

    Ok(())
}

fn apply_binding(
    workflow_json: &mut Value,
    binding: &WorkflowPluginBinding,
    value: Value,
) -> Result<()> {
    let Some(slot) = workflow_input_slot_mut(workflow_json, &binding.node_id, &binding.input_name)
    else {
        anyhow::bail!(
            "Workflow input {}.{} is missing during execution",
            binding.node_id,
            binding.input_name
        );
    };
    *slot = value;
    Ok(())
}

fn workflow_input_slot<'a>(
    workflow_json: &'a Value,
    node_id: &str,
    input_name: &str,
) -> Option<&'a Value> {
    workflow_json
        .get(node_id)
        .and_then(Value::as_object)
        .and_then(|node| node.get("inputs"))
        .and_then(Value::as_object)
        .and_then(|inputs| inputs.get(input_name))
}

fn workflow_input_slot_mut<'a>(
    workflow_json: &'a mut Value,
    node_id: &str,
    input_name: &str,
) -> Option<&'a mut Value> {
    workflow_json
        .get_mut(node_id)
        .and_then(Value::as_object_mut)
        .and_then(|node| node.get_mut("inputs"))
        .and_then(Value::as_object_mut)
        .and_then(|inputs| inputs.get_mut(input_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_bindings_into_nested_workflow_input() {
        let mut workflow = serde_json::json!({
            "10": { "inputs": { "text": "old" } }
        });
        let binding = WorkflowPluginBinding {
            source: "text".to_string(),
            node_id: "10".to_string(),
            input_name: "text".to_string(),
            required: true,
        };

        apply_binding(&mut workflow, &binding, serde_json::json!("new")).unwrap();

        assert_eq!(workflow["10"]["inputs"]["text"], serde_json::json!("new"));
    }
}
