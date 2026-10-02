//! Supervision of the optional, UI-owned local GGUF experiment/provider process.
//! The ordinary completion and tool-loop clients both use its compatible API.
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::config::AgentConfig;

const WORKER: &str = include_str!("../resources/affect_lab/worker.py");
const DISCOVERY: &str = include_str!("../resources/affect_lab/affect_discovery.py");
const EXTRACTOR: &str = include_str!("../resources/affect_lab/extractor.cpp");
const QWEN35_TEMPLATE: &str = include_str!("../resources/affect_lab/qwen35-tools.jinja");
const TEMPLATE_LICENSE: &str = include_str!("../resources/affect_lab/LLAMA_TEMPLATE_LICENSE");
pub const LOCAL_MODEL_ALIAS: &str = "ponderer-local-gguf";
pub const MAX_CONTEXT_SIZE: u32 = 1_048_576;
pub const CACHE_TYPES: &[&str] = &[
    "f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AffectLabStart {
    pub model_path: String,
    /// -1 is explicit full offload (native `all`), 0 is CPU, positive is partial.
    pub gpu_layers: i32,
    pub gpu_device: Option<String>,
    pub threads: u32,
    pub context_size: u32,
    pub unified_kv_cache: bool,
    pub cache_type_k: String,
    pub cache_type_v: String,
    pub flash_attention: String,
    pub server_binary: String,
}

impl Default for AffectLabStart {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            gpu_layers: -1,
            gpu_device: None,
            threads: 4,
            context_size: 16384,
            unified_kv_cache: true,
            cache_type_k: "f16".into(),
            cache_type_v: "f16".into(),
            flash_attention: "auto".into(),
            server_binary: "llama-server".into(),
        }
    }
}

impl AffectLabStart {
    /// Match the requested LM Studio memory settings without changing device
    /// placement or selecting an unverified engine executable.
    pub fn apply_200k_preset(&mut self) {
        self.context_size = 200_000;
        self.unified_kv_cache = true;
        self.cache_type_k = "q4_1".into();
        self.cache_type_v = "q4_1".into();
        self.flash_attention = "on".into();
    }

    fn validate(&self) -> Result<()> {
        if self.model_path.trim().is_empty() || self.model_path.contains('\0') {
            bail!("Choose an existing GGUF model or model directory");
        }
        if !(-1..=999).contains(&self.gpu_layers) || !(1..=128).contains(&self.threads) {
            bail!("GPU layers must be -1 (all), 0 (CPU), or 1..999; CPU threads 1..128");
        }
        if self
            .gpu_device
            .as_deref()
            .is_some_and(|id| !valid_device_id(id))
        {
            bail!("Choose one device ID reported by the selected llama-server");
        }
        if self.gpu_layers != 0 && self.gpu_device.is_none() {
            bail!("Scan GPUs and choose a device before loading; automatic multi-GPU placement is disabled");
        }
        if !(1024..=MAX_CONTEXT_SIZE).contains(&self.context_size) {
            bail!("Context size must be 1024..{MAX_CONTEXT_SIZE}");
        }
        if !CACHE_TYPES.contains(&self.cache_type_k.as_str())
            || !CACHE_TYPES.contains(&self.cache_type_v.as_str())
        {
            bail!("Choose a supported K/V cache type");
        }
        if !matches!(self.flash_attention.as_str(), "on" | "off" | "auto") {
            bail!("Flash attention must be on, off or auto");
        }
        if self.flash_attention == "off"
            && !matches!(self.cache_type_v.as_str(), "f32" | "f16" | "bf16")
        {
            bail!("Quantized V cache requires flash attention; select on or auto");
        }
        if self.server_binary.trim().is_empty() || self.server_binary.contains('\0') {
            bail!("Choose a llama-server executable");
        }
        Ok(())
    }
}

struct ManagedWorker {
    child: Child,
    base_url: String,
    token: String,
}

impl Drop for ManagedWorker {
    fn drop(&mut self) {
        // kill_on_drop and Linux parent-death signaling cover both orderly and
        // abrupt backend exits. Python gives the same guarantee to native children.
        let _ = self.child.start_kill();
    }
}

#[derive(Clone)]
struct ProviderSelection {
    url: String,
    model: String,
    key: Option<String>,
    reflection_model: Option<String>,
    decision_model: Option<String>,
}

impl ProviderSelection {
    fn capture(config: &AgentConfig) -> Self {
        Self {
            url: config.llm_api_url.clone(),
            model: config.llm_model.clone(),
            key: config.llm_api_key.clone(),
            reflection_model: config.reflection_model.clone(),
            decision_model: config.respond_to.decision_model.clone(),
        }
    }

    fn restore(&self, config: &mut AgentConfig) {
        config.llm_api_url = self.url.clone();
        config.llm_model = self.model.clone();
        config.llm_api_key = self.key.clone();
        config.reflection_model = self.reflection_model.clone();
        config.respond_to.decision_model = self.decision_model.clone();
    }
}

pub struct AffectLabManager {
    worker: Mutex<Option<ManagedWorker>>,
    previous_provider: Mutex<Option<ProviderSelection>>,
    http: reqwest::Client,
    data_dir: PathBuf,
}

impl AffectLabManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            worker: Mutex::new(None),
            previous_provider: Mutex::new(None),
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("local affect control client"),
            data_dir,
        }
    }

    pub fn default_data_dir() -> PathBuf {
        std::env::var_os("PONDERER_AFFECT_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("affect_lab")
            })
    }

    pub async fn start(&self, settings: AffectLabStart) -> Result<Value> {
        settings.validate()?;
        if std::env::var("PONDERER_BACKEND_PARENT_PIPE").as_deref() != Ok("1") {
            bail!("Start Affect Lab from the desktop UI; managed inference requires its backend parent-pipe safeguard");
        }
        if !cfg!(target_os = "linux") {
            bail!("This managed local provider currently requires Linux parent-death signaling");
        }
        let mut guard = self.worker.lock().await;
        if let Some(worker) = guard.as_mut() {
            if worker.child.try_wait()?.is_none() {
                bail!("Stop the current local provider before selecting another model");
            }
            // A previously selected provider must first be restored through Stop.
            bail!("The provider exited; press Stop to restore the previous provider before restarting");
        }
        let resources = self.data_dir.join("worker");
        tokio::fs::create_dir_all(&resources).await?;
        let worker_path = resources.join("worker.py");
        tokio::fs::write(&worker_path, WORKER).await?;
        tokio::fs::write(resources.join("affect_discovery.py"), DISCOVERY).await?;
        tokio::fs::write(resources.join("extractor.cpp"), EXTRACTOR).await?;
        tokio::fs::write(resources.join("qwen35-tools.jinja"), QWEN35_TEMPLATE).await?;
        tokio::fs::write(resources.join("LLAMA_TEMPLATE_LICENSE"), TEMPLATE_LICENSE).await?;
        let log_file = std::fs::File::create(self.data_dir.join("worker.log"))?;
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let mut command = Command::new("python3");
        command
            .arg("-u")
            .arg(&worker_path)
            .arg("serve")
            .arg("--model")
            .arg(settings.model_path)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--gpu-layers")
            .arg(settings.gpu_layers.to_string())
            .arg("--threads")
            .arg(settings.threads.to_string())
            .arg("--context-size")
            .arg(settings.context_size.to_string())
            .arg("--cache-type-k")
            .arg(settings.cache_type_k)
            .arg("--cache-type-v")
            .arg(settings.cache_type_v)
            .arg("--flash-attn")
            .arg(settings.flash_attention)
            .arg(if settings.unified_kv_cache {
                "--kv-unified"
            } else {
                "--no-kv-unified"
            })
            .arg("--server-binary")
            .arg(settings.server_binary)
            .env("PONDERER_AFFECT_TOKEN", &token)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log_file))
            .kill_on_drop(true);
        if let Some(device) = settings.gpu_device {
            command.arg("--gpu-device").arg(device);
        }
        set_parent_death_signal(&mut command);
        let mut child = command
            .spawn()
            .context("Cannot start Affect Lab; Python 3 is required")?;
        let mut output =
            BufReader::new(child.stdout.take().context("Missing worker startup pipe")?).lines();
        let startup = tokio::time::timeout(Duration::from_secs(30), output.next_line())
            .await
            .context("Affect Lab startup timed out")??
            .context("Affect Lab exited during startup; see worker.log")?;
        let startup: Value =
            serde_json::from_str(&startup).context("Invalid Affect Lab startup response")?;
        let port = startup["port"]
            .as_u64()
            .filter(|port| *port > 0 && *port <= 65535)
            .context("Invalid Affect Lab port")?;
        let worker = ManagedWorker {
            child,
            base_url: format!("http://127.0.0.1:{port}"),
            token,
        };
        *guard = Some(worker);
        drop(guard);
        self.status().await
    }

    pub async fn status(&self) -> Result<Value> {
        let mut guard = self.worker.lock().await;
        let Some(worker) = guard.as_mut() else {
            return Ok(
                json!({"running": false, "used_by_agent": false, "concepts": ["contentment", "satisfaction", "excitement"], "vectors": []}),
            );
        };
        if let Some(code) = worker.child.try_wait()? {
            return Ok(
                json!({"running": false, "used_by_agent": self.previous_provider.lock().await.is_some(), "error": format!("Local provider exited ({code}); Stop restores the previous provider"), "vectors": []}),
            );
        }
        let mut value = self
            .send(worker, reqwest::Method::GET, "/control/status", None)
            .await?;
        value["api_url"] = json!(format!("{}/v1", worker.base_url));
        value["used_by_agent"] = json!(self.previous_provider.lock().await.is_some());
        Ok(value)
    }

    pub async fn control(&self, action: &str, values: Value) -> Result<Value> {
        if action == "devices" {
            if std::env::var("PONDERER_BACKEND_PARENT_PIPE").as_deref() != Ok("1")
                || !cfg!(target_os = "linux")
            {
                bail!("Scan GPUs from the desktop UI; device probes require its process-lifetime safeguard");
            }
            let executable = values["server_binary"]
                .as_str()
                .context("Choose a llama-server executable")?;
            return probe_devices(executable).await;
        }
        if !matches!(
            action,
            "build" | "compare" | "profile" | "cancel" | "load" | "review" | "discover" | "study"
        ) {
            bail!("Unknown Affect Lab action");
        }
        let mut guard = self.worker.lock().await;
        let worker = guard.as_mut().context("Start the local provider first")?;
        if worker.child.try_wait()?.is_some() {
            bail!("Local provider exited; press Stop and restart");
        }
        let mut value = self
            .send(
                worker,
                reqwest::Method::POST,
                &format!("/control/{action}"),
                Some(values),
            )
            .await?;
        value["used_by_agent"] = json!(self.previous_provider.lock().await.is_some());
        value["api_url"] = json!(format!("{}/v1", worker.base_url));
        Ok(value)
    }

    async fn send(
        &self,
        worker: &ManagedWorker,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", worker.base_url))
            .bearer_auth(&worker.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .context("Cannot reach local Affect Lab")?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .context("Invalid local Affect Lab response")?;
        if !status.is_success() {
            bail!(
                "{}",
                value["error"]
                    .as_str()
                    .unwrap_or("Local Affect Lab request failed")
            );
        }
        Ok(value)
    }

    pub async fn select_for_agent(&self, config: &mut AgentConfig) -> Result<()> {
        let mut guard = self.worker.lock().await;
        let worker = guard.as_mut().context("Start the local provider first")?;
        if worker.child.try_wait()?.is_some() {
            bail!("Local provider has exited");
        }
        let status = self
            .send(worker, reqwest::Method::GET, "/control/status", None)
            .await?;
        validate_loaded_provider(&status)?;
        let mut previous = self.previous_provider.lock().await;
        if previous.is_none() {
            *previous = Some(ProviderSelection::capture(config));
        }
        config.llm_api_url = format!("{}/v1", worker.base_url);
        config.llm_model = LOCAL_MODEL_ALIAS.into();
        config.llm_api_key = Some(worker.token.clone());
        config.reflection_model = None;
        config.respond_to.decision_model = None;
        Ok(())
    }

    pub async fn restore_provider(&self, config: &mut AgentConfig) {
        if let Some(previous) = self.previous_provider.lock().await.take() {
            previous.restore(config);
        }
    }

    /// Never persist an ephemeral host URL/token. Settings changes can deliberately
    /// select a different provider; unrelated changes preserve the session override.
    pub async fn config_to_save(&self, config: &AgentConfig) -> AgentConfig {
        let guard = self.worker.lock().await;
        let mut previous = self.previous_provider.lock().await;
        let mut durable = config.clone();
        if let (Some(worker), Some(original)) = (guard.as_ref(), previous.as_ref()) {
            if config.llm_api_url == format!("{}/v1", worker.base_url)
                && config.llm_model == LOCAL_MODEL_ALIAS
            {
                original.restore(&mut durable);
            } else {
                *previous = None;
            }
        }
        durable
    }

    pub async fn stop(&self) -> Result<()> {
        let worker = self.worker.lock().await.take();
        if let Some(mut worker) = worker {
            // Ask Python to reap native children, then terminate the worker. Abrupt
            // death still kills every native child through PR_SET_PDEATHSIG.
            let _ = self
                .send(
                    &worker,
                    reqwest::Method::POST,
                    "/control/cancel",
                    Some(json!({})),
                )
                .await;
            worker.child.start_kill()?;
            worker.child.wait().await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GpuDevice {
    pub id: String,
    pub name: String,
    pub memory_total_mib: Option<u64>,
    pub memory_free_mib: Option<u64>,
}

fn validate_loaded_provider(status: &Value) -> Result<()> {
    if status["job"]["phase"].as_str() == Some("running") {
        bail!("Wait for the local model operation to finish before using it for this session");
    }
    if !status["native_pid"].is_number() || !status["applied_profile"].is_object() {
        bail!(
            "Load the local model successfully before using it for this session. {}",
            status["job"]["error"]
                .as_str()
                .unwrap_or("The inference engine is not ready.")
        );
    }
    Ok(())
}

fn valid_device_id(id: &str) -> bool {
    id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id.len() <= 64
        && id != "none"
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

fn parse_devices(output: &str) -> Vec<GpuDevice> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let Some((id, description)) = line.trim().split_once(':') else {
            continue;
        };
        let id = id.trim();
        if !valid_device_id(id)
            || id == "Available devices"
            || devices.iter().any(|d: &GpuDevice| d.id == id)
        {
            continue;
        }
        let description = description.trim();
        if description.is_empty() {
            continue;
        }
        let (name, total, free) = if let Some((name, memory)) = description
            .rsplit_once(" (")
            .filter(|(_, memory)| memory.ends_with(')') && memory.contains("MiB"))
        {
            let number = |text: &str| {
                text.split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
            };
            let mut values = memory.trim_end_matches(')').split(',');
            (
                name,
                values.next().and_then(number),
                values.next().and_then(number),
            )
        } else {
            (description, None, None)
        };
        devices.push(GpuDevice {
            id: id.into(),
            name: name.into(),
            memory_total_mib: total,
            memory_free_mib: free,
        });
    }
    devices
}

async fn read_probe_output(mut reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    const LIMIT: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    (&mut reader)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > LIMIT {
        bail!("GPU probe output exceeded 64 KiB");
    }
    Ok(bytes)
}

async fn probe_devices(executable: &str) -> Result<Value> {
    probe_devices_with_timeout(executable, Duration::from_secs(10)).await
}

async fn probe_devices_with_timeout(executable: &str, timeout: Duration) -> Result<Value> {
    if executable.trim().is_empty() || executable.contains('\0') {
        bail!("Choose a llama-server executable");
    }
    let mut command = Command::new(executable);
    command
        .arg("--list-devices")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    set_parent_death_signal(&mut command);
    let mut child = command
        .spawn()
        .context("Cannot scan GPUs with this llama-server")?;
    let stdout = child.stdout.take().context("Missing device probe stdout")?;
    let stderr = child.stderr.take().context("Missing device probe stderr")?;
    let (stdout, stderr, status) = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            read_probe_output(stdout),
            read_probe_output(stderr),
            async { Ok::<_, anyhow::Error>(child.wait().await?) }
        )
    })
    .await
    .with_context(|| format!("GPU scan timed out after {} seconds", timeout.as_secs_f32()))??;
    if !status.success() {
        bail!(
            "llama-server GPU scan failed ({status}): {}",
            String::from_utf8_lossy(&stderr)
                .chars()
                .take(1000)
                .collect::<String>()
        );
    }
    let mut text = String::from_utf8_lossy(&stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&stderr));
    Ok(json!({"server_binary":executable,"devices":parse_devices(&text)}))
}

fn set_parent_death_signal(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        let parent = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "Owning backend exited before worker startup",
                    ));
                }
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_selection_requires_a_successfully_loaded_engine() {
        assert!(validate_loaded_provider(&json!({"running":true,"native_pid":null,"job":{"phase":"failed","error":"Incompatible CUDA engine"}})).unwrap_err().to_string().contains("Incompatible CUDA engine"));
        assert!(validate_loaded_provider(
            &json!({"native_pid":123,"applied_profile":{},"job":{"phase":"running"}})
        )
        .is_err());
        assert!(validate_loaded_provider(
            &json!({"native_pid":123,"applied_profile":{},"job":{"phase":"complete"}})
        )
        .is_ok());
    }

    #[test]
    fn experiment_settings_reject_invalid_resources() {
        let mut settings = AffectLabStart {
            model_path: "/tmp/model.gguf".into(),
            gpu_device: Some("CUDA0".into()),
            ..Default::default()
        };
        assert!(settings.validate().is_ok());
        settings.threads = 0;
        assert!(settings.validate().is_err());
        settings.threads = 4;
        settings.gpu_layers = -2;
        assert!(settings.validate().is_err());
        settings.gpu_layers = -1;
        settings.gpu_device = None;
        assert!(settings.validate().is_err());
        settings.gpu_layers = 0;
        assert!(settings.validate().is_ok());
        settings.gpu_device = Some("CUDA0,CUDA1".into());
        assert!(settings.validate().is_err());
        settings.gpu_device = Some("--list-devices".into());
        assert!(settings.validate().is_err());
    }

    #[test]
    fn long_context_preset_validates_and_preserves_device_selection() {
        let mut settings = AffectLabStart {
            model_path: "/tmp/model.gguf".into(),
            gpu_layers: 17,
            gpu_device: Some("CUDA1".into()),
            server_binary: "/custom/llama-server".into(),
            ..Default::default()
        };
        settings.apply_200k_preset();
        assert!(settings.validate().is_ok());
        assert_eq!(settings.context_size, 200_000);
        assert_eq!(settings.cache_type_k, "q4_1");
        assert_eq!(settings.cache_type_v, "q4_1");
        assert_eq!(settings.flash_attention, "on");
        assert!(settings.unified_kv_cache);
        assert_eq!(settings.gpu_layers, 17);
        assert_eq!(settings.gpu_device.as_deref(), Some("CUDA1"));
        assert_eq!(settings.server_binary, "/custom/llama-server");
        settings.flash_attention = "off".into();
        assert!(settings.validate().is_err());
        settings.flash_attention = "on".into();
        settings.context_size = MAX_CONTEXT_SIZE + 1;
        assert!(settings.validate().is_err());
        settings.context_size = 200_000;
        settings.cache_type_k = "q4_k_m".into();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn legacy_start_requests_keep_conservative_cache_defaults() {
        let settings: AffectLabStart =
            serde_json::from_str(r#"{"model_path":"model.gguf"}"#).unwrap();
        assert_eq!(settings.cache_type_k, "f16");
        assert_eq!(settings.cache_type_v, "f16");
        assert_eq!(settings.flash_attention, "auto");
        assert!(settings.unified_kv_cache);
        assert_eq!(settings.gpu_layers, -1);
        assert_eq!(settings.gpu_device, None);
    }

    #[test]
    fn engine_inventory_preserves_native_ids_not_nvidia_order() {
        let devices=parse_devices("Available devices:\n  CUDA0: NVIDIA GeForce RTX 4090 (24107 MiB, 15344 MiB free)\n  CUDA1: NVIDIA GeForce RTX 2070 SUPER (7794 MiB, 2212 MiB free)\nCUDA0: duplicate\n0.00.123 I log: ignored\n");
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, "CUDA0");
        assert_eq!(devices[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(devices[0].memory_total_mib, Some(24107));
        assert_eq!(devices[1].memory_free_mib, Some(2212));
        assert!(parse_devices("Available devices:\n(none)").is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn device_probe_runs_only_list_devices_and_bounds_output() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("probe");
        std::fs::write(&executable,"#!/bin/sh\n[ \"$#\" = 1 ] && [ \"$1\" = --list-devices ] || exit 7\nprintf 'Available devices:\\nCUDA0: Fixture GPU (24000 MiB, 20000 MiB free)\\n'\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = probe_devices(executable.to_str().unwrap()).await.unwrap();
        assert_eq!(result["server_binary"], executable.to_str().unwrap());
        assert_eq!(result["devices"][0]["id"], "CUDA0");
        assert_eq!(result["devices"][0]["memory_free_mib"], 20000);
        assert!(
            read_probe_output(std::io::Cursor::new(vec![b'x'; 64 * 1024 + 1]))
                .await
                .is_err()
        );
        assert!(probe_devices("").await.is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn timed_out_gpu_probe_is_terminated() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("slow-probe");
        let pid_file = temp.path().join("probe.pid");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 30\n",
                pid_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error =
            probe_devices_with_timeout(executable.to_str().unwrap(), Duration::from_millis(200))
                .await
                .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let pid: u32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
        for _ in 0..100 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Timed-out probe {pid} survived");
    }

    #[test]
    fn provider_restore_preserves_remote_overrides() {
        let mut config = AgentConfig::default();
        config.llm_api_url = "https://provider.example/v1".into();
        config.reflection_model = Some("reflection-model".into());
        config.respond_to.decision_model = Some("decision-model".into());
        let original = ProviderSelection::capture(&config);
        config.llm_api_url = "http://127.0.0.1:1234/v1".into();
        config.reflection_model = None;
        config.respond_to.decision_model = None;
        original.restore(&mut config);
        assert_eq!(config.llm_api_url, "https://provider.example/v1");
        assert_eq!(config.reflection_model.as_deref(), Some("reflection-model"));
        assert_eq!(
            config.respond_to.decision_model.as_deref(),
            Some("decision-model")
        );
    }
}
