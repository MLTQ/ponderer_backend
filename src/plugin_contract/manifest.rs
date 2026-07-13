use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::events::PromptContributionSlot;
use super::{
    default_plugin_manifest_version, default_plugin_protocol_version, PluginEffectDeclaration,
    RuntimePluginToolManifest,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginSettingsTabManifest {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub order: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    #[serde(alias = "Builtin")]
    Builtin,
    #[serde(alias = "runtime_process", alias = "RuntimeProcessBundle")]
    RuntimeProcessBundle,
}

impl Default for PluginKind {
    fn default() -> Self {
        Self::Builtin
    }
}

pub type BackendPluginKind = PluginKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PluginContributionManifest {
    #[serde(default)]
    pub event_hooks: Vec<String>,
    #[serde(default)]
    pub prompt_slots: Vec<PromptContributionSlot>,
    #[serde(default)]
    pub poll_events: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginSettingsFieldKind {
    Boolean,
    Text,
    Multiline,
    Number,
    Select,
    Path,
    Secret,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginSettingsOptionManifest {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginSettingsFieldManifest {
    pub key: String,
    pub title: String,
    pub kind: PluginSettingsFieldKind,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default_value: Option<Value>,
    #[serde(default)]
    pub options: Vec<PluginSettingsOptionManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PluginSettingsSchemaManifest {
    #[serde(default)]
    pub fields: Vec<PluginSettingsFieldManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginManifest {
    #[serde(default = "default_plugin_manifest_version", alias = "schema_version")]
    pub manifest_version: u32,
    #[serde(
        default = "default_plugin_protocol_version",
        alias = "runtime_protocol_version"
    )]
    pub protocol_version: u32,
    pub id: String,
    #[serde(default, alias = "plugin_type")]
    pub kind: PluginKind,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub provided_tools: Vec<String>,
    #[serde(default)]
    pub tools: Vec<RuntimePluginToolManifest>,
    #[serde(default)]
    pub provided_skills: Vec<String>,
    #[serde(default)]
    pub requested_capabilities: Vec<String>,
    #[serde(default)]
    pub declared_effects: Vec<PluginEffectDeclaration>,
    #[serde(default)]
    pub contributions: Option<PluginContributionManifest>,
    #[serde(default)]
    pub settings_tab: Option<PluginSettingsTabManifest>,
    #[serde(default)]
    pub settings_schema: Option<PluginSettingsSchemaManifest>,
}

pub type BackendPluginManifest = PluginManifest;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeProcessPluginPackageManifest {
    #[serde(flatten)]
    pub plugin: PluginManifest,
    pub command: Vec<String>,
    #[serde(default)]
    pub tool_contract_file: Option<String>,
    #[serde(default)]
    pub settings_schema_file: Option<String>,
    #[serde(default)]
    pub settings_tab_title: Option<String>,
    #[serde(default)]
    pub settings_tab_order: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_manifest_defaults_to_contract_v1() {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "kind": "runtime_process_bundle",
            "name": "Legacy",
            "version": "0.1.0",
            "description": "pre-versioning fixture"
        }))
        .expect("legacy manifest should decode");

        assert_eq!(manifest.manifest_version, 1);
        assert_eq!(manifest.protocol_version, 1);
        assert!(manifest.provided_tools.is_empty());
        assert!(manifest.requested_capabilities.is_empty());
    }

    #[test]
    fn package_kind_accepts_legacy_plugin_type_names() {
        let kind: PluginKind =
            serde_json::from_str("\"runtime_process\"").expect("legacy kind should decode");
        assert_eq!(kind, PluginKind::RuntimeProcessBundle);
    }

    #[test]
    fn legacy_runtime_package_flattens_into_canonical_manifest() {
        let package: RuntimeProcessPluginPackageManifest = toml::from_str(
            r#"
                id = "legacy"
                name = "Legacy"
                version = "0.1.0"
                description = "fixture"
                plugin_type = "runtime_process"
                command = ["python3", "-m", "legacy"]
                settings_schema_file = "settings.schema.json"
            "#,
        )
        .expect("legacy package should decode");

        assert_eq!(package.plugin.manifest_version, 1);
        assert_eq!(package.plugin.protocol_version, 1);
        assert_eq!(package.plugin.kind, PluginKind::RuntimeProcessBundle);
        assert_eq!(package.command[0], "python3");
    }
}
