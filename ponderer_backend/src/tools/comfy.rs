//! ComfyUI generation and Graphchan publishing tools.
//!
//! `generate_comfy_media` runs the configured ComfyUI workflow and returns
//! structured media metadata so chat UI can render generated assets inline.
//! `post_to_graphchan` publishes content (optionally with media references or
//! embedded data URIs) to a Graphchan thread.

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::comfy_client::{ComfyUIClient, GeneratedAssetInfo};
use crate::comfy_workflow::ComfyWorkflow;
use crate::config::AgentConfig;

use super::{Tool, ToolCategory, ToolContext, ToolOutput};

const DEFAULT_COMFY_TIMEOUT_SECS: u64 = 300;
const MAX_COMFY_TIMEOUT_SECS: u64 = 900;
const MAX_GRAPHCHAN_DATA_URI_BYTES: usize = 256 * 1024;

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

pub struct PostToGraphchanTool {
    client: Client,
}

impl PostToGraphchanTool {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }
}

#[async_trait]
impl Tool for PostToGraphchanTool {
    fn name(&self) -> &str {
        "post_to_graphchan"
    }

    fn description(&self) -> &str {
        "Publish a message to Graphchan. Optional media paths can be added as references or embedded as data URIs."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "Target Graphchan thread ID"
                },
                "body": {
                    "type": "string",
                    "description": "Post body text"
                },
                "reply_to_post_id": {
                    "type": "string",
                    "description": "Optional parent post ID for replies"
                },
                "media_paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional local media file paths to include in the post"
                },
                "embed_data_uri": {
                    "type": "boolean",
                    "description": "When true, small media files are embedded directly as data URIs (default false)"
                }
            },
            "required": ["thread_id", "body"]
        })
    }

    async fn execute(&self, params: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let thread_id = match params.get("thread_id").and_then(Value::as_str) {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing 'thread_id' parameter".to_string(),
                ))
            }
        };
        let base_body = match params.get("body").and_then(Value::as_str) {
            Some(body) if !body.trim().is_empty() => body.trim().to_string(),
            _ => return Ok(ToolOutput::Error("Missing 'body' parameter".to_string())),
        };
        let reply_to_post_id = params
            .get("reply_to_post_id")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let embed_data_uri = params
            .get("embed_data_uri")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let media_paths = params
            .get("media_paths")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let config = AgentConfig::load();
        if config.graphchan_api_url.trim().is_empty() {
            return Ok(ToolOutput::Error(
                "graphchan_api_url is not configured".to_string(),
            ));
        }

        let media_lines = match build_graphchan_media_lines(&media_paths, embed_data_uri) {
            Ok(lines) => lines,
            Err(e) => return Ok(ToolOutput::Error(format!("Failed to prepare media: {}", e))),
        };

        let mut body = base_body;
        if !media_lines.is_empty() {
            body.push_str("\n\nAttached media:\n");
            for line in media_lines {
                body.push_str("- ");
                body.push_str(&line);
                body.push('\n');
            }
        }

        let mut parent_post_ids = Vec::new();
        if let Some(parent) = &reply_to_post_id {
            parent_post_ids.push(parent.clone());
        }

        let payload = json!({
            "thread_id": thread_id,
            "author_peer_id": serde_json::Value::Null,
            "body": body,
            "parent_post_ids": parent_post_ids,
            "metadata": {
                "agent": {
                    "name": ctx.username,
                    "version": serde_json::Value::Null
                },
                "client": "ponderer"
            }
        });

        let url = format!("{}/threads/{}/posts", config.graphchan_api_url, thread_id);
        let response = self.client.post(&url).json(&payload).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Ok(ToolOutput::Error(format!(
                "Graphchan post failed ({}): {}",
                status, text
            )));
        }

        let response_json: Value = response
            .json()
            .await
            .context("Failed to parse Graphchan response JSON")?;
        let post_id = response_json
            .get("post")
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let media_payload: Vec<Value> = media_paths
            .iter()
            .map(|path| {
                let abs = absolute_or_original(path);
                json!({
                    "path": abs,
                    "media_kind": infer_media_kind_from_path(path),
                    "mime_type": mime_type_from_path(path),
                    "source": "post_to_graphchan"
                })
            })
            .collect();

        Ok(ToolOutput::Json(json!({
            "status": "posted",
            "thread_id": thread_id,
            "post_id": post_id,
            "media_count": media_paths.len(),
            "media": media_payload
        })))
    }

    fn requires_approval(&self) -> bool {
        true
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

fn build_graphchan_media_lines(paths: &[String], embed_data_uri: bool) -> Result<Vec<String>> {
    let mut lines = Vec::new();

    for raw_path in paths {
        let path = PathBuf::from(raw_path);
        let display_name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or(raw_path)
            .to_string();
        let abs = absolute_or_original(raw_path);

        if embed_data_uri {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("Failed to read media file '{}'", raw_path))?;
            if bytes.len() <= MAX_GRAPHCHAN_DATA_URI_BYTES {
                let mime = mime_type_from_path(raw_path);
                let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                let data_uri = format!("data:{};base64,{}", mime, encoded);
                if mime.starts_with("image/") {
                    lines.push(format!("![{}]({})", display_name, data_uri));
                } else {
                    lines.push(format!("[{}]({})", display_name, data_uri));
                }
                continue;
            }
        }

        lines.push(format!("{} (local file: {})", display_name, abs));
    }

    Ok(lines)
}

fn absolute_or_original(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn infer_media_kind_from_path(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp") => "image".to_string(),
        Some("wav" | "mp3" | "ogg" | "flac" | "m4a") => "audio".to_string(),
        Some("mp4" | "mov" | "webm" | "mkv" | "avi") => "video".to_string(),
        _ => "file".to_string(),
    }
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
    fn infers_media_kind_from_extension() {
        assert_eq!(infer_media_kind_from_path("image.png"), "image");
        assert_eq!(infer_media_kind_from_path("clip.mp4"), "video");
        assert_eq!(infer_media_kind_from_path("voice.mp3"), "audio");
        assert_eq!(infer_media_kind_from_path("archive.bin"), "file");
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
