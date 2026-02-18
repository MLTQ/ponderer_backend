use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug, Clone, Serialize)]
pub struct QueuePromptRequest {
    pub prompt: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct QueuePromptResponse {
    pub prompt_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HistoryEntry {
    pub outputs: HashMap<String, OutputNode>,
    pub status: Option<StatusInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OutputNode {
    pub images: Option<Vec<ComfyOutputFile>>,
    pub audio: Option<Vec<ComfyOutputFile>>,
    pub videos: Option<Vec<ComfyOutputFile>>,
    pub gifs: Option<Vec<ComfyOutputFile>>,
    pub files: Option<Vec<ComfyOutputFile>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComfyOutputFile {
    pub filename: String,
    #[serde(default)]
    pub subfolder: String,
    #[serde(rename = "type")]
    pub file_type: String,
}

/// Backward-compatible alias for historical image-only flow.
pub type ImageInfo = ComfyOutputFile;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedAssetInfo {
    pub filename: String,
    pub subfolder: String,
    pub file_type: String,
    pub media_kind: String,
    pub node_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatusInfo {
    pub status_str: Option<String>,
    pub completed: Option<bool>,
}

/// Client for ComfyUI API
pub struct ComfyUIClient {
    api_url: String,
    client: Client,
}

impl ComfyUIClient {
    pub fn new(api_url: String) -> Self {
        Self {
            api_url,
            client: Client::new(),
        }
    }

    /// Queue a workflow for execution
    pub async fn queue_prompt(&self, workflow: serde_json::Value) -> Result<String> {
        let url = format!("{}/prompt", self.api_url);

        let request = QueuePromptRequest {
            prompt: workflow,
            client_id: None,
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to send prompt to ComfyUI")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("ComfyUI API error {}: {}", status, body);
        }

        let result: QueuePromptResponse = response
            .json()
            .await
            .context("Failed to parse ComfyUI response")?;

        Ok(result.prompt_id)
    }

    /// Check the status of a queued prompt
    pub async fn get_history(&self, prompt_id: &str) -> Result<Option<HistoryEntry>> {
        let url = format!("{}/history/{}", self.api_url, prompt_id);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to get history from ComfyUI")?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to get history: {}", response.status());
        }

        let history: HashMap<String, HistoryEntry> = response
            .json()
            .await
            .context("Failed to parse history response")?;

        Ok(history.get(prompt_id).cloned())
    }

    /// Wait for a prompt to complete execution
    pub async fn wait_for_completion(
        &self,
        prompt_id: &str,
        timeout_secs: u64,
    ) -> Result<ImageInfo> {
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                anyhow::bail!("Timeout waiting for ComfyUI to complete");
            }

            if let Some(history) = self.get_history(prompt_id).await? {
                // Check if completed
                if let Some(status) = &history.status {
                    if status.completed == Some(true) {
                        let assets = extract_assets_from_history(&history);
                        if let Some(image) = assets
                            .iter()
                            .find(|asset| asset.media_kind == "image")
                            .map(|asset| ComfyOutputFile {
                                filename: asset.filename.clone(),
                                subfolder: asset.subfolder.clone(),
                                file_type: asset.file_type.clone(),
                            })
                        {
                            tracing::info!("Image generated: {}", image.filename);
                            return Ok(image);
                        }
                        anyhow::bail!("Workflow completed but no image found");
                    }
                }
            }

            // Poll every second
            sleep(Duration::from_secs(1)).await;
        }
    }

    /// Wait for a prompt to complete execution and return all output assets.
    pub async fn wait_for_completion_assets(
        &self,
        prompt_id: &str,
        timeout_secs: u64,
    ) -> Result<Vec<GeneratedAssetInfo>> {
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                anyhow::bail!("Timeout waiting for ComfyUI to complete");
            }

            if let Some(history) = self.get_history(prompt_id).await? {
                if let Some(status) = &history.status {
                    if status.completed == Some(true) {
                        let assets = extract_assets_from_history(&history);
                        if assets.is_empty() {
                            anyhow::bail!("Workflow completed but no downloadable assets found");
                        }
                        return Ok(assets);
                    }
                }
            }

            sleep(Duration::from_secs(1)).await;
        }
    }

    /// Download generated image
    pub async fn download_image(&self, image_info: &ImageInfo) -> Result<PathBuf> {
        let asset = GeneratedAssetInfo {
            filename: image_info.filename.clone(),
            subfolder: image_info.subfolder.clone(),
            file_type: image_info.file_type.clone(),
            media_kind: "image".to_string(),
            node_id: "unknown".to_string(),
        };
        self.download_asset(&asset).await
    }

    /// Download a generated asset to the working directory.
    pub async fn download_asset(&self, asset: &GeneratedAssetInfo) -> Result<PathBuf> {
        let url = format!(
            "{}/view?filename={}&subfolder={}&type={}",
            self.api_url, asset.filename, asset.subfolder, asset.file_type
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to download asset from ComfyUI")?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to download asset: {}", response.status());
        }

        let bytes = response
            .bytes()
            .await
            .context("Failed to read asset bytes")?;

        // Save to a unique local file in cwd.
        let output_path = unique_output_path(&asset.filename);
        std::fs::write(&output_path, bytes)
            .with_context(|| format!("Failed to write image to {:?}", output_path))?;

        Ok(output_path)
    }

    /// Test connection to ComfyUI
    pub async fn test_connection(&self) -> Result<()> {
        let url = format!("{}/history", self.api_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to ComfyUI")?;

        if !response.status().is_success() {
            anyhow::bail!("ComfyUI returned error: {}", response.status());
        }

        Ok(())
    }
}

fn extract_assets_from_history(history: &HistoryEntry) -> Vec<GeneratedAssetInfo> {
    let mut assets = Vec::new();

    for (node_id, output) in &history.outputs {
        collect_assets_for_kind(&mut assets, node_id, "image", output.images.as_deref());
        collect_assets_for_kind(&mut assets, node_id, "audio", output.audio.as_deref());
        collect_assets_for_kind(&mut assets, node_id, "video", output.videos.as_deref());
        collect_assets_for_kind(&mut assets, node_id, "image", output.gifs.as_deref());
        collect_assets_for_kind(&mut assets, node_id, "file", output.files.as_deref());
    }

    assets
}

fn collect_assets_for_kind(
    out: &mut Vec<GeneratedAssetInfo>,
    node_id: &str,
    default_kind: &str,
    files: Option<&[ComfyOutputFile]>,
) {
    let Some(files) = files else {
        return;
    };

    for file in files {
        out.push(GeneratedAssetInfo {
            filename: file.filename.clone(),
            subfolder: file.subfolder.clone(),
            file_type: file.file_type.clone(),
            media_kind: infer_media_kind(default_kind, &file.filename),
            node_id: node_id.to_string(),
        });
    }
}

fn infer_media_kind(default_kind: &str, filename: &str) -> String {
    let ext = PathBuf::from(filename)
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_ascii_lowercase());

    match ext.as_deref() {
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp") => "image".to_string(),
        Some("wav" | "mp3" | "ogg" | "flac" | "m4a") => "audio".to_string(),
        Some("mp4" | "mov" | "webm" | "mkv" | "avi") => "video".to_string(),
        _ => default_kind.to_string(),
    }
}

fn unique_output_path(filename: &str) -> PathBuf {
    let candidate = PathBuf::from(format!("generated_{}", filename));
    if !candidate.exists() {
        return candidate;
    }

    let stem = PathBuf::from(filename)
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("output")
        .to_string();
    let ext = PathBuf::from(filename)
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| format!(".{}", s))
        .unwrap_or_default();

    for idx in 1..=9999 {
        let candidate = PathBuf::from(format!("generated_{}_{}{}", stem, idx, ext));
        if !candidate.exists() {
            return candidate;
        }
    }

    PathBuf::from(format!(
        "generated_{}_{}{}",
        stem,
        chrono::Utc::now().timestamp_millis(),
        ext
    ))
}
