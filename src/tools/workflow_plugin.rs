use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::comfy_client::ComfyUIClient;
use crate::config::AgentConfig;
use crate::workflow_plugin::WorkflowPluginCatalog;

use super::{Tool, ToolCategory, ToolContext, ToolOutput};

const DEFAULT_COMFY_TIMEOUT_SECS: u64 = 300;
const MAX_COMFY_TIMEOUT_SECS: u64 = 900;

pub struct RunWorkflowPluginTool {
    catalog: Arc<WorkflowPluginCatalog>,
    available_plugin_ids: Vec<String>,
}

impl RunWorkflowPluginTool {
    pub fn new(catalog: Arc<WorkflowPluginCatalog>) -> Self {
        let available_plugin_ids = catalog.plugin_ids();
        Self {
            catalog,
            available_plugin_ids,
        }
    }
}

#[async_trait]
impl Tool for RunWorkflowPluginTool {
    fn name(&self) -> &str {
        "run_workflow_plugin"
    }

    fn description(&self) -> &str {
        "Run an installed workflow plugin bundle through the configured ComfyUI server. Use this for optional media or audio capabilities that are packaged as plugin workflows."
    }

    fn parameters_schema(&self) -> Value {
        let mut plugin_id = json!({
            "type": "string",
            "description": "Installed workflow plugin id to execute",
        });
        if !self.available_plugin_ids.is_empty() {
            plugin_id["enum"] = json!(self.available_plugin_ids.clone());
        }

        json!({
            "type": "object",
            "properties": {
                "plugin_id": plugin_id,
                "inputs": {
                    "type": "object",
                    "description": "Runtime inputs keyed by the plugin's declared binding source names"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "ComfyUI wait timeout in seconds (default 300, max 900)"
                },
                "max_assets": {
                    "type": "integer",
                    "description": "Maximum number of generated assets to return (default 4)"
                }
            },
            "required": ["plugin_id"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let plugin_id = match params.get("plugin_id").and_then(Value::as_str) {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing 'plugin_id' parameter".to_string(),
                ))
            }
        };

        let Some(bundle) = self.catalog.get(&plugin_id) else {
            return Ok(ToolOutput::Error(format!(
                "Workflow plugin '{}' is not installed",
                plugin_id
            )));
        };

        let runtime_inputs = params
            .get("inputs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let timeout_secs = params
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_COMFY_TIMEOUT_SECS)
            .min(MAX_COMFY_TIMEOUT_SECS);
        let max_assets = params
            .get("max_assets")
            .and_then(Value::as_u64)
            .unwrap_or(4)
            .clamp(1, 24) as usize;

        let config = AgentConfig::load();
        let executable_workflow = match bundle.prepare_workflow(&config, &runtime_inputs) {
            Ok(workflow) => workflow,
            Err(error) => {
                return Ok(ToolOutput::Error(format!(
                    "Failed to prepare workflow plugin '{}': {}",
                    plugin_id, error
                )))
            }
        };

        let comfy = ComfyUIClient::new(config.comfyui.api_url.clone());
        let prompt_id = match comfy.queue_prompt(executable_workflow).await {
            Ok(id) => id,
            Err(error) => {
                return Ok(ToolOutput::Error(format!(
                    "Failed to queue workflow plugin '{}': {}",
                    plugin_id, error
                )))
            }
        };

        let assets = match comfy
            .wait_for_completion_assets(&prompt_id, timeout_secs)
            .await
        {
            Ok(assets) => assets,
            Err(error) => {
                return Ok(ToolOutput::Error(format!(
                    "Workflow plugin '{}' failed: {}",
                    plugin_id, error
                )))
            }
        };

        let mut media = Vec::new();
        for asset in assets.into_iter().take(max_assets) {
            let local_path = match comfy.download_asset(&asset).await {
                Ok(path) => path,
                Err(error) => {
                    return Ok(ToolOutput::Error(format!(
                        "Failed to download workflow plugin asset '{}': {}",
                        asset.filename, error
                    )))
                }
            };
            media.push(json!({
                "filename": asset.filename,
                "path": local_path,
                "media_kind": asset.media_kind,
                "mime_type": mime_for_kind(&asset.media_kind),
                "node_id": asset.node_id,
                "source": "run_workflow_plugin",
            }));
        }

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "plugin_id": plugin_id,
            "prompt_id": prompt_id,
            "media": media,
            "count": media.len(),
        })))
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Network
    }
}

fn mime_for_kind(media_kind: &str) -> &'static str {
    match media_kind {
        "audio" => "audio/wav",
        "video" => "video/mp4",
        "image" => "image/png",
        _ => "application/octet-stream",
    }
}
