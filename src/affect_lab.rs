//! Supervision of the optional, UI-owned local GGUF experiment/provider process.
//! The ordinary completion and tool-loop clients both use its compatible API.
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::config::AgentConfig;

const WORKER: &str = include_str!("../resources/affect_lab/worker.py");
const EXTRACTOR: &str = include_str!("../resources/affect_lab/extractor.cpp");
pub const LOCAL_MODEL_ALIAS: &str = "ponderer-local-gguf";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AffectLabStart {
    pub model_path: String,
    pub gpu_layers: i32,
    pub threads: u32,
    pub context_size: u32,
    pub server_binary: String,
}

impl Default for AffectLabStart {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            gpu_layers: 0,
            threads: 4,
            context_size: 16384,
            server_binary: "llama-server".into(),
        }
    }
}

impl AffectLabStart {
    fn validate(&self) -> Result<()> {
        if self.model_path.trim().is_empty() || self.model_path.contains('\0') {
            bail!("Choose an existing GGUF model or model directory");
        }
        if !(0..=999).contains(&self.gpu_layers) || !(1..=128).contains(&self.threads) {
            bail!("GPU layers must be 0..999 and CPU threads 1..128");
        }
        if !(1024..=65536).contains(&self.context_size) {
            bail!("Context size must be 1024..65536");
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
        tokio::fs::write(resources.join("extractor.cpp"), EXTRACTOR).await?;
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
            .arg("--server-binary")
            .arg(settings.server_binary)
            .env("PONDERER_AFFECT_TOKEN", &token)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log_file))
            .kill_on_drop(true);
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
        if !matches!(action, "build" | "compare" | "profile" | "cancel") {
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
    fn experiment_settings_reject_invalid_resources() {
        let mut settings = AffectLabStart {
            model_path: "/tmp/model.gguf".into(),
            ..Default::default()
        };
        assert!(settings.validate().is_ok());
        settings.threads = 0;
        assert!(settings.validate().is_err());
        settings.threads = 4;
        settings.gpu_layers = -1;
        assert!(settings.validate().is_err());
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
