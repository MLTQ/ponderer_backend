//! Vision-oriented tools for local media inspection and publishing.
//!
//! - `evaluate_local_image`: evaluates a local image file with a vision-capable model.
//! - `publish_media_to_chat`: emits media metadata for private chat rendering.
//! - `capture_screen`: optional screen capture tool gated by explicit config opt-in.
//! - `capture_camera_snapshot`: optional camera snapshot tool gated by explicit config opt-in.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::config::AgentConfig;
use crate::llm_client::LlmClient;

use super::{Tool, ToolCategory, ToolContext, ToolOutput};

const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;

pub struct EvaluateLocalImageTool;

impl EvaluateLocalImageTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for EvaluateLocalImageTool {
    fn name(&self) -> &str {
        "evaluate_local_image"
    }

    fn description(&self) -> &str {
        "Evaluate a local image with the configured vision-capable model and return structured JSON feedback."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to an image file (absolute or relative)"
                },
                "prompt": {
                    "type": "string",
                    "description": "What to evaluate in the image (optional)"
                },
                "context": {
                    "type": "string",
                    "description": "Additional task context for evaluation (optional)"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for the vision request"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let path = match params.get("path").and_then(Value::as_str) {
            Some(path) if !path.trim().is_empty() => path.trim(),
            _ => return Ok(ToolOutput::Error("Missing 'path' parameter".to_string())),
        };
        let prompt = params
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("Assess this image and summarize what you see.");
        let context = params.get("context").and_then(Value::as_str).unwrap_or("");
        let model_override = params
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string);

        let abs_path = canonicalize_or_original(path);
        if !is_supported_image_path(&abs_path) {
            return Ok(ToolOutput::Error(format!(
                "Unsupported image type for '{}'",
                abs_path
            )));
        }

        let image_bytes = match std::fs::read(&abs_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(ToolOutput::Error(format!(
                    "Failed to read image '{}': {}",
                    abs_path, e
                )))
            }
        };
        if image_bytes.is_empty() {
            return Ok(ToolOutput::Error(format!("Image '{}' is empty", abs_path)));
        }
        if image_bytes.len() > MAX_IMAGE_BYTES {
            return Ok(ToolOutput::Error(format!(
                "Image '{}' too large ({} bytes, max {})",
                abs_path,
                image_bytes.len(),
                MAX_IMAGE_BYTES
            )));
        }

        let config = AgentConfig::load();
        let chosen_model = model_override.unwrap_or_else(|| config.llm_model.clone());
        let llm_client = LlmClient::new(
            normalize_api_url_for_chat(&config.llm_api_url),
            config.llm_api_key.unwrap_or_default(),
            chosen_model.clone(),
        );

        let evaluation = match llm_client
            .evaluate_image(&image_bytes, prompt, context)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                return Ok(ToolOutput::Error(format!(
                    "Vision evaluation failed for '{}': {}",
                    abs_path, e
                )))
            }
        };

        let mime_type = mime_type_from_path(&abs_path);
        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "path": abs_path,
            "model": chosen_model,
            "evaluation": evaluation,
            "media": [
                {
                    "path": abs_path,
                    "media_kind": "image",
                    "mime_type": mime_type,
                    "source": "evaluate_local_image"
                }
            ]
        })))
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Network
    }
}

pub struct PublishMediaToChatTool;

impl PublishMediaToChatTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for PublishMediaToChatTool {
    fn name(&self) -> &str {
        "publish_media_to_chat"
    }

    fn description(&self) -> &str {
        "Publish one or more local media files into private chat by returning a media payload."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Local file paths to publish in chat"
                },
                "note": {
                    "type": "string",
                    "description": "Optional note to include with the media"
                }
            },
            "required": ["paths"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let paths = params
            .get("paths")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if paths.is_empty() {
            return Ok(ToolOutput::Error(
                "Missing or empty 'paths' parameter".to_string(),
            ));
        }

        let mut media = Vec::new();
        for raw_path in &paths {
            let path = canonicalize_or_original(raw_path);
            if !Path::new(&path).exists() {
                return Ok(ToolOutput::Error(format!(
                    "File does not exist: {}",
                    raw_path
                )));
            }
            media.push(json!({
                "path": path,
                "media_kind": media_kind_from_path(raw_path),
                "mime_type": mime_type_from_path(raw_path),
                "source": "publish_media_to_chat"
            }));
        }

        let note = params
            .get("note")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Shared media")
            .to_string();

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "note": note,
            "media_count": media.len(),
            "media": media
        })))
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }
}

pub struct CaptureScreenTool;

impl CaptureScreenTool {
    pub fn new() -> Self {
        Self
    }
}

pub struct CaptureCameraSnapshotTool;

impl CaptureCameraSnapshotTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CaptureScreenTool {
    fn name(&self) -> &str {
        "capture_screen"
    }

    fn description(&self) -> &str {
        "Capture a screenshot of the current desktop and publish it as media metadata. Disabled unless explicitly enabled in settings."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "output_path": {
                    "type": "string",
                    "description": "Optional output file path (.png). Defaults to screenshot_<timestamp>.png in working directory."
                }
            }
        })
    }

    async fn execute(&self, params: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let config = AgentConfig::load();
        if !config.enable_screen_capture_in_loop {
            return Ok(ToolOutput::Error(
                "Screen capture is disabled. Enable 'Allow screen capture in agentic loop' in Settings first."
                    .to_string(),
            ));
        }

        let output_path = params
            .get("output_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(&ctx.working_directory).join(format!(
                    "screenshot_{}.png",
                    Utc::now().format("%Y%m%d_%H%M%S")
                ))
            });

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create screenshot parent directory '{}'",
                    parent.display()
                )
            })?;
        }

        capture_screen_to_path(&output_path).await?;

        let abs_path = canonicalize_or_original(output_path.to_string_lossy().as_ref());
        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "path": abs_path,
            "media": [
                {
                    "path": abs_path,
                    "media_kind": "image",
                    "mime_type": "image/png",
                    "source": "capture_screen"
                }
            ]
        })))
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }
}

#[async_trait]
impl Tool for CaptureCameraSnapshotTool {
    fn name(&self) -> &str {
        "capture_camera_snapshot"
    }

    fn description(&self) -> &str {
        "Capture a camera snapshot on demand and publish it as media metadata. Disabled unless explicitly enabled in settings."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "output_path": {
                    "type": "string",
                    "description": "Optional output path for the snapshot file (.jpg/.png). Defaults to camera_<timestamp>.jpg in working directory."
                },
                "device_index": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional camera index (default: 0)."
                },
                "device_name": {
                    "type": "string",
                    "description": "Optional camera device name. Primarily used on Windows with ffmpeg/dshow."
                }
            }
        })
    }

    async fn execute(&self, params: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let config = AgentConfig::load();
        if !config.enable_camera_capture_tool {
            return Ok(ToolOutput::Error(
                "Camera snapshots are disabled. Enable 'Allow camera snapshots in agentic loop (opt-in)' in Settings first."
                    .to_string(),
            ));
        }

        let output_path = params
            .get("output_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(&ctx.working_directory)
                    .join(format!("camera_{}.jpg", Utc::now().format("%Y%m%d_%H%M%S")))
            });

        let device_index = params
            .get("device_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let device_name = params
            .get("device_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create camera snapshot parent directory '{}'",
                    parent.display()
                )
            })?;
        }

        capture_camera_to_path(&output_path, device_index, device_name.as_deref()).await?;

        let abs_path = canonicalize_or_original(output_path.to_string_lossy().as_ref());
        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "path": abs_path,
            "device_index": device_index,
            "media": [
                {
                    "path": abs_path,
                    "media_kind": "image",
                    "mime_type": mime_type_from_path(&abs_path),
                    "source": "capture_camera_snapshot"
                }
            ]
        })))
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }
}

pub async fn capture_screen_to_path(output_path: &Path) -> Result<()> {
    let path_str = output_path.to_string_lossy().to_string();

    if cfg!(target_os = "macos") {
        run_capture_command("screencapture", &["-x", &path_str]).await?;
        return Ok(());
    }

    if cfg!(target_os = "linux") {
        let attempts: [(&str, Vec<&str>); 3] = [
            ("gnome-screenshot", vec!["-f", &path_str]),
            ("scrot", vec![&path_str]),
            ("grim", vec![&path_str]),
        ];
        for (cmd, args) in attempts {
            if run_capture_command(cmd, &args).await.is_ok() {
                return Ok(());
            }
        }
        anyhow::bail!(
            "No supported Linux screenshot command succeeded (tried gnome-screenshot, scrot, grim)"
        );
    }

    if cfg!(target_os = "windows") {
        let ps_script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             Add-Type -AssemblyName System.Drawing; \
             $bounds=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; \
             $bmp=New-Object System.Drawing.Bitmap($bounds.Width,$bounds.Height); \
             $g=[System.Drawing.Graphics]::FromImage($bmp); \
             $g.CopyFromScreen($bounds.Location,[System.Drawing.Point]::Empty,$bounds.Size); \
             $bmp.Save('{}',[System.Drawing.Imaging.ImageFormat]::Png); \
             $g.Dispose(); $bmp.Dispose();",
            path_str.replace('\'', "''")
        );
        run_capture_command("powershell", &["-NoProfile", "-Command", &ps_script]).await?;
        return Ok(());
    }

    anyhow::bail!("Screen capture is not supported on this OS")
}

async fn capture_camera_to_path(
    output_path: &Path,
    device_index: u32,
    device_name: Option<&str>,
) -> Result<()> {
    let path_str = output_path.to_string_lossy().to_string();

    if cfg!(target_os = "macos") {
        let imagesnap_args = if let Some(name) = device_name {
            vec![
                "-q".to_string(),
                "-d".to_string(),
                name.to_string(),
                path_str.clone(),
            ]
        } else {
            vec!["-q".to_string(), path_str.clone()]
        };
        if try_capture_command("imagesnap", &imagesnap_args).await {
            return Ok(());
        }

        let ffmpeg_args = vec![
            "-y".to_string(),
            "-f".to_string(),
            "avfoundation".to_string(),
            "-framerate".to_string(),
            "1".to_string(),
            "-i".to_string(),
            format!("{}:none", device_index),
            "-frames:v".to_string(),
            "1".to_string(),
            path_str.clone(),
        ];
        if try_capture_command("ffmpeg", &ffmpeg_args).await {
            return Ok(());
        }

        anyhow::bail!(
            "No supported macOS camera capture command succeeded (tried imagesnap and ffmpeg)"
        );
    }

    if cfg!(target_os = "linux") {
        let device_path = format!("/dev/video{}", device_index);
        let ffmpeg_args = vec![
            "-y".to_string(),
            "-f".to_string(),
            "video4linux2".to_string(),
            "-i".to_string(),
            device_path.clone(),
            "-frames:v".to_string(),
            "1".to_string(),
            path_str.clone(),
        ];
        if try_capture_command("ffmpeg", &ffmpeg_args).await {
            return Ok(());
        }

        let fswebcam_args = vec![
            "-d".to_string(),
            device_path.clone(),
            "--no-banner".to_string(),
            path_str.clone(),
        ];
        if try_capture_command("fswebcam", &fswebcam_args).await {
            return Ok(());
        }

        let libcamera_args = vec![
            "-n".to_string(),
            "--camera".to_string(),
            device_index.to_string(),
            "-o".to_string(),
            path_str.clone(),
        ];
        if try_capture_command("libcamera-still", &libcamera_args).await {
            return Ok(());
        }

        anyhow::bail!(
            "No supported Linux camera capture command succeeded (tried ffmpeg, fswebcam, libcamera-still)"
        );
    }

    if cfg!(target_os = "windows") {
        if let Some(name) = device_name {
            let ffmpeg_args = vec![
                "-y".to_string(),
                "-f".to_string(),
                "dshow".to_string(),
                "-i".to_string(),
                format!("video={}", name),
                "-frames:v".to_string(),
                "1".to_string(),
                path_str.clone(),
            ];
            if try_capture_command("ffmpeg", &ffmpeg_args).await {
                return Ok(());
            }
            anyhow::bail!(
                "ffmpeg camera capture failed for device '{}'; verify the camera name and permissions",
                name
            );
        }

        anyhow::bail!(
            "Camera capture on Windows currently requires `device_name` plus ffmpeg (dshow input)"
        );
    }

    anyhow::bail!("Camera capture is not supported on this OS")
}

async fn try_capture_command(cmd: &str, args: &[String]) -> bool {
    let borrowed_args: Vec<&str> = args.iter().map(String::as_str).collect();
    match run_capture_command(cmd, &borrowed_args).await {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(
                "Capture command '{}' failed with args {:?}: {}",
                cmd,
                borrowed_args,
                error
            );
            false
        }
    }
}

async fn run_capture_command(cmd: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .with_context(|| {
            format!(
                "Failed to execute screenshot command '{}' with args {:?}",
                cmd, args
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Screenshot command '{}' failed (status {}): {}",
            cmd,
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn normalize_api_url_for_chat(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.trim_end_matches("/v1").to_string()
    } else {
        trimmed.to_string()
    }
}

fn canonicalize_or_original(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn is_supported_image_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(OsStr::to_str)
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp")
    )
}

fn media_kind_from_path(path: &str) -> String {
    match Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp") => "image".to_string(),
        Some("wav" | "mp3" | "ogg" | "flac" | "m4a") => "audio".to_string(),
        Some("mp4" | "mov" | "webm" | "mkv" | "avi") => "video".to_string(),
        _ => "file".to_string(),
    }
}

fn mime_type_from_path(path: &str) -> String {
    match Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
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
    fn detects_supported_images() {
        assert!(is_supported_image_path("a.png"));
        assert!(is_supported_image_path("a.JPEG"));
        assert!(!is_supported_image_path("a.mp4"));
    }

    #[test]
    fn infers_media_kind() {
        assert_eq!(media_kind_from_path("a.jpg"), "image");
        assert_eq!(media_kind_from_path("b.mp3"), "audio");
        assert_eq!(media_kind_from_path("c.mov"), "video");
        assert_eq!(media_kind_from_path("d.bin"), "file");
    }

    #[test]
    fn normalizes_v1_api_urls() {
        assert_eq!(
            normalize_api_url_for_chat("http://localhost:11434/v1"),
            "http://localhost:11434"
        );
        assert_eq!(
            normalize_api_url_for_chat("http://localhost:11434"),
            "http://localhost:11434"
        );
    }
}
