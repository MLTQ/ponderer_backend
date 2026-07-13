//! ComfyUI generation tool.
//!
//! `generate_comfy_media` runs the configured ComfyUI workflow and returns
//! structured media metadata so chat UI can render generated assets inline.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

use crate::comfy_client::{ComfyUIClient, GeneratedAssetInfo};
use crate::comfy_workflow::ComfyWorkflow;
use crate::config::AgentConfig;

use super::{Tool, ToolCategory, ToolContext, ToolOutput};

const DEFAULT_COMFY_TIMEOUT_SECS: u64 = 300;
const MAX_COMFY_TIMEOUT_SECS: u64 = 900;

pub struct GenerateComfyMediaTool;

impl GenerateComfyMediaTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GenerateComfyMediaTool {
    fn name(&self) -> &str {
        "generate_comfy_media"
    }

    fn description(&self) -> &str {
        "Generate media via the configured ComfyUI workflow. Returns JSON metadata and local file paths for generated assets (images/audio/video/files)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Prompt text to apply to controllable text inputs in the workflow"
                },
                "inputs": {
                    "type": "object",
                    "description": "Optional workflow input overrides keyed by '<node_id>_<input_name>'"
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
            "required": ["prompt"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let prompt = params
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if prompt.is_empty() {
            return Ok(ToolOutput::Error(
                "Missing or empty 'prompt' parameter".to_string(),
            ));
        }

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
        let overrides = parse_input_overrides(params.get("inputs"));

        let config = AgentConfig::load();
        if !config.enable_image_generation {
            return Ok(ToolOutput::Error(
                "Image generation is disabled in config".to_string(),
            ));
        }

        let workflow_json = match config.workflow_settings.as_deref() {
            Some(raw) => raw,
            None => {
                return Ok(ToolOutput::Error(
                    "No ComfyUI workflow configured (workflow_settings missing)".to_string(),
                ))
            }
        };

        let workflow: ComfyWorkflow = match serde_json::from_str(workflow_json) {
            Ok(workflow) => workflow,
            Err(e) => {
                return Ok(ToolOutput::Error(format!(
                    "Failed to parse configured workflow: {}",
                    e
                )))
            }
        };

        let mut workflow_inputs = overrides;
        apply_prompt_and_seed_defaults(&workflow, &prompt, &mut workflow_inputs);

        let executable_workflow = match workflow.prepare_for_execution(&workflow_inputs) {
            Ok(w) => w,
            Err(e) => {
                return Ok(ToolOutput::Error(format!(
                    "Failed to prepare workflow for execution: {}",
                    e
                )))
            }
        };

        let comfy = ComfyUIClient::new(config.comfyui.api_url.clone());
        let prompt_id = match comfy.queue_prompt(executable_workflow).await {
            Ok(id) => id,
            Err(e) => {
                return Ok(ToolOutput::Error(format!(
                    "Failed to queue ComfyUI prompt: {}",
                    e
                )))
            }
        };

        let assets = match comfy
            .wait_for_completion_assets(&prompt_id, timeout_secs)
            .await
        {
            Ok(assets) => assets,
            Err(e) => {
                return Ok(ToolOutput::Error(format!(
                    "ComfyUI generation failed: {}",
                    e
                )))
            }
        };

        let mut media = Vec::new();
        for asset in assets.into_iter().take(max_assets) {
            let local_path = match comfy.download_asset(&asset).await {
                Ok(path) => path,
                Err(e) => {
                    return Ok(ToolOutput::Error(format!(
                        "Failed to download generated asset '{}': {}",
                        asset.filename, e
                    )))
                }
            };
            media.push(asset_to_json(&asset, &local_path));
        }

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "prompt": prompt,
            "prompt_id": prompt_id,
            "workflow": workflow.name,
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

fn parse_input_overrides(raw: Option<&Value>) -> HashMap<String, Value> {
    raw.and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default()
}

fn apply_prompt_and_seed_defaults(
    workflow: &ComfyWorkflow,
    prompt: &str,
    inputs: &mut HashMap<String, Value>,
) {
    for (node_id, node) in &workflow.controllable_nodes {
        for input in &node.inputs {
            if !input.agent_modifiable {
                continue;
            }
            let key = format!("{}_{}", node_id, input.name);
            if inputs.contains_key(&key) {
                continue;
            }
            match input.name.as_str() {
                "text" => {
                    inputs.insert(key, Value::String(prompt.to_string()));
                }
                "seed" => {
                    inputs.insert(key, json!(-1));
                }
                _ => {}
            }
        }
    }
}

fn asset_to_json(asset: &GeneratedAssetInfo, local_path: &Path) -> Value {
    json!({
        "path": absolute_or_original(local_path.to_string_lossy().as_ref()),
        "filename": asset.filename,
        "subfolder": asset.subfolder,
        "file_type": asset.file_type,
        "media_kind": asset.media_kind,
        "node_id": asset.node_id,
        "mime_type": mime_type_from_path(&asset.filename),
        "source": "generate_comfy_media"
    })
}

fn absolute_or_original(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn mime_type_from_path(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_ascii_lowercase());

    match ext.as_deref() {
        Some("png") => "image/png".to_string(),
        Some("jpg" | "jpeg") => "image/jpeg".to_string(),
        Some("webp") => "image/webp".to_string(),
        Some("gif") => "image/gif".to_string(),
        Some("bmp") => "image/bmp".to_string(),
        Some("wav") => "audio/wav".to_string(),
        Some("mp3") => "audio/mpeg".to_string(),
        Some("ogg") => "audio/ogg".to_string(),
        Some("flac") => "audio/flac".to_string(),
        Some("m4a") => "audio/mp4".to_string(),
        Some("mp4") => "video/mp4".to_string(),
        Some("mov") => "video/quicktime".to_string(),
        Some("webm") => "video/webm".to_string(),
        Some("mkv") => "video/x-matroska".to_string(),
        Some("avi") => "video/x-msvideo".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_mime_type_from_extension() {
        assert_eq!(mime_type_from_path("image.png"), "image/png");
        assert_eq!(mime_type_from_path("clip.mp4"), "video/mp4");
        assert_eq!(mime_type_from_path("voice.mp3"), "audio/mpeg");
        assert_eq!(
            mime_type_from_path("archive.bin"),
            "application/octet-stream"
        );
    }

    #[test]
    fn parses_overrides_object() {
        let overrides = parse_input_overrides(Some(&json!({
            "12_text": "hello",
            "9_seed": 42
        })));
        assert_eq!(overrides.get("12_text"), Some(&json!("hello")));
        assert_eq!(overrides.get("9_seed"), Some(&json!(42)));
    }
}
