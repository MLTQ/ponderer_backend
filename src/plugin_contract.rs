use serde::{Deserialize, Serialize};

pub use crate::plugin::{
    BackendPluginKind, BackendPluginManifest, PluginSettingsFieldKind,
    PluginSettingsFieldManifest, PluginSettingsOptionManifest, PluginSettingsSchemaManifest,
    PluginSettingsTabManifest,
};

pub type PluginKind = BackendPluginKind;
pub type PluginManifest = BackendPluginManifest;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginRuntimeState {
    Unknown,
    Stopped,
    Starting,
    Running,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRuntimeStatus {
    pub plugin_id: String,
    pub state: PluginRuntimeState,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub message: Option<String>,
}
