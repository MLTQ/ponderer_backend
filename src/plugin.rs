use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::AgentConfig;
use crate::skills::Skill;
use crate::tools::ToolRegistry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSettingsTabManifest {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub order: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendPluginKind {
    Builtin,
    WorkflowBundle,
    RuntimeProcessBundle,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSettingsOptionManifest {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSettingsSchemaManifest {
    #[serde(default)]
    pub fields: Vec<PluginSettingsFieldManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendPluginManifest {
    pub id: String,
    #[serde(default = "default_backend_plugin_kind")]
    pub kind: BackendPluginKind,
    pub name: String,
    pub version: String,
    pub description: String,
    pub provided_tools: Vec<String>,
    pub provided_skills: Vec<String>,
    #[serde(default)]
    pub settings_tab: Option<PluginSettingsTabManifest>,
    #[serde(default)]
    pub settings_schema: Option<PluginSettingsSchemaManifest>,
}

fn default_backend_plugin_kind() -> BackendPluginKind {
    BackendPluginKind::Builtin
}

#[async_trait]
pub trait BackendPlugin: Send + Sync {
    fn manifest(&self) -> BackendPluginManifest;

    async fn register_tools(
        &self,
        _tool_registry: Arc<ToolRegistry>,
        _config: &AgentConfig,
    ) -> Result<()> {
        Ok(())
    }

    fn build_skills(&self, _config: &AgentConfig) -> Result<Vec<Box<dyn Skill>>> {
        Ok(Vec::new())
    }
}
