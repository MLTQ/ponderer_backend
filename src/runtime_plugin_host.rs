use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock};

use crate::config::AgentConfig;
use crate::plugin::BackendPluginManifest;
use crate::runtime_process_plugin::{
    RuntimeProcessPluginBundle, SharedRuntimeProcessPluginCatalog,
};
use crate::tools::runtime_plugin::RuntimePluginToolProxy;
use crate::tools::{ToolCategory, ToolOutput, ToolRegistry};

const DEFAULT_MAX_CONTRIBUTION_CHARS: usize = 300;
const DEFAULT_MAX_SLOT_TOTAL_CHARS: usize = 1_200;
const MAX_NON_JSON_PLUGIN_LINES: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginRpcRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginRpcError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginRpcResponse {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<RuntimePluginRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePluginToolCategory {
    FileSystem,
    Shell,
    Network,
    Memory,
    General,
}

impl Default for RuntimePluginToolCategory {
    fn default() -> Self {
        Self::General
    }
}

impl RuntimePluginToolCategory {
    pub fn as_tool_category(&self) -> ToolCategory {
        match self {
            RuntimePluginToolCategory::FileSystem => ToolCategory::FileSystem,
            RuntimePluginToolCategory::Shell => ToolCategory::Shell,
            RuntimePluginToolCategory::Network => ToolCategory::Network,
            RuntimePluginToolCategory::Memory => ToolCategory::Memory,
            RuntimePluginToolCategory::General => ToolCategory::General,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginToolManifest {
    pub name: String,
    pub description: String,
    #[serde(default = "default_tool_parameters_schema")]
    pub parameters: Value,
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub category: RuntimePluginToolCategory,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimePluginCapabilities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub event_hooks: Vec<String>,
    #[serde(default)]
    pub prompt_slots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginHandshake {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: RuntimePluginCapabilities,
    #[serde(default)]
    pub tools: Vec<RuntimePluginToolManifest>,
}

impl RuntimePluginHandshake {
    pub fn tool_names(&self) -> Vec<String> {
        if !self.tools.is_empty() {
            return self.tools.iter().map(|tool| tool.name.clone()).collect();
        }
        self.capabilities.tools.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum PromptContributionSlot {
    EngagedInstructions,
    EngagedContext,
    AmbientInstructions,
    OrientationContext,
    ReflectionConsiderations,
    PersonaEvolutionConsiderations,
}

impl PromptContributionSlot {
    pub fn wire_name(self) -> &'static str {
        match self {
            PromptContributionSlot::EngagedInstructions => "engaged.instructions",
            PromptContributionSlot::EngagedContext => "engaged.context",
            PromptContributionSlot::AmbientInstructions => "ambient.instructions",
            PromptContributionSlot::OrientationContext => "orientation.context",
            PromptContributionSlot::ReflectionConsiderations => "reflection.considerations",
            PromptContributionSlot::PersonaEvolutionConsiderations => {
                "persona_evolution.considerations"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptContributionKind {
    Instruction,
    Context,
    Constraint,
}

impl PromptContributionKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            PromptContributionKind::Instruction => "instruction",
            PromptContributionKind::Context => "context",
            PromptContributionKind::Constraint => "constraint",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptContribution {
    pub plugin_id: String,
    pub slot: PromptContributionSlot,
    pub kind: PromptContributionKind,
    pub text: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_contribution_max_chars")]
    pub max_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptContributionContext {
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub loop_name: Option<String>,
    #[serde(default)]
    pub current_summary: Option<String>,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginPromptQuery {
    pub slot: PromptContributionSlot,
    #[serde(default)]
    pub context: PromptContributionContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginPromptResponse {
    #[serde(default)]
    pub contributions: Vec<PromptContribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RuntimePluginLifecycleEvent {
    PersonaEvolved {
        current_self_description: String,
        #[serde(default)]
        previous_self_description: Option<String>,
        #[serde(default)]
        trajectory: Option<String>,
        #[serde(default)]
        guiding_principles: Vec<String>,
    },
    OrientationUpdated {
        disposition: String,
        anomaly_count: usize,
        salience_count: usize,
    },
    MessageFinalized {
        conversation_id: String,
        role: String,
        content: String,
    },
    ReflectionCompleted {
        summary: String,
    },
    SettingsChanged {
        plugin_id: String,
        settings: Value,
    },
}

impl RuntimePluginLifecycleEvent {
    pub fn wire_name(&self) -> &'static str {
        match self {
            RuntimePluginLifecycleEvent::PersonaEvolved { .. } => "persona_evolved",
            RuntimePluginLifecycleEvent::OrientationUpdated { .. } => "orientation_updated",
            RuntimePluginLifecycleEvent::MessageFinalized { .. } => "message_finalized",
            RuntimePluginLifecycleEvent::ReflectionCompleted { .. } => "reflection_completed",
            RuntimePluginLifecycleEvent::SettingsChanged { .. } => "settings_changed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimePluginEventAck {
    #[serde(default)]
    pub state_changed: bool,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginEventEffect {
    pub plugin_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginToolInvocation {
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePluginToolResultKind {
    Text,
    Json,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginToolResult {
    pub kind: RuntimePluginToolResultKind,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Copy)]
pub struct PromptContributionMergeLimits {
    pub max_per_contribution_chars: usize,
    pub max_total_chars: usize,
}

impl Default for PromptContributionMergeLimits {
    fn default() -> Self {
        Self {
            max_per_contribution_chars: DEFAULT_MAX_CONTRIBUTION_CHARS,
            max_total_chars: DEFAULT_MAX_SLOT_TOTAL_CHARS,
        }
    }
}

struct RuntimePluginClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

struct LoadedRuntimePlugin {
    bundle: RuntimeProcessPluginBundle,
    handshake: RuntimePluginHandshake,
    client: Mutex<RuntimePluginClient>,
    registered_tools: RwLock<Vec<String>>,
}

pub struct RuntimePluginHost {
    catalog: SharedRuntimeProcessPluginCatalog,
    loaded: RwLock<HashMap<String, Arc<LoadedRuntimePlugin>>>,
    last_tool_registry: RwLock<Option<Arc<ToolRegistry>>>,
    request_counter: AtomicU64,
}

impl Default for RuntimePluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimePluginHost {
    pub fn new() -> Self {
        Self::with_catalog(Arc::new(Default::default()))
    }

    pub fn with_catalog(catalog: SharedRuntimeProcessPluginCatalog) -> Self {
        Self {
            catalog,
            loaded: RwLock::new(HashMap::new()),
            last_tool_registry: RwLock::new(None),
            request_counter: AtomicU64::new(1),
        }
    }

    pub async fn initialize(
        self: &Arc<Self>,
        config: &AgentConfig,
        tool_registry: Arc<ToolRegistry>,
    ) -> Result<Vec<BackendPluginManifest>> {
        self.apply_config(config, tool_registry).await?;
        Ok(self.manifests().await)
    }

    pub async fn manifests(&self) -> Vec<BackendPluginManifest> {
        let loaded = self.loaded.read().await;
        let mut manifests = self.catalog.manifests();
        for manifest in &mut manifests {
            if let Some(plugin) = loaded.get(&manifest.id) {
                manifest.provided_tools = plugin.handshake.tool_names();
            }
        }
        manifests
    }

    pub async fn apply_config(
        self: &Arc<Self>,
        config: &AgentConfig,
        tool_registry: Arc<ToolRegistry>,
    ) -> Result<()> {
        *self.last_tool_registry.write().await = Some(tool_registry.clone());

        let loaded_ids = self.loaded.read().await.keys().cloned().collect::<Vec<_>>();
        for plugin_id in loaded_ids {
            let should_keep = self
                .catalog
                .get(&plugin_id)
                .map(|bundle| bundle.is_enabled(config))
                .unwrap_or(false);
            if !should_keep {
                self.stop_plugin(&plugin_id, tool_registry.clone()).await;
            }
        }

        for plugin_id in self.catalog.plugin_ids() {
            let Some(bundle) = self.catalog.get(&plugin_id).cloned() else {
                continue;
            };
            if !bundle.is_enabled(config) {
                continue;
            }

            let existing = self.loaded.read().await.get(&plugin_id).cloned();
            if let Some(existing) = existing {
                if let Err(error) = self.configure_loaded_plugin(existing, config).await {
                    tracing::warn!(
                        "Failed to reconfigure runtime plugin '{}': {}",
                        plugin_id,
                        error
                    );
                    if Self::is_transport_error(&error) {
                        self.deactivate_failed_plugin(&plugin_id).await;
                    }
                }
                continue;
            }

            match self.start_plugin(bundle.clone(), config).await {
                Ok(loaded) => {
                    self.register_plugin_tools(&loaded, tool_registry.clone())
                        .await;
                    self.loaded.write().await.insert(plugin_id.clone(), loaded);
                }
                Err(error) => {
                    tracing::error!("Failed to start runtime plugin '{}': {}", plugin_id, error);
                }
            }
        }

        Ok(())
    }

    pub async fn dispatch_event(
        self: &Arc<Self>,
        event: &RuntimePluginLifecycleEvent,
    ) -> Result<Vec<RuntimePluginEventEffect>> {
        let plugins = self
            .loaded
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut effects = Vec::new();
        for plugin in plugins {
            if !plugin
                .handshake
                .capabilities
                .event_hooks
                .iter()
                .any(|hook| hook == event.wire_name())
            {
                continue;
            }

            let params = serde_json::to_value(event)?;
            let mut client = plugin.client.lock().await;
            match self
                .call_plugin::<RuntimePluginEventAck>(&mut client, "plugin.handle_event", params)
                .await
            {
                Ok(ack) => {
                    if let Some(summary) = ack.summary.filter(|summary| !summary.trim().is_empty())
                    {
                        effects.push(RuntimePluginEventEffect {
                            plugin_id: plugin.bundle.id().to_string(),
                            summary,
                        });
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "Runtime plugin '{}' failed to handle event '{}': {}",
                        plugin.bundle.id(),
                        event.wire_name(),
                        error
                    );
                    if Self::is_transport_error(&error) {
                        self.deactivate_failed_plugin(plugin.bundle.id()).await;
                    }
                }
            }
        }
        Ok(effects)
    }

    pub async fn collect_prompt_contributions(
        self: &Arc<Self>,
        query: &RuntimePluginPromptQuery,
    ) -> Result<Vec<PromptContribution>> {
        let plugins = self
            .loaded
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut contributions = Vec::new();
        for plugin in plugins {
            if !plugin
                .handshake
                .capabilities
                .prompt_slots
                .iter()
                .any(|slot| slot == query.slot.wire_name())
            {
                continue;
            }

            let mut client = plugin.client.lock().await;
            match self
                .call_plugin::<RuntimePluginPromptResponse>(
                    &mut client,
                    "plugin.get_prompt_contributions",
                    serde_json::to_value(query)?,
                )
                .await
            {
                Ok(response) => {
                    for mut contribution in response.contributions {
                        contribution.plugin_id = plugin.bundle.id().to_string();
                        if contribution.slot == query.slot && !contribution.text.trim().is_empty() {
                            contributions.push(contribution);
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "Runtime plugin '{}' failed to provide prompt contributions for '{}': {}",
                        plugin.bundle.id(),
                        query.slot.wire_name(),
                        error
                    );
                    if Self::is_transport_error(&error) {
                        self.deactivate_failed_plugin(plugin.bundle.id()).await;
                    }
                }
            }
        }
        Ok(contributions)
    }

    pub async fn invoke_tool(
        self: &Arc<Self>,
        plugin_id: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolOutput> {
        let plugin = self
            .loaded
            .read()
            .await
            .get(plugin_id)
            .cloned()
            .with_context(|| format!("Runtime plugin '{}' is not active", plugin_id))?;

        if !plugin
            .handshake
            .tools
            .iter()
            .any(|tool| tool.name == tool_name)
        {
            anyhow::bail!(
                "Runtime plugin '{}' does not expose tool '{}'",
                plugin_id,
                tool_name
            );
        }

        let mut client = plugin.client.lock().await;
        let call_result = self
            .call_plugin::<RuntimePluginToolResult>(
                &mut client,
                "plugin.invoke_tool",
                serde_json::to_value(RuntimePluginToolInvocation {
                    tool: tool_name.to_string(),
                    arguments,
                })?,
            )
            .await;
        drop(client);
        let result = match call_result {
            Ok(result) => result,
            Err(error) => {
                if Self::is_transport_error(&error) {
                    self.deactivate_failed_plugin(plugin_id).await;
                    anyhow::bail!(
                        "Runtime plugin '{}' became unavailable while running '{}': {}",
                        plugin_id,
                        tool_name,
                        error
                    );
                }
                return Err(error);
            }
        };
        Ok(convert_tool_result(result))
    }

    async fn start_plugin(
        &self,
        bundle: RuntimeProcessPluginBundle,
        config: &AgentConfig,
    ) -> Result<Arc<LoadedRuntimePlugin>> {
        let launch = bundle.launch_spec();
        let (command, args) = launch
            .command
            .split_first()
            .context("Runtime plugin command cannot be empty")?;

        let mut process = Command::new(command);
        process
            .args(args)
            .current_dir(&launch.working_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = process
            .spawn()
            .with_context(|| format!("Failed to launch runtime plugin '{}'", bundle.id()))?;
        let stdin = child
            .stdin
            .take()
            .context("Runtime plugin child is missing stdin pipe")?;
        let stdout = child
            .stdout
            .take()
            .context("Runtime plugin child is missing stdout pipe")?;
        let mut client = RuntimePluginClient {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        };

        let handshake: RuntimePluginHandshake = self
            .call_plugin(&mut client, "plugin.handshake", json!({}))
            .await?;
        if handshake.id != bundle.id() {
            anyhow::bail!(
                "Runtime plugin '{}' reported mismatched handshake id '{}'",
                bundle.id(),
                handshake.id
            );
        }

        self.configure_client(&mut client, &bundle, config).await?;

        Ok(Arc::new(LoadedRuntimePlugin {
            bundle,
            handshake,
            client: Mutex::new(client),
            registered_tools: RwLock::new(Vec::new()),
        }))
    }

    async fn configure_loaded_plugin(
        &self,
        plugin: Arc<LoadedRuntimePlugin>,
        config: &AgentConfig,
    ) -> Result<()> {
        let mut client = plugin.client.lock().await;
        self.configure_client(&mut client, &plugin.bundle, config)
            .await
    }

    async fn configure_client(
        &self,
        client: &mut RuntimePluginClient,
        bundle: &RuntimeProcessPluginBundle,
        config: &AgentConfig,
    ) -> Result<()> {
        let settings = config
            .plugin_settings
            .get(bundle.id())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let _: Value = self
            .call_plugin(
                client,
                "plugin.configure",
                json!({ "settings": settings.clone() }),
            )
            .await?;
        let _: RuntimePluginEventAck = self
            .call_plugin(
                client,
                "plugin.handle_event",
                serde_json::to_value(RuntimePluginLifecycleEvent::SettingsChanged {
                    plugin_id: bundle.id().to_string(),
                    settings,
                })?,
            )
            .await
            .unwrap_or_default();
        Ok(())
    }

    async fn register_plugin_tools(
        self: &Arc<Self>,
        plugin: &Arc<LoadedRuntimePlugin>,
        tool_registry: Arc<ToolRegistry>,
    ) {
        let mut registered = Vec::new();
        for tool in &plugin.handshake.tools {
            if tool_registry.get(&tool.name).await.is_some() {
                tracing::warn!(
                    "Skipping runtime plugin tool '{}' from '{}' because the name is already registered",
                    tool.name,
                    plugin.bundle.id()
                );
                continue;
            }
            tool_registry
                .register(Arc::new(RuntimePluginToolProxy::new(
                    plugin.bundle.id().to_string(),
                    tool.clone(),
                    self.clone(),
                )))
                .await;
            registered.push(tool.name.clone());
        }
        *plugin.registered_tools.write().await = registered;
    }

    async fn stop_plugin(&self, plugin_id: &str, tool_registry: Arc<ToolRegistry>) {
        let removed = self.loaded.write().await.remove(plugin_id);
        let Some(plugin) = removed else {
            return;
        };

        let registered_tools = plugin.registered_tools.read().await.clone();
        for tool_name in registered_tools {
            let _ = tool_registry.deregister(&tool_name).await;
        }

        let mut client = plugin.client.lock().await;
        if let Err(error) = client.child.kill().await {
            tracing::debug!(
                "Runtime plugin '{}' child already stopped or could not be killed: {}",
                plugin_id,
                error
            );
        }
    }

    async fn deactivate_failed_plugin(&self, plugin_id: &str) {
        let tool_registry = self.last_tool_registry.read().await.clone();
        if let Some(tool_registry) = tool_registry {
            self.stop_plugin(plugin_id, tool_registry).await;
            return;
        }

        let removed = self.loaded.write().await.remove(plugin_id);
        let Some(plugin) = removed else {
            return;
        };

        let mut client = plugin.client.lock().await;
        let _ = client.child.kill().await;
    }

    async fn call_plugin<T: DeserializeOwned>(
        &self,
        client: &mut RuntimePluginClient,
        method: &str,
        params: Value,
    ) -> Result<T> {
        let raw = self.call_plugin_raw(client, method, params).await?;
        serde_json::from_value(raw)
            .with_context(|| format!("Failed to parse runtime plugin response for '{}'", method))
    }

    async fn call_plugin_raw(
        &self,
        client: &mut RuntimePluginClient,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let request = RuntimePluginRpcRequest {
            id: format!(
                "req_{}",
                self.request_counter.fetch_add(1, Ordering::SeqCst)
            ),
            method: method.to_string(),
            params,
        };

        let line = serde_json::to_string(&request)?;
        if let Err(error) = client.stdin.write_all(line.as_bytes()).await {
            if let Ok(Some(status)) = client.child.try_wait() {
                anyhow::bail!(
                    "Runtime plugin process exited ({}) while sending '{}': {}",
                    status,
                    method,
                    error
                );
            }
            return Err(error).with_context(|| {
                format!("Failed to write runtime plugin request for '{}'", method)
            });
        }
        if let Err(error) = client.stdin.write_all(b"\n").await {
            if let Ok(Some(status)) = client.child.try_wait() {
                anyhow::bail!(
                    "Runtime plugin process exited ({}) while sending '{}': {}",
                    status,
                    method,
                    error
                );
            }
            return Err(error).with_context(|| {
                format!("Failed to write runtime plugin request for '{}'", method)
            });
        }
        client
            .stdin
            .flush()
            .await
            .with_context(|| format!("Failed to flush runtime plugin request for '{}'", method))?;

        let mut ignored_non_json = 0usize;
        let response: RuntimePluginRpcResponse = loop {
            let mut response_line = String::new();
            let bytes_read = client
                .stdout
                .read_line(&mut response_line)
                .await
                .with_context(|| {
                    format!("Failed to read runtime plugin response for '{}'", method)
                })?;
            if bytes_read == 0 {
                if let Ok(Some(status)) = client.child.try_wait() {
                    anyhow::bail!(
                        "Runtime plugin process exited ({}) while waiting for '{}'",
                        status,
                        method
                    );
                }
                anyhow::bail!(
                    "Runtime plugin process closed stdout while waiting for '{}'",
                    method
                );
            }

            let trimmed = response_line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<RuntimePluginRpcResponse>(trimmed) {
                Ok(response) => break response,
                Err(error) => {
                    ignored_non_json += 1;
                    if ignored_non_json > MAX_NON_JSON_PLUGIN_LINES {
                        return Err(error).with_context(|| {
                            format!("Failed to parse runtime plugin JSON for '{}'", method)
                        });
                    }
                    tracing::warn!(
                        "Ignoring non-JSON stdout from runtime plugin during '{}': {}",
                        method,
                        truncate_chars(trimmed, 240)
                    );
                }
            }
        };
        if response.id != request.id {
            anyhow::bail!(
                "Runtime plugin response id mismatch for '{}': expected '{}', got '{}'",
                method,
                request.id,
                response.id
            );
        }
        if !response.ok {
            let error = response.error.unwrap_or(RuntimePluginRpcError {
                code: "plugin_error".to_string(),
                message: "runtime plugin call failed".to_string(),
            });
            anyhow::bail!("{}: {}", error.code, error.message);
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    fn is_transport_error(error: &anyhow::Error) -> bool {
        let message = format!("{error:#}").to_lowercase();
        message.contains("broken pipe")
            || message.contains("closed stdout")
            || message.contains("failed to write runtime plugin request")
            || message.contains("failed to flush runtime plugin request")
            || message.contains("failed to read runtime plugin response")
            || message.contains("process exited")
            || message.contains("connection reset by peer")
    }
}

pub fn render_prompt_slot_addendum(
    slot: PromptContributionSlot,
    contributions: &[PromptContribution],
    limits: PromptContributionMergeLimits,
) -> Option<String> {
    let mut relevant = contributions
        .iter()
        .filter(|item| item.slot == slot)
        .filter_map(|item| {
            let trimmed = item.text.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(item)
        })
        .collect::<Vec<_>>();
    if relevant.is_empty() {
        return None;
    }

    relevant.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.plugin_id.cmp(&right.plugin_id))
            .then_with(|| left.kind.wire_name().cmp(right.kind.wire_name()))
    });

    let mut rendered = String::new();
    let mut remaining = limits.max_total_chars;
    for item in relevant {
        if remaining == 0 {
            break;
        }

        let local_limit = item.max_chars.min(limits.max_per_contribution_chars);
        let text = truncate_chars(item.text.trim(), local_limit.min(remaining));
        if text.is_empty() {
            continue;
        }

        let block = format!(
            "[Plugin: {} | {}]\n{}",
            item.plugin_id,
            item.kind.wire_name(),
            text
        );
        let block_len = block.chars().count();
        if block_len > remaining {
            let overhead = format!("[Plugin: {} | {}]\n", item.plugin_id, item.kind.wire_name())
                .chars()
                .count();
            if overhead >= remaining {
                break;
            }
            let available = remaining - overhead;
            let truncated = truncate_chars(&text, available);
            if truncated.is_empty() {
                break;
            }
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str(&format!(
                "[Plugin: {} | {}]\n{}",
                item.plugin_id,
                item.kind.wire_name(),
                truncated
            ));
            break;
        }

        if !rendered.is_empty() {
            rendered.push_str("\n\n");
            remaining = remaining.saturating_sub(2);
        }
        rendered.push_str(&block);
        remaining = remaining.saturating_sub(block_len);
    }

    let trimmed = rendered.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn default_contribution_max_chars() -> usize {
    240
}

fn default_tool_parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {}
    })
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut count = 0usize;
    for ch in input.chars() {
        if count >= max_chars {
            break;
        }
        out.push(ch);
        count += 1;
    }
    out
}

fn convert_tool_result(result: RuntimePluginToolResult) -> ToolOutput {
    match result.kind {
        RuntimePluginToolResultKind::Text => ToolOutput::Text(result.text.unwrap_or_default()),
        RuntimePluginToolResultKind::Json => ToolOutput::Json(result.data.unwrap_or(Value::Null)),
        RuntimePluginToolResultKind::Error => ToolOutput::Error(
            result
                .text
                .or_else(|| result.data.map(|value| value.to_string()))
                .unwrap_or_else(|| "Runtime plugin tool failed".to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_prompt_slot_addendum_sorts_and_labels_blocks() {
        let rendered = render_prompt_slot_addendum(
            PromptContributionSlot::EngagedInstructions,
            &[
                PromptContribution {
                    plugin_id: "zeta".to_string(),
                    slot: PromptContributionSlot::EngagedInstructions,
                    kind: PromptContributionKind::Instruction,
                    text: "second".to_string(),
                    priority: 10,
                    max_chars: 240,
                },
                PromptContribution {
                    plugin_id: "alpha".to_string(),
                    slot: PromptContributionSlot::EngagedInstructions,
                    kind: PromptContributionKind::Context,
                    text: "first".to_string(),
                    priority: 5,
                    max_chars: 240,
                },
            ],
            PromptContributionMergeLimits::default(),
        )
        .expect("rendered addendum");

        let first_index = rendered.find("alpha").expect("alpha block");
        let second_index = rendered.find("zeta").expect("zeta block");
        assert!(first_index < second_index);
        assert!(rendered.contains("[Plugin: alpha | context]"));
        assert!(rendered.contains("[Plugin: zeta | instruction]"));
    }

    #[test]
    fn render_prompt_slot_addendum_respects_slot_and_total_budget() {
        let rendered = render_prompt_slot_addendum(
            PromptContributionSlot::EngagedContext,
            &[
                PromptContribution {
                    plugin_id: "alpha".to_string(),
                    slot: PromptContributionSlot::EngagedContext,
                    kind: PromptContributionKind::Context,
                    text: "A".repeat(100),
                    priority: 0,
                    max_chars: 100,
                },
                PromptContribution {
                    plugin_id: "beta".to_string(),
                    slot: PromptContributionSlot::EngagedInstructions,
                    kind: PromptContributionKind::Instruction,
                    text: "ignored".to_string(),
                    priority: 0,
                    max_chars: 100,
                },
            ],
            PromptContributionMergeLimits {
                max_per_contribution_chars: 40,
                max_total_chars: 50,
            },
        )
        .expect("rendered addendum");

        assert!(rendered.contains("[Plugin: alpha | context]"));
        assert!(!rendered.contains("ignored"));
        assert!(rendered.chars().count() <= 52);
    }

    #[test]
    fn convert_tool_result_maps_error_payloads() {
        let output = convert_tool_result(RuntimePluginToolResult {
            kind: RuntimePluginToolResultKind::Error,
            text: Some("boom".to_string()),
            data: None,
        });

        assert!(matches!(output, ToolOutput::Error(message) if message == "boom"));
    }
}
