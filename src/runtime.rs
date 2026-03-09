use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use flume::Sender;

use crate::agent::{Agent, AgentEvent};
use crate::config::AgentConfig;
use crate::database::AgentDatabase;
use crate::plugin::{
    BackendPlugin, BackendPluginKind, BackendPluginManifest, PluginSettingsTabManifest,
};
use crate::process_registry::ProcessRegistry;
use crate::runtime_plugin_host::RuntimePluginHost;
use crate::runtime_process_plugin::RuntimeProcessPluginCatalog;
use crate::skills::Skill;
use crate::tools::ToolRegistry;

pub struct BackendRuntime {
    pub config: AgentConfig,
    pub agent: Arc<Agent>,
    pub tool_registry: Arc<ToolRegistry>,
    pub process_registry: Arc<ProcessRegistry>,
    pub runtime_plugin_host: Arc<RuntimePluginHost>,
    pub ui_database: Option<Arc<AgentDatabase>>,
    pub plugin_manifests: Vec<BackendPluginManifest>,
}

pub struct BackendRuntimeBuilder {
    config: AgentConfig,
    event_tx: Sender<AgentEvent>,
    plugins: Vec<Arc<dyn BackendPlugin>>,
}

impl BackendRuntimeBuilder {
    pub fn new(config: AgentConfig, event_tx: Sender<AgentEvent>) -> Self {
        Self {
            config,
            event_tx,
            plugins: Vec::new(),
        }
    }

    pub fn with_plugin(mut self, plugin: Arc<dyn BackendPlugin>) -> Self {
        self.plugins.push(plugin);
        self
    }

    pub fn with_plugins<I>(mut self, plugins: I) -> Self
    where
        I: IntoIterator<Item = Arc<dyn BackendPlugin>>,
    {
        self.plugins.extend(plugins);
        self
    }

    pub fn build(self) -> Result<BackendRuntime> {
        let config = self.config;
        let mut skill_list = build_builtin_skills(&config);
        let tool_registry = Arc::new(ToolRegistry::new());
        let process_registry = Arc::new(ProcessRegistry::new());
        let runtime_process_plugins = Arc::new(RuntimeProcessPluginCatalog::discover()?);
        let runtime_plugin_host = Arc::new(RuntimePluginHost::with_catalog(
            runtime_process_plugins.clone(),
        ));

        let init_rt = tokio::runtime::Runtime::new()?;
        init_rt.block_on(register_builtin_core_tools(
            tool_registry.clone(),
            process_registry.clone(),
            self.event_tx.clone(),
        ))?;
        init_rt.block_on(register_builtin_orbweaver_tools(tool_registry.clone()))?;

        let mut manifests = vec![builtin_core_manifest(), builtin_orbweaver_manifest()];
        manifests.extend(runtime_process_plugins.manifests());
        for plugin in self.plugins {
            let manifest = plugin.manifest();
            let plugin_id = manifest.id.clone();
            let mut plugin_skills = plugin
                .build_skills(&config)
                .with_context(|| format!("Plugin '{}' failed to build skills", plugin_id))?;
            let plugin_skill_count = plugin_skills.len();
            skill_list.append(&mut plugin_skills);

            init_rt
                .block_on(plugin.register_tools(tool_registry.clone(), &config))
                .with_context(|| format!("Plugin '{}' failed to register tools", plugin_id))?;

            tracing::info!(
                "Loaded plugin '{}' (skills added: {}, tools: {:?})",
                plugin_id,
                plugin_skill_count,
                manifest.provided_tools
            );
            manifests.push(manifest);
        }

        let ui_database = match AgentDatabase::new(&config.database_path) {
            Ok(db) => Some(Arc::new(db)),
            Err(e) => {
                tracing::warn!("Failed to create UI database: {}", e);
                None
            }
        };

        let agent = Arc::new(Agent::new(
            skill_list,
            tool_registry.clone(),
            runtime_plugin_host.clone(),
            config.clone(),
            self.event_tx,
        ));

        Ok(BackendRuntime {
            config,
            agent,
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
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("backend runtime thread");
            rt.block_on(async {
                if let Err(e) = agent.run_loop().await {
                    tracing::error!("Agent loop error: {}", e);
                }
            });
        })
    }
}

fn build_builtin_skills(config: &AgentConfig) -> Vec<Box<dyn Skill>> {
    let mut skill_list: Vec<Box<dyn Skill>> = Vec::new();
    skill_list.extend(build_builtin_orbweaver_skills(config));
    tracing::info!("Loaded {} built-in skill(s)", skill_list.len());
    skill_list
}

fn build_builtin_orbweaver_skills(config: &AgentConfig) -> Vec<Box<dyn Skill>> {
    let mut skill_list: Vec<Box<dyn Skill>> = Vec::new();
    if !config.graphchan_api_url.trim().is_empty() {
        tracing::info!("Graphchan skill enabled: {}", config.graphchan_api_url);
        skill_list.push(Box::new(crate::skills::graphchan::GraphchanSkill::new(
            config.graphchan_api_url.clone(),
        )));
    }
    skill_list
}

fn builtin_core_manifest() -> BackendPluginManifest {
    BackendPluginManifest {
        id: "builtin.core".to_string(),
        kind: BackendPluginKind::Builtin,
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
        ],
        provided_skills: Vec::new(),
        settings_tab: None,
        settings_schema: None,
    }
}

fn builtin_orbweaver_manifest() -> BackendPluginManifest {
    BackendPluginManifest {
        id: "builtin.orbweaver".to_string(),
        kind: BackendPluginKind::Builtin,
        name: "OrbWeaver".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: "Built-in OrbWeaver / Graphchan integration.".to_string(),
        provided_tools: vec![
            "post_to_graphchan".to_string(),
            "graphchan_skill".to_string(),
        ],
        provided_skills: vec!["graphchan".to_string()],
        settings_tab: Some(PluginSettingsTabManifest {
            id: "skill.orbweaver".to_string(),
            title: "OrbWeaver".to_string(),
            order: 210,
        }),
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

    tracing::info!("Core tool registry initialized");
    Ok(())
}

async fn register_builtin_orbweaver_tools(tool_registry: Arc<ToolRegistry>) -> Result<()> {
    use crate::tools::{comfy::PostToGraphchanTool, skill_bridge::GraphchanSkillTool};

    tool_registry
        .register(Arc::new(PostToGraphchanTool::new()))
        .await;
    tool_registry
        .register(Arc::new(GraphchanSkillTool::new()))
        .await;
    tracing::info!("OrbWeaver plugin tools initialized");
    Ok(())
}
