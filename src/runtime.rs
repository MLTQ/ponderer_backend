use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use flume::Sender;
use futures_util::FutureExt;
use serde::Serialize;

use crate::agent::{Agent, AgentEvent};
use crate::config::AgentConfig;
use crate::database::AgentDatabase;

use crate::plugin_contract::{
    PluginKind, PluginManifest, PluginRuntimeStatus, CURRENT_PLUGIN_MANIFEST_VERSION,
    CURRENT_PLUGIN_PROTOCOL_VERSION,
};
use crate::process_registry::ProcessRegistry;
use crate::runtime_plugin_host::RuntimePluginHost;
use crate::runtime_process_plugin::RuntimeProcessPluginCatalog;
use crate::tools::ToolRegistry;

pub struct BackendRuntime {
    pub config: AgentConfig,
    pub agent: Arc<Agent>,
    pub agent_supervisor: AgentLoopSupervisor,
    pub tool_registry: Arc<ToolRegistry>,
    pub process_registry: Arc<ProcessRegistry>,
    pub runtime_plugin_host: Arc<RuntimePluginHost>,
    pub ui_database: Option<Arc<AgentDatabase>>,
    pub plugin_manifests: Vec<PluginManifest>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct AgentLoopSupervisorStatus {
    pub active: bool,
    pub generation: u64,
    pub restart_count: u64,
    pub last_started_at: Option<DateTime<Utc>>,
    pub last_exited_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Clone, Default)]
pub struct AgentLoopSupervisor {
    status: Arc<RwLock<AgentLoopSupervisorStatus>>,
}

impl AgentLoopSupervisor {
    pub fn snapshot(&self) -> AgentLoopSupervisorStatus {
        self.status
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn mark_generation_started(&self) {
        let mut status = self
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if status.generation > 0 {
            status.restart_count = status.restart_count.saturating_add(1);
        }
        status.generation = status.generation.saturating_add(1);
        status.active = true;
        status.last_started_at = Some(Utc::now());
    }

    fn mark_generation_exited(&self, error: String) {
        let mut status = self
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        status.active = false;
        status.last_exited_at = Some(Utc::now());
        status.last_error = Some(error);
    }
}

const AGENT_RESTART_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const AGENT_RESTART_MAX_BACKOFF: Duration = Duration::from_secs(30);
const PLUGIN_CONTROL_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);

pub struct BackendRuntimeBuilder {
    config: AgentConfig,
    event_tx: Sender<AgentEvent>,
}

impl BackendRuntimeBuilder {
    pub fn new(config: AgentConfig, event_tx: Sender<AgentEvent>) -> Self {
        Self { config, event_tx }
    }

    pub fn build(self) -> Result<BackendRuntime> {
        let config = self.config;
        let tool_registry = Arc::new(ToolRegistry::new());
        let process_registry = Arc::new(ProcessRegistry::new());
        let runtime_process_plugins = Arc::new(RuntimeProcessPluginCatalog::discover()?);
        let ui_database = match AgentDatabase::new(&config.database_path) {
            Ok(db) => Some(Arc::new(db)),
            Err(e) => {
                tracing::warn!("Failed to create shared runtime database: {}", e);
                None
            }
        };
        let runtime_plugin_host = Arc::new(RuntimePluginHost::with_catalog_and_database(
            runtime_process_plugins.clone(),
            ui_database.clone(),
        ));

        let init_rt = tokio::runtime::Runtime::new()?;
        init_rt.block_on(register_builtin_core_tools(
            tool_registry.clone(),
            process_registry.clone(),
            self.event_tx.clone(),
        ))?;

        let mut manifests = vec![builtin_core_manifest()];
        manifests.extend(runtime_process_plugins.manifests());

        let agent = Arc::new(Agent::new(
            tool_registry.clone(),
            runtime_plugin_host.clone(),
            config.clone(),
            self.event_tx,
        ));

        Ok(BackendRuntime {
            config,
            agent,
            agent_supervisor: AgentLoopSupervisor::default(),
            tool_registry,
            process_registry,
            runtime_plugin_host,
            ui_database,
            plugin_manifests: manifests,
        })
    }
}

impl BackendRuntime {
    pub fn bootstrap(config: AgentConfig, event_tx: Sender<AgentEvent>) -> Result<Self> {
        BackendRuntimeBuilder::new(config, event_tx).build()
    }

    pub fn spawn_agent_loop(&self) -> JoinHandle<()> {
        let agent = self.agent.clone();
        let supervisor = self.agent_supervisor.clone();
        let runtime_plugin_host = self.runtime_plugin_host.clone();
        let tool_registry = self.tool_registry.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let error = format!("Failed to create agent Tokio runtime: {error}");
                    supervisor.mark_generation_exited(error.clone());
                    tracing::error!("{error}");
                    return;
                }
            };
            runtime.block_on(supervise_agent_loop(
                agent,
                supervisor,
                runtime_plugin_host,
                tool_registry,
            ));
        })
    }

    pub async fn current_plugin_manifests(&self) -> Vec<PluginManifest> {
        self.runtime_plugin_host.manifests().await
    }

    pub async fn plugin_statuses(&self) -> Vec<PluginRuntimeStatus> {
        self.runtime_plugin_host.statuses().await
    }
}

async fn supervise_agent_loop(
    agent: Arc<Agent>,
    supervisor: AgentLoopSupervisor,
    runtime_plugin_host: Arc<RuntimePluginHost>,
    tool_registry: Arc<ToolRegistry>,
) {
    let mut consecutive_exits = 0_u64;
    loop {
        supervisor.mark_generation_started();
        reconcile_runtime_plugins_once(
            agent.clone(),
            runtime_plugin_host.clone(),
            tool_registry.clone(),
        )
        .await;
        let plugin_control_task = tokio::spawn(supervise_runtime_plugins(
            agent.clone(),
            runtime_plugin_host.clone(),
            tool_registry.clone(),
        ));
        let outcome = AssertUnwindSafe(agent.clone().run_loop())
            .catch_unwind()
            .await;
        plugin_control_task.abort();
        if let Err(error) = plugin_control_task.await {
            if !error.is_cancelled() {
                tracing::warn!("Runtime plugin control task exited abnormally: {error}");
            }
        }
        let error = describe_agent_loop_exit(outcome);
        supervisor.mark_generation_exited(error.clone());

        consecutive_exits = consecutive_exits.saturating_add(1);
        let backoff = agent_restart_backoff(consecutive_exits);
        tracing::error!(
            error = %error,
            restart_in_seconds = backoff.as_secs(),
            "Agent loop exited; supervisor will restart it"
        );
        tokio::time::sleep(backoff).await;
    }
}

async fn supervise_runtime_plugins(
    agent: Arc<Agent>,
    runtime_plugin_host: Arc<RuntimePluginHost>,
    tool_registry: Arc<ToolRegistry>,
) {
    let mut interval = tokio::time::interval(PLUGIN_CONTROL_RECONCILE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;
    loop {
        interval.tick().await;
        reconcile_runtime_plugins_once(
            agent.clone(),
            runtime_plugin_host.clone(),
            tool_registry.clone(),
        )
        .await;
    }
}

async fn reconcile_runtime_plugins_once(
    agent: Arc<Agent>,
    runtime_plugin_host: Arc<RuntimePluginHost>,
    tool_registry: Arc<ToolRegistry>,
) {
    let config = agent.config_snapshot().await;
    if let Err(error) = runtime_plugin_host
        .apply_config(&config, tool_registry)
        .await
    {
        tracing::warn!("Runtime plugin control reconciliation failed: {error:#}");
    }
    if let Err(error) = runtime_plugin_host
        .compact_event_ledger_if_due(Utc::now())
        .await
    {
        tracing::warn!("Runtime plugin event compaction failed: {error:#}");
    }
}

fn describe_agent_loop_exit(
    outcome: std::result::Result<Result<()>, Box<dyn Any + Send>>,
) -> String {
    match outcome {
        Ok(Ok(())) => "Agent loop exited unexpectedly without an error".to_string(),
        Ok(Err(error)) => format!("Agent loop returned an error: {error:#}"),
        Err(payload) => format!("Agent loop panicked: {}", panic_payload_message(&*payload)),
    }
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or("non-string panic payload")
}

fn agent_restart_backoff(consecutive_exit_count: u64) -> Duration {
    let exponent = consecutive_exit_count.saturating_sub(1).min(5) as u32;
    let multiplier = 1_u32 << exponent;
    AGENT_RESTART_INITIAL_BACKOFF
        .saturating_mul(multiplier)
        .min(AGENT_RESTART_MAX_BACKOFF)
}

fn builtin_core_manifest() -> PluginManifest {
    PluginManifest {
        manifest_version: CURRENT_PLUGIN_MANIFEST_VERSION,
        protocol_version: CURRENT_PLUGIN_PROTOCOL_VERSION,
        id: "builtin.core".to_string(),
        kind: PluginKind::Builtin,
        name: "Ponderer Built-ins".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: "Core tools and default runtime wiring provided by ponderer_backend."
            .to_string(),
        provided_tools: vec![
            "shell".to_string(),
            "read_file".to_string(),
            "write_file".to_string(),
            "list_directory".to_string(),
            "patch_file".to_string(),
            "evaluate_local_image".to_string(),
            "publish_media_to_chat".to_string(),
            "capture_screen".to_string(),
            "capture_camera_snapshot".to_string(),
            "search_memory".to_string(),
            "write_memory".to_string(),
            "write_session_handoff".to_string(),
            "private_chat_mode".to_string(),
            "scratch_note".to_string(),
            "http_fetch".to_string(),
            "flag_uncertainty".to_string(),
            "list_scheduled_jobs".to_string(),
            "create_scheduled_job".to_string(),
            "update_scheduled_job".to_string(),
            "delete_scheduled_job".to_string(),
            "plugin_workbench".to_string(),
        ],
        tools: Vec::new(),
        provided_skills: Vec::new(),
        requested_capabilities: Vec::new(),
        declared_effects: Vec::new(),
        contributions: Some(Default::default()),
        settings_tab: None,
        settings_schema: None,
    }
}

async fn register_builtin_core_tools(
    tool_registry: Arc<ToolRegistry>,
    process_registry: Arc<ProcessRegistry>,
    event_tx: Sender<AgentEvent>,
) -> Result<()> {
    use crate::tools::{
        files::{ListDirectoryTool, PatchFileTool, ReadFileTool, WriteFileTool},
        http::HttpFetchTool,
        memory::{
            FlagUncertaintyTool, MemorySearchTool, MemoryWriteTool, PrivateChatModeTool,
            ScratchNoteTool, WriteSessionHandoffTool,
        },
        plugin_workbench::PluginWorkbenchTool,
        scheduled_jobs::{
            CreateScheduledJobTool, DeleteScheduledJobTool, ListScheduledJobsTool,
            UpdateScheduledJobTool,
        },
        shell::ShellTool,
        vision::{
            CaptureCameraSnapshotTool, CaptureScreenTool, EvaluateLocalImageTool,
            PublishMediaToChatTool,
        },
    };

    tool_registry
        .register(Arc::new(ShellTool::new(process_registry)))
        .await;
    tool_registry.register(Arc::new(ReadFileTool::new())).await;
    tool_registry.register(Arc::new(WriteFileTool::new())).await;
    tool_registry
        .register(Arc::new(ListDirectoryTool::new()))
        .await;
    tool_registry.register(Arc::new(PatchFileTool::new())).await;
    tool_registry
        .register(Arc::new(EvaluateLocalImageTool::new()))
        .await;
    tool_registry
        .register(Arc::new(PublishMediaToChatTool::new()))
        .await;
    tool_registry
        .register(Arc::new(CaptureScreenTool::new()))
        .await;
    tool_registry
        .register(Arc::new(CaptureCameraSnapshotTool::new()))
        .await;
    tool_registry
        .register(Arc::new(MemorySearchTool::new()))
        .await;
    tool_registry
        .register(Arc::new(MemoryWriteTool::new()))
        .await;
    tool_registry
        .register(Arc::new(WriteSessionHandoffTool::new()))
        .await;
    tool_registry
        .register(Arc::new(PrivateChatModeTool::new()))
        .await;
    tool_registry
        .register(Arc::new(ScratchNoteTool::new()))
        .await;
    tool_registry.register(Arc::new(HttpFetchTool::new())).await;
    tool_registry
        .register(Arc::new(FlagUncertaintyTool::new(event_tx)))
        .await;
    tool_registry
        .register(Arc::new(ListScheduledJobsTool::new()))
        .await;
    tool_registry
        .register(Arc::new(CreateScheduledJobTool::new()))
        .await;
    tool_registry
        .register(Arc::new(UpdateScheduledJobTool::new()))
        .await;
    tool_registry
        .register(Arc::new(DeleteScheduledJobTool::new()))
        .await;
    tool_registry
        .register(Arc::new(PluginWorkbenchTool::new(
            crate::plugin_workbench::PluginWorkbench::from_environment(),
        )))
        .await;

    tracing::info!("Core tool registry initialized");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_backoff_grows_exponentially_and_is_bounded() {
        let delays = (1..=8)
            .map(agent_restart_backoff)
            .collect::<Vec<Duration>>();

        assert_eq!(
            delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
    }

    #[test]
    fn supervisor_tracks_generation_restarts_and_last_exit() {
        let supervisor = AgentLoopSupervisor::default();
        assert_eq!(supervisor.snapshot(), AgentLoopSupervisorStatus::default());

        supervisor.mark_generation_started();
        let first_generation = supervisor.snapshot();
        assert!(first_generation.active);
        assert_eq!(first_generation.generation, 1);
        assert_eq!(first_generation.restart_count, 0);
        assert!(first_generation.last_started_at.is_some());

        supervisor.mark_generation_exited("first failure".to_string());
        let exited = supervisor.snapshot();
        assert!(!exited.active);
        assert!(exited.last_exited_at.is_some());
        assert_eq!(exited.last_error.as_deref(), Some("first failure"));

        supervisor.mark_generation_started();
        let restarted = supervisor.snapshot();
        assert!(restarted.active);
        assert_eq!(restarted.generation, 2);
        assert_eq!(restarted.restart_count, 1);
        assert_eq!(restarted.last_error.as_deref(), Some("first failure"));
    }

    #[tokio::test]
    async fn panic_is_converted_to_supervisor_error_text() {
        let outcome = AssertUnwindSafe(async {
            panic!("simulated agent panic");
            #[allow(unreachable_code)]
            anyhow::Ok(())
        })
        .catch_unwind()
        .await;

        let error = describe_agent_loop_exit(outcome);
        assert!(error.contains("panicked"));
        assert!(error.contains("simulated agent panic"));
    }
}
