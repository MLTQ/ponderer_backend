use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    default_plugin_protocol_version, default_supported_plugin_protocol_versions,
    PluginEffectDeclaration, PluginHostDescriptor, PluginStateMutation, PluginStateValue,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePluginRpcRequest {
    #[serde(default = "default_plugin_protocol_version")]
    pub protocol_version: u32,
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePluginRpcError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePluginRpcResponse {
    #[serde(default = "default_plugin_protocol_version")]
    pub protocol_version: u32,
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<RuntimePluginRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePluginHandshakeRequest {
    #[serde(default = "default_supported_plugin_protocol_versions")]
    pub supported_protocol_versions: Vec<u32>,
    pub host: PluginHostDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePluginToolCategory {
    FileSystem,
    Shell,
    Network,
    Memory,
    General,
}

impl Default for RuntimePluginToolCategory {
    fn default() -> Self {
        Self::General
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePluginToolManifest {
    pub name: String,
    pub description: String,
    #[serde(default = "default_tool_parameters_schema")]
    pub parameters: Value,
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub category: RuntimePluginToolCategory,
    #[serde(default)]
    pub effects: Vec<PluginEffectDeclaration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RuntimePluginCapabilities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub event_hooks: Vec<String>,
    #[serde(default)]
    pub prompt_slots: Vec<String>,
    #[serde(default)]
    pub skill_polling: bool,
    #[serde(default, alias = "permissions")]
    pub requested_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePluginHandshake {
    #[serde(default = "default_plugin_protocol_version")]
    pub protocol_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: RuntimePluginCapabilities,
    #[serde(default)]
    pub tools: Vec<RuntimePluginToolManifest>,
}

impl RuntimePluginHandshake {
    pub fn tool_names(&self) -> Vec<String> {
        if !self.tools.is_empty() {
            return self.tools.iter().map(|tool| tool.name.clone()).collect();
        }
        self.capabilities.tools.clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RuntimePluginConfigureRequest {
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub state: HashMap<String, PluginStateValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePluginConfigureResponse {
    #[serde(default = "default_true")]
    pub configured: bool,
    #[serde(default)]
    pub state_updates: Vec<PluginStateMutation>,
}

impl Default for RuntimePluginConfigureResponse {
    fn default() -> Self {
        Self {
            configured: true,
            state_updates: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePluginToolInvocation {
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub context: RuntimePluginToolInvocationContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RuntimePluginToolInvocationContext {
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub loop_name: Option<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub autonomous: bool,
    #[serde(default)]
    pub working_directory: String,
    #[serde(default)]
    pub invoked_at: String,
    #[serde(default)]
    pub deadline_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePluginToolResultKind {
    Text,
    Json,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePluginToolResult {
    pub kind: RuntimePluginToolResultKind,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub state_updates: Vec<PluginStateMutation>,
}

fn default_tool_parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {}
    })
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_rpc_response_and_handshake_default_to_v1() {
        let response: RuntimePluginRpcResponse = serde_json::from_value(serde_json::json!({
            "id": "1",
            "ok": true,
            "result": null
        }))
        .expect("legacy response should decode");
        let handshake: RuntimePluginHandshake = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "name": "Legacy",
            "version": "0.1.0"
        }))
        .expect("legacy handshake should decode");

        assert_eq!(response.protocol_version, 1);
        assert_eq!(handshake.protocol_version, 1);
    }

    #[test]
    fn tool_manifest_defaults_are_legacy_compatible() {
        let tool: RuntimePluginToolManifest = serde_json::from_value(serde_json::json!({
            "name": "legacy_tool",
            "description": "fixture"
        }))
        .expect("legacy tool should decode");

        assert_eq!(tool.category, RuntimePluginToolCategory::General);
        assert!(!tool.requires_approval);
        assert!(tool.effects.is_empty());
        assert_eq!(tool.parameters["type"], "object");
    }
}
