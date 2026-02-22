use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use flume::Sender;

use crate::agent::{Agent, AgentEvent};
use crate::config::AgentConfig;
use crate::database::AgentDatabase;
use crate::plugin::{BackendPlugin, BackendPluginManifest};
use crate::skills::Skill;
use crate::tools::ToolRegistry;

pub struct BackendRuntime {
    pub config: AgentConfig,
    pub agent: Arc<Agent>,
    pub tool_registry: Arc<ToolRegistry>,
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
        let mut skill_list = build_skills(&config);
        let tool_registry = Arc::new(ToolRegistry::new());

        let init_rt = tokio::runtime::Runtime::new()?;
        init_rt.block_on(register_builtin_tools(tool_registry.clone()))?;

        let mut manifests = vec![builtin_manifest(&config)];
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
            config.clone(),
            self.event_tx,
        ));

        Ok(BackendRuntime {
            config,
            agent,
            tool_registry,
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

fn build_skills(config: &AgentConfig) -> Vec<Box<dyn Skill>> {
    let mut skill_list: Vec<Box<dyn Skill>> = Vec::new();

    if !config.graphchan_api_url.is_empty() {
        tracing::info!("Graphchan skill enabled: {}", config.graphchan_api_url);
        skill_list.push(Box::new(crate::skills::graphchan::GraphchanSkill::new(
            config.graphchan_api_url.clone(),
        )));
    }

    tracing::info!("Loaded {} built-in skill(s)", skill_list.len());
    skill_list
}

fn builtin_manifest(config: &AgentConfig) -> BackendPluginManifest {
    let mut provided_skills = Vec::new();
    if !config.graphchan_api_url.trim().is_empty() {
        provided_skills.push("graphchan".to_string());
    }

    BackendPluginManifest {
        id: "builtin.core".to_string(),
        name: "Ponderer Built-ins".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: "Core tools and default skill wiring provided by ponderer_backend."
            .to_string(),
        provided_tools: vec![
            "shell".to_string(),
            "read_file".to_string(),
            "write_file".to_string(),
            "list_directory".to_string(),
            "patch_file".to_string(),
            "generate_comfy_media".to_string(),
            "post_to_graphchan".to_string(),
            "evaluate_local_image".to_string(),
            "publish_media_to_chat".to_string(),
            "capture_screen".to_string(),
            "capture_camera_snapshot".to_string(),
            "search_memory".to_string(),
            "write_memory".to_string(),
            "http_fetch".to_string(),
            "graphchan_skill".to_string(),
        ],
        provided_skills,
    }
}

async fn register_builtin_tools(tool_registry: Arc<ToolRegistry>) -> Result<()> {
    use crate::tools::{
        comfy::{GenerateComfyMediaTool, PostToGraphchanTool},
        files::{ListDirectoryTool, PatchFileTool, ReadFileTool, WriteFileTool},
        http::HttpFetchTool,
        memory::{MemorySearchTool, MemoryWriteTool, WriteSessionHandoffTool},
        shell::ShellTool,
        skill_bridge::GraphchanSkillTool,
        vision::{
            CaptureCameraSnapshotTool, CaptureScreenTool, EvaluateLocalImageTool,
            PublishMediaToChatTool,
        },
    };

    tool_registry.register(Arc::new(ShellTool::new())).await;
    tool_registry.register(Arc::new(ReadFileTool::new())).await;
    tool_registry.register(Arc::new(WriteFileTool::new())).await;
    tool_registry
        .register(Arc::new(ListDirectoryTool::new()))
        .await;
    tool_registry.register(Arc::new(PatchFileTool::new())).await;
    tool_registry
        .register(Arc::new(GenerateComfyMediaTool::new()))
        .await;
    tool_registry
        .register(Arc::new(PostToGraphchanTool::new()))
        .await;
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
    tool_registry.register(Arc::new(HttpFetchTool::new())).await;
    tool_registry
        .register(Arc::new(GraphchanSkillTool::new()))
        .await;

    tracing::info!("Tool registry initialized with 16 built-in tools");
    Ok(())
}
