use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock};

use crate::config::AgentConfig;
use crate::database::{AgentDatabase, PluginEventRecord, PluginEventRetentionPolicy};
use crate::plugin_contract::{
    is_supported_plugin_protocol_version, PluginHostDescriptor, PluginManifest, PluginRuntimeState,
    PluginRuntimeStatus, PluginStateMutation, PluginStateValue, RuntimePluginConfigureRequest,
    RuntimePluginConfigureResponse, RuntimePluginHandshakeRequest,
    RuntimePluginToolInvocationContext, CURRENT_PLUGIN_PROTOCOL_VERSION,
    SUPPORTED_PLUGIN_PROTOCOL_VERSIONS,
};
use crate::plugin_event_ledger::{PluginEventLedger, PluginSkillEventBatch};
use crate::plugin_lifecycle::{
    PluginDesiredState, PluginLifecycleAction, PluginLifecycleMachine, PluginLifecycleSnapshot,
    PluginOperationalState,
};
use crate::plugin_restart_policy::PluginRestartPolicy;
use crate::runtime_process_plugin::{
    RuntimeProcessCatalogRefresh, RuntimeProcessPluginBundle, SharedRuntimeProcessPluginCatalog,
};
use crate::tools::runtime_plugin::RuntimePluginToolProxy;
use crate::tools::{ToolCategory, ToolContext, ToolOutput, ToolRegistry};

pub use crate::plugin_contract::{
    PromptContribution, PromptContributionContext, PromptContributionKind, PromptContributionSlot,
    RuntimePluginCapabilities, RuntimePluginEventAck, RuntimePluginEventEffect,
    RuntimePluginHandshake, RuntimePluginLifecycleEvent, RuntimePluginPollEvent,
    RuntimePluginPollResponse, RuntimePluginPromptQuery, RuntimePluginPromptResponse,
    RuntimePluginRpcError, RuntimePluginRpcRequest, RuntimePluginRpcResponse,
    RuntimePluginToolCategory, RuntimePluginToolInvocation, RuntimePluginToolManifest,
    RuntimePluginToolResult, RuntimePluginToolResultKind,
};

const DEFAULT_MAX_CONTRIBUTION_CHARS: usize = 300;
const DEFAULT_MAX_SLOT_TOTAL_CHARS: usize = 1_200;
const MAX_NON_JSON_PLUGIN_LINES: usize = 256;
const DEFAULT_RUNTIME_PLUGIN_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const RUNTIME_PLUGIN_PROMPT_RPC_TIMEOUT: Duration = Duration::from_millis(250);
const RUNTIME_PLUGIN_TOOL_RPC_TIMEOUT: Duration = Duration::from_secs(300);

impl RuntimePluginToolCategory {
    pub fn as_tool_category(&self) -> ToolCategory {
        match self {
            Self::FileSystem => ToolCategory::FileSystem,
            Self::Shell => ToolCategory::Shell,
            Self::Network => ToolCategory::Network,
            Self::Memory => ToolCategory::Memory,
            Self::General => ToolCategory::General,
        }
    }
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
    generation: u64,
    process_id: Option<u32>,
    client: Mutex<RuntimePluginClient>,
    registered_tools: RwLock<Vec<String>>,
    tool_registration_complete: AtomicBool,
    configured_settings: RwLock<Value>,
}

enum RuntimePluginStartFailure {
    Retryable(anyhow::Error),
    Terminal(anyhow::Error),
}

impl RuntimePluginStartFailure {
    fn error(&self) -> &anyhow::Error {
        match self {
            Self::Retryable(error) | Self::Terminal(error) => error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePluginConfigureOutcome {
    Unchanged,
    Applied,
}

pub struct RuntimePluginHost {
    catalog: SharedRuntimeProcessPluginCatalog,
    database: Option<Arc<AgentDatabase>>,
    event_ledger: Option<PluginEventLedger>,
    loaded: RwLock<HashMap<String, Arc<LoadedRuntimePlugin>>>,
    lifecycles: RwLock<HashMap<String, PluginLifecycleMachine>>,
    last_settings: RwLock<HashMap<String, Value>>,
    last_tool_registry: RwLock<Option<Arc<ToolRegistry>>>,
    restart_policy: PluginRestartPolicy,
    reconcile_lock: Mutex<()>,
    last_event_compaction: RwLock<Option<chrono::DateTime<Utc>>>,
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
        Self::with_catalog_database_and_restart_policy(
            catalog,
            None,
            PluginRestartPolicy::default(),
        )
    }

    pub fn with_catalog_and_database(
        catalog: SharedRuntimeProcessPluginCatalog,
        database: Option<Arc<AgentDatabase>>,
    ) -> Self {
        Self::with_catalog_database_and_restart_policy(
            catalog,
            database,
            PluginRestartPolicy::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_catalog_and_restart_policy(
        catalog: SharedRuntimeProcessPluginCatalog,
        restart_policy: PluginRestartPolicy,
    ) -> Self {
        Self::with_catalog_database_and_restart_policy(catalog, None, restart_policy)
    }

    fn with_catalog_database_and_restart_policy(
        catalog: SharedRuntimeProcessPluginCatalog,
        database: Option<Arc<AgentDatabase>>,
        restart_policy: PluginRestartPolicy,
    ) -> Self {
        let now = Utc::now();
        let lifecycles = catalog
            .plugin_ids()
            .into_iter()
            .map(|plugin_id| {
                let lifecycle = PluginLifecycleMachine::new(
                    plugin_id.clone(),
                    true,
                    PluginDesiredState::Disabled,
                    now,
                );
                (plugin_id, lifecycle)
            })
            .collect();
        Self {
            catalog,
            event_ledger: database.clone().map(PluginEventLedger::new),
            database,
            loaded: RwLock::new(HashMap::new()),
            lifecycles: RwLock::new(lifecycles),
            last_settings: RwLock::new(HashMap::new()),
            last_tool_registry: RwLock::new(None),
            restart_policy,
            reconcile_lock: Mutex::new(()),
            last_event_compaction: RwLock::new(None),
            request_counter: AtomicU64::new(1),
        }
    }

    pub async fn initialize(
        self: &Arc<Self>,
        config: &AgentConfig,
        tool_registry: Arc<ToolRegistry>,
    ) -> Result<Vec<PluginManifest>> {
        self.apply_config(config, tool_registry).await?;
        Ok(self.manifests().await)
    }

    pub async fn manifests(&self) -> Vec<PluginManifest> {
        let loaded = self.loaded.read().await;
        let mut manifests = self.catalog.manifests();
        for manifest in &mut manifests {
            if let Some(plugin) = loaded.get(&manifest.id) {
                manifest.provided_tools = plugin.handshake.tool_names();
                manifest.tools = plugin.handshake.tools.clone();
            }
        }
        manifests
    }

    pub async fn statuses(&self) -> Vec<PluginRuntimeStatus> {
        let runtime_metadata = self
            .loaded
            .read()
            .await
            .iter()
            .map(|(plugin_id, plugin)| {
                (
                    plugin_id.clone(),
                    (plugin.handshake.protocol_version, plugin.process_id),
                )
            })
            .collect::<HashMap<_, _>>();
        let lifecycles = self.lifecycles.read().await;
        let mut statuses = lifecycles
            .values()
            .map(|lifecycle| {
                let snapshot = lifecycle.snapshot();
                let metadata = runtime_metadata.get(&snapshot.plugin_id).copied();
                runtime_status_from_snapshot(snapshot, metadata)
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        statuses
    }

    fn state_snapshot(&self, plugin_id: &str) -> Result<HashMap<String, PluginStateValue>> {
        let Some(database) = &self.database else {
            return Ok(HashMap::new());
        };
        let state = database
            .list_plugin_state(plugin_id)?
            .into_iter()
            .map(|record| {
                (
                    record.key,
                    PluginStateValue {
                        schema_version: record.schema_version,
                        value: record.value,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        Ok(state)
    }

    fn apply_state_updates(&self, plugin_id: &str, updates: &[PluginStateMutation]) -> Result<()> {
        let Some(database) = &self.database else {
            if !updates.is_empty() {
                tracing::warn!(
                    plugin_id,
                    update_count = updates.len(),
                    "Discarding plugin state updates because durable storage is unavailable"
                );
            }
            return Ok(());
        };
        database.apply_plugin_state_mutations(plugin_id, updates)
    }

    pub async fn compact_event_ledger_if_due(&self, now: chrono::DateTime<Utc>) -> Result<()> {
        let compaction_interval = chrono::Duration::hours(1);
        {
            let last = self.last_event_compaction.read().await;
            if last.is_some_and(|last| now.signed_duration_since(last) < compaction_interval) {
                return Ok(());
            }
        }
        let mut last = self.last_event_compaction.write().await;
        if last.is_some_and(|last| now.signed_duration_since(last) < compaction_interval) {
            return Ok(());
        }
        *last = Some(now);
        let Some(database) = &self.database else {
            return Ok(());
        };
        let report = database.compact_plugin_events(PluginEventRetentionPolicy::default(), now)?;
        if report.acknowledged_events_deleted > 0
            || report.expired_unconsumed_events_deleted > 0
            || report.dead_letters_deleted > 0
        {
            tracing::info!(
                acknowledged_events_deleted = report.acknowledged_events_deleted,
                expired_unconsumed_events_deleted = report.expired_unconsumed_events_deleted,
                dead_letters_deleted = report.dead_letters_deleted,
                "Compacted durable plugin event ledger"
            );
        }
        Ok(())
    }

    pub async fn apply_config(
        self: &Arc<Self>,
        config: &AgentConfig,
        tool_registry: Arc<ToolRegistry>,
    ) -> Result<()> {
        let _reconcile_guard = self.reconcile_lock.lock().await;
        *self.last_tool_registry.write().await = Some(tool_registry.clone());
        let refresh = self.catalog.refresh()?;
        if !refresh.is_empty() {
            tracing::info!(
                added = ?refresh.added,
                updated = ?refresh.updated,
                removed = ?refresh.removed,
                "Runtime plugin catalog refreshed"
            );
        }
        self.sync_lifecycle_inputs(config).await;
        self.detect_exited_plugins().await;
        self.repair_interrupted_transitions().await;
        self.reset_updated_bundles(&refresh, tool_registry.clone())
            .await;
        self.drive_lifecycles(config, tool_registry.clone()).await;
        self.reconfigure_running_plugins(config, tool_registry)
            .await;
        Ok(())
    }

    async fn sync_lifecycle_inputs(&self, config: &AgentConfig) {
        let now = Utc::now();
        let catalog_ids = self.catalog.plugin_ids();
        let available_ids = catalog_ids.iter().cloned().collect::<HashSet<_>>();
        let mut desired = HashMap::new();
        for plugin_id in &catalog_ids {
            if let Some(bundle) = self.catalog.get(plugin_id) {
                desired.insert(
                    plugin_id.clone(),
                    (
                        if bundle.is_enabled(config) {
                            PluginDesiredState::Enabled
                        } else {
                            PluginDesiredState::Disabled
                        },
                        plugin_settings_value(config, plugin_id),
                    ),
                );
            }
        }

        let changed_settings = {
            let mut previous = self.last_settings.write().await;
            let mut changed = HashSet::new();
            for (plugin_id, (_, settings)) in &desired {
                if previous.get(plugin_id).is_some_and(|old| old != settings) {
                    changed.insert(plugin_id.clone());
                }
                previous.insert(plugin_id.clone(), settings.clone());
            }
            changed
        };

        let mut lifecycles = self.lifecycles.write().await;
        for (plugin_id, lifecycle) in lifecycles.iter_mut() {
            lifecycle.set_available(available_ids.contains(plugin_id));
        }
        for plugin_id in catalog_ids {
            let Some((desired_state, _)) = desired.get(&plugin_id) else {
                continue;
            };
            let lifecycle = lifecycles.entry(plugin_id.clone()).or_insert_with(|| {
                PluginLifecycleMachine::new(plugin_id.clone(), true, *desired_state, now)
            });
            lifecycle.set_available(true);
            lifecycle.set_desired_state(*desired_state);
            if changed_settings.contains(&plugin_id) {
                lifecycle.reset_recovery_after_input_change(now);
            }
        }
    }

    async fn reset_updated_bundles(
        &self,
        refresh: &RuntimeProcessCatalogRefresh,
        tool_registry: Arc<ToolRegistry>,
    ) {
        for plugin_id in &refresh.updated {
            let action = {
                let mut lifecycles = self.lifecycles.write().await;
                let Some(lifecycle) = lifecycles.get_mut(plugin_id) else {
                    continue;
                };
                lifecycle.set_available(false);
                lifecycle.reset_recovery_after_input_change(Utc::now());
                lifecycle.reconcile(Utc::now(), &self.restart_policy)
            };

            if let Some(PluginLifecycleAction::Stop { generation }) = action {
                self.stop_plugin(plugin_id, tool_registry.clone()).await;
                let mut lifecycles = self.lifecycles.write().await;
                if let Some(lifecycle) = lifecycles.get_mut(plugin_id) {
                    if let Err(error) = lifecycle.mark_stopped(generation, Utc::now()) {
                        tracing::debug!(
                            "Runtime plugin '{}' replacement stop raced with another outcome: {}",
                            plugin_id,
                            error
                        );
                    }
                }
            }

            let mut lifecycles = self.lifecycles.write().await;
            if let Some(lifecycle) = lifecycles.get_mut(plugin_id) {
                lifecycle.set_available(true);
                lifecycle.reset_recovery_after_input_change(Utc::now());
            }
        }
    }

    async fn drive_lifecycles(
        self: &Arc<Self>,
        config: &AgentConfig,
        tool_registry: Arc<ToolRegistry>,
    ) {
        let mut plugin_ids = self
            .lifecycles
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        plugin_ids.sort();

        for plugin_id in plugin_ids {
            let action = {
                let mut lifecycles = self.lifecycles.write().await;
                lifecycles
                    .get_mut(&plugin_id)
                    .and_then(|lifecycle| lifecycle.reconcile(Utc::now(), &self.restart_policy))
            };
            if let Some(action) = action {
                self.execute_lifecycle_action(&plugin_id, action, config, tool_registry.clone())
                    .await;
            }
        }
    }

    async fn execute_lifecycle_action(
        self: &Arc<Self>,
        plugin_id: &str,
        action: PluginLifecycleAction,
        config: &AgentConfig,
        tool_registry: Arc<ToolRegistry>,
    ) {
        match action {
            PluginLifecycleAction::Stop { generation } => {
                self.stop_plugin(plugin_id, tool_registry).await;
                let mut lifecycles = self.lifecycles.write().await;
                if let Some(lifecycle) = lifecycles.get_mut(plugin_id) {
                    if let Err(error) = lifecycle.mark_stopped(generation, Utc::now()) {
                        tracing::debug!(
                            "Runtime plugin '{}' stop completion was stale: {}",
                            plugin_id,
                            error
                        );
                    }
                }
            }
            PluginLifecycleAction::Start { generation, reason } => {
                let Some(bundle) = self.catalog.get(plugin_id) else {
                    let mut lifecycles = self.lifecycles.write().await;
                    if let Some(lifecycle) = lifecycles.get_mut(plugin_id) {
                        lifecycle.set_available(false);
                        let _ = lifecycle.mark_failed(
                            generation,
                            "plugin package disappeared before start",
                            Utc::now(),
                            &self.restart_policy,
                        );
                    }
                    return;
                };
                let settings = plugin_settings_value(config, plugin_id);
                tracing::info!(plugin_id, generation, ?reason, "Starting runtime plugin");
                match self.start_plugin(bundle, settings, generation).await {
                    Ok(loaded) => {
                        let marked_running = {
                            let mut lifecycles = self.lifecycles.write().await;
                            lifecycles
                                .get_mut(plugin_id)
                                .map(|lifecycle| lifecycle.mark_running(generation, Utc::now()))
                        };
                        if !matches!(marked_running, Some(Ok(()))) {
                            let mut client = loaded.client.lock().await;
                            let _ = client.child.kill().await;
                            tracing::warn!(
                                "Discarded stale runtime plugin '{}' generation {} startup",
                                plugin_id,
                                generation
                            );
                            return;
                        }
                        self.loaded
                            .write()
                            .await
                            .insert(plugin_id.to_string(), loaded.clone());
                        self.register_plugin_tools(&loaded, tool_registry).await;
                    }
                    Err(failure) => {
                        let message = format!("{:#}", failure.error());
                        tracing::error!(
                            plugin_id,
                            generation,
                            error = %message,
                            "Failed to start runtime plugin"
                        );
                        let mut lifecycles = self.lifecycles.write().await;
                        let Some(lifecycle) = lifecycles.get_mut(plugin_id) else {
                            return;
                        };
                        let transition = match failure {
                            RuntimePluginStartFailure::Retryable(_) => lifecycle.mark_failed(
                                generation,
                                message,
                                Utc::now(),
                                &self.restart_policy,
                            ),
                            RuntimePluginStartFailure::Terminal(_) => {
                                lifecycle.mark_terminal_failure(generation, message, Utc::now())
                            }
                        };
                        if let Err(error) = transition {
                            tracing::debug!(
                                "Runtime plugin '{}' start failure was stale: {}",
                                plugin_id,
                                error
                            );
                        }
                    }
                }
            }
        }
    }

    async fn reconfigure_running_plugins(
        self: &Arc<Self>,
        config: &AgentConfig,
        tool_registry: Arc<ToolRegistry>,
    ) {
        let plugins = self
            .loaded
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for plugin in plugins {
            self.register_plugin_tools(&plugin, tool_registry.clone())
                .await;
            let plugin_id = plugin.bundle.id().to_string();
            let settings = plugin_settings_value(config, &plugin_id);
            match self.configure_loaded_plugin(plugin.clone(), settings).await {
                Ok(RuntimePluginConfigureOutcome::Unchanged) => {}
                Ok(RuntimePluginConfigureOutcome::Applied) => {
                    let mut lifecycles = self.lifecycles.write().await;
                    if let Some(lifecycle) = lifecycles.get_mut(&plugin_id) {
                        let _ = lifecycle.mark_healthy(
                            plugin.generation,
                            Utc::now(),
                            &self.restart_policy,
                        );
                    }
                }
                Err(error) if Self::is_transport_error(&error) => {
                    tracing::warn!(
                        "Runtime plugin '{}' reconfiguration lost transport: {}",
                        plugin_id,
                        error
                    );
                    self.deactivate_failed_plugin(&plugin_id, &error).await;
                }
                Err(error) => {
                    tracing::warn!(
                        "Runtime plugin '{}' rejected changed settings: {}",
                        plugin_id,
                        error
                    );
                    // A response may have changed process-local SDK state
                    // before host-owned state was durably accepted. Stop this
                    // generation so supervised recovery reconfigures it from
                    // the last durable snapshot.
                    self.deactivate_failed_plugin(&plugin_id, &error).await;
                }
            }
        }
    }

    async fn detect_exited_plugins(&self) {
        let plugins = self
            .loaded
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for plugin in plugins {
            let exit_error = match plugin.client.try_lock() {
                Ok(mut client) => match client.child.try_wait() {
                    Ok(Some(status)) => Some(anyhow::anyhow!(
                        "runtime plugin process exited unexpectedly ({status})"
                    )),
                    Ok(None) => None,
                    Err(error) => Some(anyhow::anyhow!(
                        "failed to inspect runtime plugin process: {error}"
                    )),
                },
                Err(_) => None,
            };
            if let Some(error) = exit_error {
                self.deactivate_failed_plugin(plugin.bundle.id(), &error)
                    .await;
            }
        }
    }

    async fn repair_interrupted_transitions(&self) {
        let loaded_ids = self
            .loaded
            .read()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let interrupted = self
            .lifecycles
            .read()
            .await
            .iter()
            .filter_map(|(plugin_id, lifecycle)| {
                let snapshot = lifecycle.snapshot();
                if loaded_ids.contains(plugin_id) {
                    return None;
                }
                match snapshot.state {
                    PluginOperationalState::Starting
                    | PluginOperationalState::Running
                    | PluginOperationalState::Degraded
                    | PluginOperationalState::Stopping => {
                        Some((plugin_id.clone(), snapshot.generation, snapshot.state))
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>();

        if interrupted.is_empty() {
            return;
        }
        let mut lifecycles = self.lifecycles.write().await;
        for (plugin_id, generation, state) in interrupted {
            let Some(lifecycle) = lifecycles.get_mut(&plugin_id) else {
                continue;
            };
            let result = if state == PluginOperationalState::Stopping {
                lifecycle.mark_stopped(generation, Utc::now())
            } else {
                lifecycle.mark_failed(
                    generation,
                    "plugin process registration was interrupted",
                    Utc::now(),
                    &self.restart_policy,
                )
            };
            if let Err(error) = result {
                tracing::debug!(
                    "Runtime plugin '{}' interrupted transition was already resolved: {}",
                    plugin_id,
                    error
                );
            }
        }
    }

    pub async fn dispatch_event(
        self: &Arc<Self>,
        event: &RuntimePluginLifecycleEvent,
    ) -> Result<Vec<RuntimePluginEventEffect>> {
        if let RuntimePluginLifecycleEvent::SettingsChanged { plugin_id, .. } = event {
            let plugin = self.loaded.read().await.get(plugin_id).cloned();
            let Some(plugin) = plugin else {
                return Ok(Vec::new());
            };
            if !plugin_declares_event_hook(
                plugin.bundle.manifest(),
                &plugin.handshake,
                "settings_changed",
            ) {
                return Ok(Vec::new());
            }

            let call_result = {
                let mut client = plugin.client.lock().await;
                self.call_lifecycle_event(&mut client, plugin_id, event, None)
                    .await
            };
            let call_result = match call_result {
                Ok((summary, state_updates)) => self
                    .apply_state_updates(plugin_id, &state_updates)
                    .map(|_| summary),
                Err(error) => Err(error),
            };
            return match call_result {
                Ok(Some(summary)) => Ok(vec![RuntimePluginEventEffect {
                    plugin_id: plugin_id.clone(),
                    summary,
                }]),
                Ok(None) => Ok(Vec::new()),
                Err(error) => {
                    tracing::warn!(
                        "Runtime plugin '{}' failed to handle its scoped settings_changed event: {}",
                        plugin_id,
                        error
                    );
                    self.deactivate_failed_plugin(plugin_id, &error).await;
                    Ok(Vec::new())
                }
            };
        }

        let ledger_record = self
            .event_ledger
            .as_ref()
            .map(|ledger| ledger.record_lifecycle_event(event))
            .transpose()?;
        let plugins = self
            .loaded
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let subscription = ledger_record
            .as_ref()
            .map(|record| record.event_type.clone())
            .unwrap_or_else(|| format!("host.lifecycle.{}", event.wire_name()));
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

            let call_result = {
                let mut client = plugin.client.lock().await;
                self.deliver_lifecycle_subscription(
                    &mut client,
                    plugin.bundle.id(),
                    &subscription,
                    Some(event),
                )
                .await
            };
            match call_result {
                Ok(summaries) => {
                    for summary in summaries {
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
                    // Keep the durable receipt pending and use supervised
                    // restart/backoff as the protocol-v1 redelivery schedule.
                    self.deactivate_failed_plugin(plugin.bundle.id(), &error)
                        .await;
                }
            }
        }
        Ok(effects)
    }

    async fn deliver_lifecycle_subscription(
        &self,
        client: &mut RuntimePluginClient,
        plugin_id: &str,
        subscription: &str,
        fallback_event: Option<&RuntimePluginLifecycleEvent>,
    ) -> Result<Vec<String>> {
        let mut summaries = Vec::new();
        if let Some(database) = &self.database {
            // Keep the durable commit boundary aligned with one callback. If a
            // later event fails, earlier event state and receipts remain
            // committed instead of replaying an already accepted batch.
            for _ in 0..1_000 {
                let delivery =
                    database.prepare_plugin_event_delivery(plugin_id, subscription, 1)?;
                let Some(receipt) = delivery.receipt else {
                    break;
                };
                let mut pending_state_updates = Vec::new();
                for record in delivery.records {
                    if record.schema_version != 1 {
                        database.quarantine_plugin_event(
                            record.sequence,
                            &format!(
                                "unsupported lifecycle event schema {}; host supports version 1",
                                record.schema_version
                            ),
                        )?;
                        continue;
                    }
                    let event = match serde_json::from_value::<RuntimePluginLifecycleEvent>(
                        record.payload.clone(),
                    ) {
                        Ok(event) => event,
                        Err(error) => {
                            database.quarantine_plugin_event(
                                record.sequence,
                                &format!("lifecycle event decode failed: {error}"),
                            )?;
                            continue;
                        }
                    };
                    let (summary, state_updates) = self
                        .call_lifecycle_event(client, plugin_id, &event, Some(&record))
                        .await?;
                    if let Some(summary) = summary {
                        summaries.push(summary);
                    }
                    pending_state_updates.extend(state_updates);
                }
                database.acknowledge_plugin_event_delivery_with_state(
                    plugin_id,
                    subscription,
                    &receipt.delivery_token,
                    receipt.through_sequence,
                    &pending_state_updates,
                )?;
            }
        } else if let Some(event) = fallback_event {
            let (summary, state_updates) = self
                .call_lifecycle_event(client, plugin_id, event, None)
                .await?;
            self.apply_state_updates(plugin_id, &state_updates)?;
            if let Some(summary) = summary {
                summaries.push(summary);
            }
        }
        Ok(summaries)
    }

    async fn call_lifecycle_event(
        &self,
        client: &mut RuntimePluginClient,
        plugin_id: &str,
        event: &RuntimePluginLifecycleEvent,
        record: Option<&PluginEventRecord>,
    ) -> Result<(Option<String>, Vec<PluginStateMutation>)> {
        let mut params = serde_json::to_value(event)?;
        if let (Some(record), Some(object)) = (record, params.as_object_mut()) {
            object.insert(
                "ledger".to_string(),
                json!({
                    "event_id": record.event_id,
                    "sequence": record.sequence,
                    "event_type": record.event_type,
                    "occurred_at": record.occurred_at,
                }),
            );
        }
        let ack = self
            .call_plugin::<RuntimePluginEventAck>(client, "plugin.handle_event", params)
            .await?;
        if let Some(record) = record {
            if ack.acknowledged_event_id.as_deref() != Some(record.event_id.as_str()) {
                anyhow::bail!(
                    "Runtime plugin '{}' acknowledged lifecycle event {:?}, expected '{}'",
                    plugin_id,
                    ack.acknowledged_event_id,
                    record.event_id
                );
            }
        }
        Ok((
            ack.summary.filter(|summary| !summary.trim().is_empty()),
            ack.state_updates,
        ))
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

            let call_result = {
                let mut client = plugin.client.lock().await;
                self.call_plugin::<RuntimePluginPromptResponse>(
                    &mut client,
                    "plugin.get_prompt_contributions",
                    serde_json::to_value(query)?,
                )
                .await
            };
            match call_result {
                Ok(response) => {
                    if let Err(error) =
                        self.apply_state_updates(plugin.bundle.id(), &response.state_updates)
                    {
                        tracing::warn!(
                            "Runtime plugin '{}' returned invalid prompt state updates: {}",
                            plugin.bundle.id(),
                            error
                        );
                        self.deactivate_failed_plugin(plugin.bundle.id(), &error)
                            .await;
                        continue;
                    }
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
                    self.deactivate_failed_plugin(plugin.bundle.id(), &error)
                        .await;
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
        context: &ToolContext,
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

        let invoked_at = Utc::now();
        let mut client = plugin.client.lock().await;
        let configured_settings = plugin.configured_settings.read().await.clone();
        let call_result = self
            .call_plugin::<RuntimePluginToolResult>(
                &mut client,
                "plugin.invoke_tool",
                serde_json::to_value(RuntimePluginToolInvocation {
                    tool: tool_name.to_string(),
                    arguments,
                    context: RuntimePluginToolInvocationContext {
                        conversation_id: context.conversation_id.clone(),
                        loop_name: Some(if context.autonomous {
                            "autonomous".to_string()
                        } else {
                            "interactive".to_string()
                        }),
                        username: context.username.clone(),
                        autonomous: context.autonomous,
                        working_directory: context.working_directory.clone(),
                        invoked_at: invoked_at.to_rfc3339(),
                        deadline_at: Some(
                            (invoked_at
                                + chrono::Duration::from_std(RUNTIME_PLUGIN_TOOL_RPC_TIMEOUT)
                                    .unwrap_or_else(|_| chrono::Duration::seconds(300)))
                            .to_rfc3339(),
                        ),
                    },
                })?,
            )
            .await;
        drop(client);
        let result = match call_result {
            Ok(result) => result,
            Err(error) => {
                self.deactivate_failed_plugin(plugin_id, &error).await;
                anyhow::bail!(
                    "Runtime plugin '{}' could not complete '{}'; its process was reset to the durable state boundary: {}",
                    plugin_id,
                    tool_name,
                    error
                );
            }
        };
        if let Err(error) = self.apply_state_updates(plugin_id, &result.state_updates) {
            self.deactivate_failed_plugin(plugin_id, &error).await;
            return Err(error).with_context(|| {
                format!(
                    "Runtime plugin '{}' result for '{}' could not commit host-owned state",
                    plugin_id, tool_name
                )
            });
        }
        Ok(convert_tool_result(result, &configured_settings))
    }

    fn accept_poll_response(
        &self,
        plugin_id: &str,
        response: RuntimePluginPollResponse,
        transient_events: &mut Vec<crate::skills::SkillEvent>,
    ) -> Result<()> {
        let RuntimePluginPollResponse {
            events,
            state_updates,
        } = response;

        if let Some(ledger) = &self.event_ledger {
            for event in &events {
                ledger
                    .record_polled_event(plugin_id, event)
                    .with_context(|| {
                        format!(
                            "durably record runtime plugin '{}' event '{}'",
                            plugin_id, event.id
                        )
                    })?;
            }
        } else {
            transient_events.extend(events.into_iter().map(|event| {
                crate::skills::SkillEvent::NewContent {
                    id: format!("{}:{}", plugin_id, event.id),
                    source: event.source,
                    author: event.author,
                    body: event.body,
                    parent_ids: event.parent_ids,
                }
            }));
        }

        // Poll state commonly contains a remote cursor. It must advance only after
        // every observation returned alongside it is safely in the durable ledger.
        self.apply_state_updates(plugin_id, &state_updates)
    }

    pub async fn poll_plugin_events(&self) -> Result<PluginSkillEventBatch> {
        let plugins = self
            .loaded
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut transient_events = Vec::new();
        for plugin in plugins {
            if !plugin.handshake.capabilities.skill_polling {
                continue;
            }
            let call_result = {
                let mut client = plugin.client.lock().await;
                self.call_plugin::<RuntimePluginPollResponse>(
                    &mut client,
                    "plugin.poll_events",
                    serde_json::json!({}),
                )
                .await
            };
            match call_result {
                Ok(response) => {
                    if let Err(error) = self.accept_poll_response(
                        plugin.bundle.id(),
                        response,
                        &mut transient_events,
                    ) {
                        tracing::warn!(
                            "Runtime plugin '{}' poll response was not accepted: {}",
                            plugin.bundle.id(),
                            error
                        );
                        // Protocol v1 has no two-phase callback commit. A
                        // restarted process is therefore the rollback boundary:
                        // configure restores the last host-owned cursor/state.
                        self.deactivate_failed_plugin(plugin.bundle.id(), &error)
                            .await;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "Runtime plugin '{}' poll_events failed: {}",
                        plugin.bundle.id(),
                        error
                    );
                    self.deactivate_failed_plugin(plugin.bundle.id(), &error)
                        .await;
                }
            }
        }
        if let Some(ledger) = &self.event_ledger {
            let batch = ledger.pending_skill_events()?;
            if batch.quarantined_count > 0 {
                tracing::warn!(
                    quarantined_count = batch.quarantined_count,
                    "Quarantined malformed plugin events while preparing delivery"
                );
            }
            Ok(batch)
        } else {
            Ok(PluginSkillEventBatch {
                events: transient_events,
                through_sequence: 0,
                receipt: None,
                quarantined_count: 0,
            })
        }
    }

    pub fn acknowledge_plugin_events(&self, batch: &PluginSkillEventBatch) -> Result<()> {
        if let Some(ledger) = &self.event_ledger {
            ledger.acknowledge_skill_events(batch)?;
        } else if batch.receipt.is_some() {
            anyhow::bail!("cannot acknowledge a durable plugin event batch without its ledger");
        }
        Ok(())
    }

    async fn start_plugin(
        &self,
        bundle: RuntimeProcessPluginBundle,
        settings: Value,
        generation: u64,
    ) -> std::result::Result<Arc<LoadedRuntimePlugin>, RuntimePluginStartFailure> {
        let launch = bundle.launch_spec();
        let Some((command, args)) = launch.command.split_first() else {
            return Err(RuntimePluginStartFailure::Terminal(anyhow::anyhow!(
                "Runtime plugin command cannot be empty"
            )));
        };

        let mut process = Command::new(command);
        process
            .args(args)
            .current_dir(&launch.working_directory)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = process.spawn().map_err(|error| {
            RuntimePluginStartFailure::Retryable(
                anyhow::Error::new(error)
                    .context(format!("Failed to launch runtime plugin '{}'", bundle.id())),
            )
        })?;
        let process_id = child.id();
        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill().await;
            return Err(RuntimePluginStartFailure::Retryable(anyhow::anyhow!(
                "Runtime plugin child is missing stdin pipe"
            )));
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill().await;
            return Err(RuntimePluginStartFailure::Retryable(anyhow::anyhow!(
                "Runtime plugin child is missing stdout pipe"
            )));
        };
        let Some(stderr) = child.stderr.take() else {
            let _ = child.kill().await;
            return Err(RuntimePluginStartFailure::Retryable(anyhow::anyhow!(
                "Runtime plugin child is missing stderr pipe"
            )));
        };
        Self::spawn_plugin_stderr_logger(bundle.id().to_string(), stderr);
        let mut client = RuntimePluginClient {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        };

        let handshake_request = RuntimePluginHandshakeRequest {
            supported_protocol_versions: SUPPORTED_PLUGIN_PROTOCOL_VERSIONS.to_vec(),
            host: PluginHostDescriptor {
                name: "ponderer".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };
        let handshake_params = serde_json::to_value(handshake_request).map_err(|error| {
            RuntimePluginStartFailure::Terminal(
                anyhow::Error::new(error)
                    .context("Failed to serialize runtime plugin handshake request"),
            )
        })?;
        let handshake: RuntimePluginHandshake = match self
            .call_plugin(&mut client, "plugin.handshake", handshake_params)
            .await
        {
            Ok(handshake) => handshake,
            Err(error) => {
                let _ = client.child.kill().await;
                return Err(Self::classify_start_rpc_failure(error));
            }
        };
        if !is_supported_plugin_protocol_version(handshake.protocol_version) {
            let _ = client.child.kill().await;
            return Err(RuntimePluginStartFailure::Terminal(anyhow::anyhow!(
                "Runtime plugin '{}' selected unsupported protocol version {} (host supports {:?})",
                bundle.id(),
                handshake.protocol_version,
                SUPPORTED_PLUGIN_PROTOCOL_VERSIONS
            )));
        }
        if let Err(error) = validate_handshake_against_package(bundle.manifest(), &handshake) {
            let _ = client.child.kill().await;
            return Err(RuntimePluginStartFailure::Terminal(error));
        }

        let handles_settings_changed =
            plugin_declares_event_hook(bundle.manifest(), &handshake, "settings_changed");
        if let Err(error) = self
            .configure_client(
                &mut client,
                &bundle,
                settings.clone(),
                handles_settings_changed,
            )
            .await
        {
            let _ = client.child.kill().await;
            return Err(Self::classify_start_rpc_failure(error));
        }

        for hook in &handshake.capabilities.event_hooks {
            if hook == "settings_changed" {
                // Settings are plugin-scoped control data, not a global temporal
                // stream. They are delivered directly during configure above.
                continue;
            }
            let subscription = format!("host.lifecycle.{hook}");
            if let Err(error) = self
                .deliver_lifecycle_subscription(&mut client, bundle.id(), &subscription, None)
                .await
            {
                let _ = client.child.kill().await;
                return Err(RuntimePluginStartFailure::Retryable(error.context(
                    format!(
                        "Runtime plugin '{}' could not accept pending lifecycle subscription '{}'",
                        bundle.id(),
                        subscription
                    ),
                )));
            }
        }

        Ok(Arc::new(LoadedRuntimePlugin {
            bundle,
            handshake,
            generation,
            process_id,
            client: Mutex::new(client),
            registered_tools: RwLock::new(Vec::new()),
            tool_registration_complete: AtomicBool::new(false),
            configured_settings: RwLock::new(settings),
        }))
    }

    async fn configure_loaded_plugin(
        &self,
        plugin: Arc<LoadedRuntimePlugin>,
        settings: Value,
    ) -> Result<RuntimePluginConfigureOutcome> {
        if *plugin.configured_settings.read().await == settings {
            return Ok(RuntimePluginConfigureOutcome::Unchanged);
        }
        let mut client = plugin.client.lock().await;
        let handles_settings_changed = plugin_declares_event_hook(
            plugin.bundle.manifest(),
            &plugin.handshake,
            "settings_changed",
        );
        self.configure_client(
            &mut client,
            &plugin.bundle,
            settings.clone(),
            handles_settings_changed,
        )
        .await?;
        *plugin.configured_settings.write().await = settings;
        Ok(RuntimePluginConfigureOutcome::Applied)
    }

    fn spawn_plugin_stderr_logger(plugin_id: String, stderr: ChildStderr) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        tracing::info!(
                            target: "runtime_plugin_stderr",
                            plugin_id = %plugin_id,
                            "{}",
                            trimmed
                        );
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::debug!(
                            target: "runtime_plugin_stderr",
                            plugin_id = %plugin_id,
                            "Stopped reading plugin stderr: {}",
                            error
                        );
                        break;
                    }
                }
            }
        });
    }

    async fn configure_client(
        &self,
        client: &mut RuntimePluginClient,
        bundle: &RuntimeProcessPluginBundle,
        settings: Value,
        handles_settings_changed: bool,
    ) -> Result<()> {
        let state = self.state_snapshot(bundle.id())?;
        let raw_response: Value = self
            .call_plugin(
                client,
                "plugin.configure",
                serde_json::to_value(RuntimePluginConfigureRequest {
                    settings: settings.clone(),
                    state,
                })?,
            )
            .await?;
        let response = if raw_response.is_null() {
            RuntimePluginConfigureResponse::default()
        } else {
            serde_json::from_value::<RuntimePluginConfigureResponse>(raw_response)
                .context("parse runtime plugin configure response")?
        };
        if !response.configured {
            anyhow::bail!("Runtime plugin '{}' rejected configuration", bundle.id());
        }
        self.apply_state_updates(bundle.id(), &response.state_updates)?;
        if !handles_settings_changed {
            return Ok(());
        }

        let settings_event = RuntimePluginLifecycleEvent::SettingsChanged {
            plugin_id: bundle.id().to_string(),
            settings,
        };
        match self
            .call_lifecycle_event(client, bundle.id(), &settings_event, None)
            .await
        {
            Ok((_summary, state_updates)) => {
                // A successful callback has already committed SDK-local state.
                // Propagate persistence failure so the caller restarts and
                // restores the last host-owned snapshot.
                self.apply_state_updates(bundle.id(), &state_updates)?;
            }
            Err(error) => {
                if Self::is_transport_error(&error) {
                    return Err(error);
                }
                tracing::warn!(
                    "Runtime plugin '{}' rejected optional settings_changed event: {}",
                    bundle.id(),
                    error
                );
            }
        }
        Ok(())
    }

    async fn register_plugin_tools(
        self: &Arc<Self>,
        plugin: &Arc<LoadedRuntimePlugin>,
        tool_registry: Arc<ToolRegistry>,
    ) {
        if plugin.tool_registration_complete.load(Ordering::Acquire) {
            return;
        }
        for tool in &plugin.handshake.tools {
            let owns_name = plugin.registered_tools.read().await.contains(&tool.name);
            let existing = tool_registry.get(&tool.name).await;
            if !owns_name && existing.is_some() {
                tracing::warn!(
                    "Skipping runtime plugin tool '{}' from '{}' because the name is already registered",
                    tool.name,
                    plugin.bundle.id()
                );
                continue;
            }
            if !owns_name {
                plugin
                    .registered_tools
                    .write()
                    .await
                    .push(tool.name.clone());
            }
            if existing.is_some() {
                continue;
            }
            tool_registry
                .register(Arc::new(RuntimePluginToolProxy::new(
                    plugin.bundle.id().to_string(),
                    &plugin.handshake.version,
                    plugin.generation,
                    tool.clone(),
                    self.clone(),
                )))
                .await;
        }
        plugin
            .tool_registration_complete
            .store(true, Ordering::Release);
    }

    async fn stop_plugin(
        &self,
        plugin_id: &str,
        tool_registry: Arc<ToolRegistry>,
    ) -> Option<Arc<LoadedRuntimePlugin>> {
        let removed = self.loaded.write().await.remove(plugin_id);
        let Some(plugin) = removed else {
            return None;
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
        drop(client);
        Some(plugin)
    }

    async fn deactivate_failed_plugin(&self, plugin_id: &str, failure: &anyhow::Error) {
        let tool_registry = self.last_tool_registry.read().await.clone();
        let removed = if let Some(tool_registry) = tool_registry {
            self.stop_plugin(plugin_id, tool_registry).await
        } else {
            let removed = self.loaded.write().await.remove(plugin_id);
            if let Some(plugin) = &removed {
                let mut client = plugin.client.lock().await;
                let _ = client.child.kill().await;
            }
            removed
        };
        let Some(plugin) = removed else {
            return;
        };
        let mut lifecycles = self.lifecycles.write().await;
        let Some(lifecycle) = lifecycles.get_mut(plugin_id) else {
            return;
        };
        if let Err(error) = lifecycle.mark_failed(
            plugin.generation,
            format!("{failure:#}"),
            Utc::now(),
            &self.restart_policy,
        ) {
            tracing::debug!(
                "Runtime plugin '{}' transport failure was stale: {}",
                plugin_id,
                error
            );
        }
    }

    fn classify_start_rpc_failure(error: anyhow::Error) -> RuntimePluginStartFailure {
        if Self::is_transport_error(&error) {
            RuntimePluginStartFailure::Retryable(error)
        } else {
            RuntimePluginStartFailure::Terminal(error)
        }
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
        let timeout_duration = rpc_timeout_for_method(method);
        await_rpc_with_timeout(
            method,
            timeout_duration,
            self.call_plugin_raw_io(client, method, params),
        )
        .await
    }

    async fn call_plugin_raw_io(
        &self,
        client: &mut RuntimePluginClient,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let request = RuntimePluginRpcRequest {
            protocol_version: CURRENT_PLUGIN_PROTOCOL_VERSION,
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
        if !is_supported_plugin_protocol_version(response.protocol_version) {
            anyhow::bail!(
                "Runtime plugin response for '{}' used unsupported protocol version {}",
                method,
                response.protocol_version
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
            || message.contains("runtime plugin transport timeout")
            || message.contains("closed stdout")
            || message.contains("failed to write runtime plugin request")
            || message.contains("failed to flush runtime plugin request")
            || message.contains("failed to read runtime plugin response")
            || message.contains("process exited")
            || message.contains("connection reset by peer")
    }
}

fn plugin_settings_value(config: &AgentConfig, plugin_id: &str) -> Value {
    config
        .plugin_settings
        .get(plugin_id)
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn plugin_declares_event_hook(
    manifest: &PluginManifest,
    handshake: &RuntimePluginHandshake,
    hook: &str,
) -> bool {
    let runtime_declares_hook = handshake
        .capabilities
        .event_hooks
        .iter()
        .any(|declared| declared == hook);
    if !runtime_declares_hook {
        return false;
    }

    match &manifest.contributions {
        Some(contributions) => contributions
            .event_hooks
            .iter()
            .any(|declared| declared == hook),
        None => true,
    }
}

fn validate_handshake_against_package(
    manifest: &PluginManifest,
    handshake: &RuntimePluginHandshake,
) -> Result<()> {
    if handshake.id != manifest.id {
        anyhow::bail!(
            "Runtime plugin '{}' reported mismatched handshake id '{}'",
            manifest.id,
            handshake.id
        );
    }
    if handshake.version != manifest.version {
        anyhow::bail!(
            "Runtime plugin '{}' reported version '{}' but its package declares '{}'",
            manifest.id,
            handshake.version,
            manifest.version
        );
    }

    let runtime_tool_names = handshake.tool_names();
    let runtime_tools = runtime_tool_names.iter().collect::<HashSet<_>>();
    if runtime_tools.len() != runtime_tool_names.len() {
        anyhow::bail!(
            "Runtime plugin '{}' reported duplicate tool names",
            manifest.id
        );
    }
    if !handshake.tools.is_empty() && !handshake.capabilities.tools.is_empty() {
        let capability_tools = handshake.capabilities.tools.iter().collect::<HashSet<_>>();
        if capability_tools.len() != handshake.capabilities.tools.len()
            || capability_tools != runtime_tools
        {
            anyhow::bail!(
                "Runtime plugin '{}' reported inconsistent tool names between capabilities and tool manifests",
                manifest.id
            );
        }
    }
    if !manifest.tools.is_empty() {
        if manifest.tools.len() != handshake.tools.len()
            || manifest.tools.iter().any(|declared| {
                !handshake
                    .tools
                    .iter()
                    .any(|runtime| runtime.name == declared.name && runtime == declared)
            })
        {
            anyhow::bail!(
                "Runtime plugin '{}' handshake tool contracts do not match its static package contract",
                manifest.id
            );
        }
    } else if manifest.contributions.is_some() && !runtime_tools.is_empty() {
        anyhow::bail!(
            "Runtime plugin '{}' reported tools without a structured static package contract",
            manifest.id
        );
    }
    if manifest.provided_tools.is_empty() {
        if !runtime_tools.is_empty() {
            tracing::warn!(
                plugin_id = %manifest.id,
                "Legacy package has no static provided_tools declaration; accepting runtime tools for compatibility"
            );
        }
    } else {
        let declared_tools = manifest.provided_tools.iter().collect::<HashSet<_>>();
        if declared_tools.len() != manifest.provided_tools.len() || declared_tools != runtime_tools
        {
            anyhow::bail!(
                "Runtime plugin '{}' handshake tools do not match package provided_tools",
                manifest.id
            );
        }
    }

    if let Some(contributions) = &manifest.contributions {
        let declared_hooks = contributions.event_hooks.iter().collect::<HashSet<_>>();
        let runtime_hooks = handshake
            .capabilities
            .event_hooks
            .iter()
            .collect::<HashSet<_>>();
        if declared_hooks.len() != contributions.event_hooks.len()
            || runtime_hooks.len() != handshake.capabilities.event_hooks.len()
            || declared_hooks != runtime_hooks
        {
            anyhow::bail!(
                "Runtime plugin '{}' handshake event hooks do not match its static contribution contract",
                manifest.id
            );
        }
        let declared_slots = contributions
            .prompt_slots
            .iter()
            .map(|slot| slot.wire_name())
            .collect::<HashSet<_>>();
        let runtime_slots = handshake
            .capabilities
            .prompt_slots
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        if declared_slots.len() != contributions.prompt_slots.len()
            || runtime_slots.len() != handshake.capabilities.prompt_slots.len()
            || declared_slots != runtime_slots
        {
            anyhow::bail!(
                "Runtime plugin '{}' handshake prompt slots do not match its static contribution contract",
                manifest.id
            );
        }
        if contributions.poll_events != handshake.capabilities.skill_polling {
            anyhow::bail!(
                "Runtime plugin '{}' handshake polling capability does not match its static contribution contract",
                manifest.id
            );
        }
    } else if !handshake.capabilities.event_hooks.is_empty()
        || !handshake.capabilities.prompt_slots.is_empty()
        || handshake.capabilities.skill_polling
    {
        tracing::warn!(
            plugin_id = %manifest.id,
            "Legacy package has no static contribution contract; accepting runtime hooks for compatibility"
        );
    }

    let requested_capabilities = handshake
        .capabilities
        .requested_capabilities
        .iter()
        .collect::<HashSet<_>>();
    if requested_capabilities.len() != handshake.capabilities.requested_capabilities.len() {
        anyhow::bail!(
            "Runtime plugin '{}' reported duplicate requested capabilities",
            manifest.id
        );
    }
    if manifest.requested_capabilities.is_empty() {
        if !requested_capabilities.is_empty() {
            if manifest.contributions.is_some() {
                anyhow::bail!(
                    "Runtime plugin '{}' requested capabilities absent from its strict static package contract",
                    manifest.id
                );
            } else {
                tracing::warn!(
                    plugin_id = %manifest.id,
                    "Legacy package has no static requested_capabilities declaration; accepting runtime requests for compatibility"
                );
            }
        }
    } else {
        let declared_capabilities = manifest
            .requested_capabilities
            .iter()
            .collect::<HashSet<_>>();
        if let Some(capability) = requested_capabilities
            .difference(&declared_capabilities)
            .next()
        {
            anyhow::bail!(
                "Runtime plugin '{}' requested undeclared capability '{}'",
                manifest.id,
                capability
            );
        }
    }

    let runtime_effect_ids = handshake
        .tools
        .iter()
        .flat_map(|tool| tool.effects.iter().map(|effect| effect.id.as_str()))
        .collect::<HashSet<_>>();
    if manifest.declared_effects.is_empty() {
        if !runtime_effect_ids.is_empty() {
            if manifest.contributions.is_some() {
                anyhow::bail!(
                    "Runtime plugin '{}' reported effects absent from its strict static package contract",
                    manifest.id
                );
            } else {
                tracing::warn!(
                    plugin_id = %manifest.id,
                    "Legacy package has no static declared_effects; accepting runtime tool effects for compatibility"
                );
            }
        }
    } else {
        let declared_effect_ids = manifest
            .declared_effects
            .iter()
            .map(|effect| effect.id.as_str())
            .collect::<HashSet<_>>();
        if let Some(effect_id) = runtime_effect_ids.difference(&declared_effect_ids).next() {
            anyhow::bail!(
                "Runtime plugin '{}' tool reported undeclared effect '{}'",
                manifest.id,
                effect_id
            );
        }
    }

    Ok(())
}

fn runtime_status_from_snapshot(
    snapshot: PluginLifecycleSnapshot,
    runtime_metadata: Option<(u32, Option<u32>)>,
) -> PluginRuntimeStatus {
    let (negotiated_protocol_version, process_id) = runtime_metadata
        .map(|(protocol_version, process_id)| (Some(protocol_version), process_id))
        .unwrap_or((None, None));
    PluginRuntimeStatus {
        plugin_id: snapshot.plugin_id,
        desired_enabled: snapshot.desired_state.is_enabled(),
        available: snapshot.available,
        state: match snapshot.state {
            PluginOperationalState::Unavailable => PluginRuntimeState::Unavailable,
            PluginOperationalState::Disabled => PluginRuntimeState::Disabled,
            PluginOperationalState::Stopped => PluginRuntimeState::Stopped,
            PluginOperationalState::Starting => PluginRuntimeState::Starting,
            PluginOperationalState::Running => PluginRuntimeState::Running,
            PluginOperationalState::Degraded => PluginRuntimeState::Degraded,
            PluginOperationalState::Stopping => PluginRuntimeState::Stopping,
            PluginOperationalState::Backoff => PluginRuntimeState::Backoff,
            PluginOperationalState::CircuitOpen => PluginRuntimeState::CircuitOpen,
            PluginOperationalState::Failed => PluginRuntimeState::Failed,
        },
        generation: snapshot.generation,
        restart_attempts: snapshot.restart_attempts.min(u32::MAX as u64) as u32,
        consecutive_failures: snapshot.consecutive_failures,
        negotiated_protocol_version,
        process_id,
        state_changed_at: Some(snapshot.state_changed_at.to_rfc3339()),
        last_started_at: snapshot.last_started_at.map(|time| time.to_rfc3339()),
        last_stopped_at: snapshot.last_stopped_at.map(|time| time.to_rfc3339()),
        last_healthy_at: snapshot.last_healthy_at.map(|time| time.to_rfc3339()),
        last_error: snapshot.last_error,
        last_error_at: snapshot.last_error_at.map(|time| time.to_rfc3339()),
        next_retry_at: snapshot.next_retry_at.map(|time| time.to_rfc3339()),
    }
}

fn rpc_timeout_for_method(method: &str) -> Duration {
    match method {
        "plugin.get_prompt_contributions" => RUNTIME_PLUGIN_PROMPT_RPC_TIMEOUT,
        "plugin.invoke_tool" => RUNTIME_PLUGIN_TOOL_RPC_TIMEOUT,
        _ => DEFAULT_RUNTIME_PLUGIN_RPC_TIMEOUT,
    }
}

async fn await_rpc_with_timeout<T, F>(
    method: &str,
    timeout_duration: Duration,
    operation: F,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout_duration, operation).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "Runtime plugin transport timeout during '{}' after {}ms",
            method,
            timeout_duration.as_millis()
        ),
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

fn convert_tool_result(mut result: RuntimePluginToolResult, plugin_settings: &Value) -> ToolOutput {
    match result.kind {
        RuntimePluginToolResultKind::Text => ToolOutput::Text(result.text.unwrap_or_default()),
        RuntimePluginToolResultKind::Json => {
            let mut data = result.data.take().unwrap_or(Value::Null);
            apply_media_autoplay_default(&mut data, plugin_settings);
            ToolOutput::Json(data)
        }
        RuntimePluginToolResultKind::Error => ToolOutput::Error(
            result
                .text
                .or_else(|| result.data.map(|value| value.to_string()))
                .unwrap_or_else(|| "Runtime plugin tool failed".to_string()),
        ),
    }
}

fn apply_media_autoplay_default(data: &mut Value, plugin_settings: &Value) {
    let auto_play = plugin_settings
        .get("auto_play_generated_media")
        .and_then(Value::as_bool)
        .or_else(|| {
            plugin_settings
                .get("auto_play_generated_audio")
                .and_then(Value::as_bool)
        });
    let Some(auto_play) = auto_play else {
        return;
    };
    let Some(media) = data.get_mut("media").and_then(Value::as_array_mut) else {
        return;
    };

    for item in media {
        let is_audio = item
            .get("media_kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("audio"))
            || item
                .get("mime_type")
                .and_then(Value::as_str)
                .is_some_and(|mime| mime.to_ascii_lowercase().starts_with("audio/"));
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        if is_audio && !object.contains_key("auto_play") {
            object.insert("auto_play".to_string(), Value::Bool(auto_play));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::{BufRead, Write};

    use super::*;

    const FIXTURE_PLUGIN_ID: &str = "test.runtime-supervisor";

    fn write_runtime_fixture_package(root: &std::path::Path) {
        write_runtime_fixture_package_with_id(root, FIXTURE_PLUGIN_ID);
    }

    fn write_runtime_fixture_package_with_id(root: &std::path::Path, package_id: &str) {
        let plugin_dir = root.join("runtime-fixture");
        fs::create_dir_all(&plugin_dir).unwrap();
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let command = [
            executable,
            "--ignored".to_string(),
            "--exact".to_string(),
            "runtime_plugin_host::tests::runtime_plugin_fixture_process".to_string(),
            "--nocapture".to_string(),
        ]
        .into_iter()
        .map(|token| toml::Value::String(token).to_string())
        .collect::<Vec<_>>()
        .join(", ");
        fs::write(
            plugin_dir.join("tools.json"),
            serde_json::to_vec_pretty(&json!({
                "tools": [{
                    "name": "fixture.echo",
                    "description": "fixture tool",
                    "parameters": {"type": "object", "properties": {}},
                    "requires_approval": false,
                    "category": "general",
                    "effects": []
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                r#"
                    manifest_version = 1
                    protocol_version = 1
                    id = "{package_id}"
                    name = "Runtime supervisor fixture"
                    version = "1.0.0"
                    description = "test-only process plugin"
                    plugin_type = "runtime_process"
                    command = [{command}]
                    tool_contract_file = "tools.json"

                    [contributions]
                    event_hooks = ["persona_evolved"]
                    prompt_slots = []
                    poll_events = true
                "#
            ),
        )
        .unwrap();
    }

    fn fixture_config(count_path: &std::path::Path, exit_after_configure: bool) -> AgentConfig {
        let mut config = AgentConfig::default();
        config.plugin_settings.insert(
            FIXTURE_PLUGIN_ID.to_string(),
            json!({
                "enabled": true,
                "count_path": count_path,
                "exit_after_configure": exit_after_configure,
                "revision": 1,
            }),
        );
        config
    }

    fn authority_manifest() -> PluginManifest {
        PluginManifest {
            manifest_version: crate::plugin_contract::CURRENT_PLUGIN_MANIFEST_VERSION,
            protocol_version: CURRENT_PLUGIN_PROTOCOL_VERSION,
            id: "dev.ponderer.authority-fixture".to_string(),
            kind: crate::plugin_contract::PluginKind::RuntimeProcessBundle,
            name: "Authority fixture".to_string(),
            version: "1.2.3".to_string(),
            description: "fixture".to_string(),
            provided_tools: vec!["fixture.read".to_string()],
            tools: Vec::new(),
            provided_skills: Vec::new(),
            requested_capabilities: vec!["network.read".to_string()],
            declared_effects: vec![crate::plugin_contract::PluginEffectDeclaration {
                id: "network.read".to_string(),
                description: None,
                requires_approval: false,
            }],
            contributions: None,
            settings_tab: None,
            settings_schema: None,
        }
    }

    fn authority_handshake() -> RuntimePluginHandshake {
        RuntimePluginHandshake {
            protocol_version: CURRENT_PLUGIN_PROTOCOL_VERSION,
            id: "dev.ponderer.authority-fixture".to_string(),
            name: "Authority fixture".to_string(),
            version: "1.2.3".to_string(),
            capabilities: RuntimePluginCapabilities {
                tools: vec!["fixture.read".to_string()],
                requested_capabilities: vec!["network.read".to_string()],
                ..Default::default()
            },
            tools: vec![RuntimePluginToolManifest {
                name: "fixture.read".to_string(),
                description: "read fixture".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
                requires_approval: false,
                category: RuntimePluginToolCategory::Network,
                effects: vec![crate::plugin_contract::PluginEffectDeclaration {
                    id: "network.read".to_string(),
                    description: None,
                    requires_approval: false,
                }],
            }],
        }
    }

    #[test]
    fn plugin_state_is_namespaced_and_restored_through_the_host() {
        let path = std::env::temp_dir().join(format!(
            "ponderer_runtime_plugin_state_{}.db",
            uuid::Uuid::new_v4()
        ));
        let database = Arc::new(AgentDatabase::new(&path).expect("database"));
        let host = RuntimePluginHost::with_catalog_and_database(
            Arc::new(Default::default()),
            Some(database),
        );

        host.apply_state_updates(
            "dev.clock",
            &[PluginStateMutation {
                key: "last_tick".to_string(),
                schema_version: 2,
                value: json!({"at": "2026-07-13T12:00:00Z"}),
                delete: false,
            }],
        )
        .expect("persist state");
        let snapshot = host.state_snapshot("dev.clock").expect("state snapshot");
        assert_eq!(snapshot["last_tick"].schema_version, 2);
        assert_eq!(
            snapshot["last_tick"].value,
            json!({"at": "2026-07-13T12:00:00Z"})
        );
        assert!(host
            .state_snapshot("dev.other")
            .expect("other namespace")
            .is_empty());

        host.apply_state_updates(
            "dev.clock",
            &[PluginStateMutation {
                key: "last_tick".to_string(),
                schema_version: 2,
                value: Value::Null,
                delete: true,
            }],
        )
        .expect("delete state");
        assert!(host
            .state_snapshot("dev.clock")
            .expect("deleted snapshot")
            .is_empty());
        drop(host);
        let _ = fs::remove_file(path);
    }

    #[test]
    #[ignore = "launched as a subprocess by runtime host tests"]
    fn runtime_plugin_fixture_process() {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout().lock();
        let mut exit_after_configure = false;
        let mut event_count_path = None;
        let mut poll_oversized_event = false;
        let mut wrong_lifecycle_ack = false;
        for line in stdin.lock().lines() {
            let line = line.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let method = request["method"].as_str().unwrap();
            let result = match method {
                "plugin.handshake" => json!({
                    "protocol_version": CURRENT_PLUGIN_PROTOCOL_VERSION,
                    "id": FIXTURE_PLUGIN_ID,
                    "name": "Runtime supervisor fixture",
                    "version": "1.0.0",
                    "capabilities": {
                        "tools": ["fixture.echo"],
                        "event_hooks": ["persona_evolved"],
                        "skill_polling": true
                    },
                    "tools": [{
                        "name": "fixture.echo",
                        "description": "fixture tool",
                        "parameters": {"type": "object", "properties": {}},
                    }],
                }),
                "plugin.configure" => {
                    let settings = &request["params"]["settings"];
                    if let Some(path) = settings["count_path"].as_str() {
                        let mut file = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)
                            .unwrap();
                        writeln!(file, "configured").unwrap();
                    }
                    exit_after_configure =
                        settings["exit_after_configure"].as_bool().unwrap_or(false);
                    event_count_path = settings["event_count_path"].as_str().map(str::to_string);
                    poll_oversized_event =
                        settings["poll_oversized_event"].as_bool().unwrap_or(false);
                    wrong_lifecycle_ack =
                        settings["wrong_lifecycle_ack"].as_bool().unwrap_or(false);
                    json!({
                        "configured": !settings["reject_configure"]
                            .as_bool()
                            .unwrap_or(false)
                    })
                }
                "plugin.handle_event" => {
                    if let Some(path) = &event_count_path {
                        let mut file = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)
                            .unwrap();
                        writeln!(file, "{}", request["params"]["event"]).unwrap();
                    }
                    json!({
                        "state_changed": false,
                        "acknowledged_event_id": if wrong_lifecycle_ack {
                            json!("wrong-event-id")
                        } else {
                            request["params"]["ledger"]["event_id"].clone()
                        }
                    })
                }
                "plugin.poll_events" if poll_oversized_event => json!({
                    "events": [
                        {
                            "id": "poll-small",
                            "source": "fixture",
                            "author": "fixture",
                            "body": "recorded before failure",
                            "parent_ids": []
                        },
                        {
                            "id": "poll-oversized",
                            "source": "fixture",
                            "author": "fixture",
                            "body": "x".repeat(300 * 1024),
                            "parent_ids": []
                        }
                    ],
                    "state_updates": [{
                        "key": "remote_cursor",
                        "schema_version": 1,
                        "value": "after-oversized",
                        "delete": false
                    }]
                }),
                "plugin.poll_events" => json!({"events": []}),
                _ => Value::Null,
            };
            let response = json!({
                "protocol_version": CURRENT_PLUGIN_PROTOCOL_VERSION,
                "id": request["id"],
                "ok": true,
                "result": result,
            });
            writeln!(stdout, "{}", serde_json::to_string(&response).unwrap()).unwrap();
            stdout.flush().unwrap();
            if method == "plugin.configure" && exit_after_configure {
                return;
            }
        }
    }

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
        let output = convert_tool_result(
            RuntimePluginToolResult {
                kind: RuntimePluginToolResultKind::Error,
                text: Some("boom".to_string()),
                data: None,
                state_updates: Vec::new(),
            },
            &json!({}),
        );

        assert!(matches!(output, ToolOutput::Error(message) if message == "boom"));
    }

    #[test]
    fn runtime_media_autoplay_default_is_plugin_local_and_audio_only() {
        let output = convert_tool_result(
            RuntimePluginToolResult {
                kind: RuntimePluginToolResultKind::Json,
                text: None,
                data: Some(json!({
                    "media": [
                        {"path": "/tmp/new.wav", "media_kind": "audio"},
                        {"path": "/tmp/image.png", "media_kind": "image"},
                        {"path": "/tmp/quiet.wav", "mime_type": "audio/wav", "auto_play": false}
                    ]
                })),
                state_updates: Vec::new(),
            },
            &json!({
                "auto_play_generated_media": true,
                "auto_play_generated_audio": false
            }),
        );

        let ToolOutput::Json(data) = output else {
            panic!("expected JSON output");
        };
        assert_eq!(data["media"][0]["auto_play"], true);
        assert!(data["media"][1].get("auto_play").is_none());
        assert_eq!(data["media"][2]["auto_play"], false);

        let mut legacy_data = json!({
            "media": [{"path": "/tmp/legacy.wav", "media_kind": "audio"}]
        });
        apply_media_autoplay_default(
            &mut legacy_data,
            &json!({"auto_play_generated_audio": true}),
        );
        assert_eq!(legacy_data["media"][0]["auto_play"], true);
    }

    #[test]
    fn settings_hook_dispatch_requires_runtime_and_strict_static_declarations() {
        let mut manifest = authority_manifest();
        manifest.contributions = Some(Default::default());
        manifest
            .contributions
            .as_mut()
            .unwrap()
            .event_hooks
            .push("settings_changed".to_string());
        let mut handshake = authority_handshake();
        handshake
            .capabilities
            .event_hooks
            .push("settings_changed".to_string());

        assert!(plugin_declares_event_hook(
            &manifest,
            &handshake,
            "settings_changed"
        ));

        manifest.contributions.as_mut().unwrap().event_hooks.clear();
        assert!(!plugin_declares_event_hook(
            &manifest,
            &handshake,
            "settings_changed"
        ));

        manifest.contributions = None;
        assert!(plugin_declares_event_hook(
            &manifest,
            &handshake,
            "settings_changed"
        ));
        handshake.capabilities.event_hooks.clear();
        assert!(!plugin_declares_event_hook(
            &manifest,
            &handshake,
            "settings_changed"
        ));
    }

    #[test]
    fn poll_checkpoint_waits_for_every_durable_event_append() {
        let path = std::env::temp_dir().join(format!(
            "ponderer_runtime_plugin_poll_checkpoint_{}.db",
            uuid::Uuid::new_v4()
        ));
        let database = Arc::new(AgentDatabase::new(&path).expect("database"));
        let host = RuntimePluginHost::with_catalog_and_database(
            Arc::new(Default::default()),
            Some(database),
        );
        let checkpoint = PluginStateMutation {
            key: "remote_cursor".to_string(),
            schema_version: 1,
            value: json!("after-two"),
            delete: false,
        };
        let event = |id: &str, body: String| RuntimePluginPollEvent {
            id: id.to_string(),
            source: "fixture".to_string(),
            author: "tester".to_string(),
            body,
            parent_ids: Vec::new(),
        };
        let mut transient_events = Vec::new();

        let error = host
            .accept_poll_response(
                "dev.poller",
                RuntimePluginPollResponse {
                    events: vec![
                        event("one", "recorded".to_string()),
                        event("two", "x".repeat(300 * 1024)),
                    ],
                    state_updates: vec![checkpoint.clone()],
                },
                &mut transient_events,
            )
            .expect_err("oversized second event should fail durable recording");
        assert!(error.to_string().contains("event 'two'"));
        assert!(host
            .state_snapshot("dev.poller")
            .expect("state snapshot")
            .get("remote_cursor")
            .is_none());

        host.accept_poll_response(
            "dev.poller",
            RuntimePluginPollResponse {
                events: vec![
                    event("one", "deduplicated".to_string()),
                    event("two", "recorded on retry".to_string()),
                ],
                state_updates: vec![checkpoint],
            },
            &mut transient_events,
        )
        .expect("retry should record every event before advancing state");
        assert_eq!(
            host.state_snapshot("dev.poller").expect("state snapshot")["remote_cursor"].value,
            json!("after-two")
        );
        assert_eq!(
            host.event_ledger
                .as_ref()
                .expect("ledger")
                .pending_skill_events()
                .expect("pending events")
                .events
                .len(),
            2
        );

        drop(host);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn handshake_authority_must_match_static_package_declarations() {
        validate_handshake_against_package(&authority_manifest(), &authority_handshake())
            .expect("matching runtime authority should load");

        let mut wrong_version = authority_handshake();
        wrong_version.version = "9.9.9".to_string();
        assert!(
            validate_handshake_against_package(&authority_manifest(), &wrong_version)
                .unwrap_err()
                .to_string()
                .contains("reported version")
        );

        let mut extra_tool = authority_handshake();
        extra_tool
            .capabilities
            .tools
            .push("fixture.write".to_string());
        assert!(
            validate_handshake_against_package(&authority_manifest(), &extra_tool)
                .unwrap_err()
                .to_string()
                .contains("inconsistent tool names")
        );

        let mut undeclared_tool = authority_handshake();
        undeclared_tool
            .capabilities
            .tools
            .push("fixture.write".to_string());
        let mut write_tool = undeclared_tool.tools[0].clone();
        write_tool.name = "fixture.write".to_string();
        undeclared_tool.tools.push(write_tool);
        assert!(
            validate_handshake_against_package(&authority_manifest(), &undeclared_tool)
                .unwrap_err()
                .to_string()
                .contains("do not match package provided_tools")
        );

        let mut extra_capability = authority_handshake();
        extra_capability
            .capabilities
            .requested_capabilities
            .push("network.publish".to_string());
        assert!(
            validate_handshake_against_package(&authority_manifest(), &extra_capability)
                .unwrap_err()
                .to_string()
                .contains("undeclared capability")
        );

        let mut extra_effect = authority_handshake();
        extra_effect.tools[0]
            .effects
            .push(crate::plugin_contract::PluginEffectDeclaration {
                id: "external.publish".to_string(),
                description: None,
                requires_approval: true,
            });
        assert!(
            validate_handshake_against_package(&authority_manifest(), &extra_effect)
                .unwrap_err()
                .to_string()
                .contains("undeclared effect")
        );

        let mut structured_manifest = authority_manifest();
        structured_manifest.tools = authority_handshake().tools;
        validate_handshake_against_package(&structured_manifest, &authority_handshake())
            .expect("identical structured tool contract should load");
        let mut changed_schema = authority_handshake();
        changed_schema.tools[0].description = "runtime changed this".to_string();
        assert!(
            validate_handshake_against_package(&structured_manifest, &changed_schema)
                .unwrap_err()
                .to_string()
                .contains("static package contract")
        );

        let mut contribution_manifest = authority_manifest();
        contribution_manifest.contributions = Some(Default::default());
        assert!(
            validate_handshake_against_package(&contribution_manifest, &authority_handshake())
                .unwrap_err()
                .to_string()
                .contains("structured static package contract")
        );
        contribution_manifest.tools = authority_handshake().tools;
        let mut unexpected_hook = authority_handshake();
        unexpected_hook
            .capabilities
            .event_hooks
            .push("persona_evolved".to_string());
        assert!(
            validate_handshake_against_package(&contribution_manifest, &unexpected_hook)
                .unwrap_err()
                .to_string()
                .contains("static contribution contract")
        );
    }

    #[test]
    fn empty_legacy_authority_declarations_remain_compatible() {
        let mut manifest = authority_manifest();
        manifest.provided_tools.clear();
        manifest.requested_capabilities.clear();
        manifest.declared_effects.clear();

        validate_handshake_against_package(&manifest, &authority_handshake())
            .expect("empty legacy declarations should remain compatible");
    }

    #[test]
    fn rpc_timeout_policy_bounds_every_method_class() {
        assert_eq!(
            rpc_timeout_for_method("plugin.get_prompt_contributions"),
            Duration::from_millis(250)
        );
        assert_eq!(
            rpc_timeout_for_method("plugin.invoke_tool"),
            Duration::from_secs(300)
        );
        assert_eq!(
            rpc_timeout_for_method("plugin.poll_events"),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn rpc_timeout_is_classified_as_transport_failure() {
        let error = anyhow::anyhow!(
            "Runtime plugin transport timeout during 'plugin.poll_events' after 10000ms"
        );
        assert!(RuntimePluginHost::is_transport_error(&error));
    }

    #[tokio::test]
    async fn rpc_timeout_bounds_a_stalled_transport_operation() {
        let error =
            await_rpc_with_timeout("plugin.poll_events", Duration::from_millis(10), async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<_, anyhow::Error>(Value::Null)
            })
            .await
            .expect_err("stalled transport should time out");

        assert!(RuntimePluginHost::is_transport_error(&error));
        assert!(error.to_string().contains("plugin.poll_events"));
    }

    #[tokio::test]
    async fn reconciliation_skips_unchanged_settings_and_reports_live_status() {
        let root = tempfile::tempdir().unwrap();
        write_runtime_fixture_package(root.path());
        let catalog = Arc::new(
            crate::runtime_process_plugin::RuntimeProcessPluginCatalog::discover_from_dir(
                root.path().to_path_buf(),
            )
            .unwrap(),
        );
        let host = Arc::new(RuntimePluginHost::with_catalog(catalog));
        let tools = Arc::new(ToolRegistry::new());
        let count_path = root.path().join("configure-count.txt");
        let mut config = fixture_config(&count_path, false);

        host.apply_config(&config, tools.clone()).await.unwrap();
        host.apply_config(&config, tools.clone()).await.unwrap();
        assert_eq!(fs::read_to_string(&count_path).unwrap().lines().count(), 1);
        assert!(tools.get("fixture.echo").await.is_some());

        let status = host.statuses().await.remove(0);
        assert_eq!(status.plugin_id, FIXTURE_PLUGIN_ID);
        assert_eq!(status.state, PluginRuntimeState::Running);
        assert_eq!(status.generation, 1);
        assert_eq!(status.restart_attempts, 0);
        assert!(status.process_id.is_some());
        assert_eq!(
            status.negotiated_protocol_version,
            Some(CURRENT_PLUGIN_PROTOCOL_VERSION)
        );
        assert!(status.last_started_at.is_some());

        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["revision"] = json!(2);
        host.apply_config(&config, tools.clone()).await.unwrap();
        assert_eq!(fs::read_to_string(&count_path).unwrap().lines().count(), 2);

        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["enabled"] = json!(false);
        host.apply_config(&config, tools.clone()).await.unwrap();
        assert_eq!(host.statuses().await[0].state, PluginRuntimeState::Disabled);
        assert!(tools.get("fixture.echo").await.is_none());
    }

    #[tokio::test]
    async fn rejected_reconfiguration_restarts_from_the_durable_boundary() {
        let root = tempfile::tempdir().unwrap();
        write_runtime_fixture_package(root.path());
        let catalog = Arc::new(
            crate::runtime_process_plugin::RuntimeProcessPluginCatalog::discover_from_dir(
                root.path().to_path_buf(),
            )
            .unwrap(),
        );
        let host = Arc::new(RuntimePluginHost::with_catalog(catalog));
        let tools = Arc::new(ToolRegistry::new());
        let count_path = root.path().join("configure-count.txt");
        let mut config = fixture_config(&count_path, false);

        host.apply_config(&config, tools.clone()).await.unwrap();
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["revision"] = json!(2);
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["reject_configure"] =
            json!(true);
        host.apply_config(&config, tools.clone()).await.unwrap();

        assert_ne!(host.statuses().await[0].state, PluginRuntimeState::Running);
        assert!(host.loaded.read().await.is_empty());
        assert!(tools.get("fixture.echo").await.is_none());

        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["revision"] = json!(3);
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["reject_configure"] =
            json!(false);
        host.apply_config(&config, tools.clone()).await.unwrap();

        assert_eq!(host.statuses().await[0].state, PluginRuntimeState::Running);
        assert_eq!(host.statuses().await[0].generation, 2);
        assert!(tools.get("fixture.echo").await.is_some());
    }

    #[tokio::test]
    async fn rejected_poll_response_resets_process_before_cursor_can_skip() {
        let root = tempfile::tempdir().unwrap();
        write_runtime_fixture_package(root.path());
        let catalog = Arc::new(
            crate::runtime_process_plugin::RuntimeProcessPluginCatalog::discover_from_dir(
                root.path().to_path_buf(),
            )
            .unwrap(),
        );
        let database = Arc::new(AgentDatabase::new(&root.path().join("plugin-events.db")).unwrap());
        let host = Arc::new(RuntimePluginHost::with_catalog_and_database(
            catalog,
            Some(database),
        ));
        let tools = Arc::new(ToolRegistry::new());
        let count_path = root.path().join("configure-count.txt");
        let mut config = fixture_config(&count_path, false);
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["poll_oversized_event"] =
            json!(true);

        host.apply_config(&config, tools.clone()).await.unwrap();
        let batch = host.poll_plugin_events().await.expect("poll batch");

        assert_eq!(batch.events.len(), 1);
        assert!(host.loaded.read().await.is_empty());
        assert!(tools.get("fixture.echo").await.is_none());
        assert!(host
            .state_snapshot(FIXTURE_PLUGIN_ID)
            .unwrap()
            .get("remote_cursor")
            .is_none());
    }

    #[tokio::test]
    async fn configure_keeps_settings_private_and_skips_undeclared_hook() {
        let root = tempfile::tempdir().unwrap();
        write_runtime_fixture_package(root.path());
        let catalog = Arc::new(
            crate::runtime_process_plugin::RuntimeProcessPluginCatalog::discover_from_dir(
                root.path().to_path_buf(),
            )
            .unwrap(),
        );
        let database =
            Arc::new(AgentDatabase::new(&root.path().join("plugin-events.db")).expect("database"));
        let host = Arc::new(RuntimePluginHost::with_catalog_and_database(
            catalog,
            Some(database.clone()),
        ));
        let tools = Arc::new(ToolRegistry::new());
        let count_path = root.path().join("configure-count.txt");
        let event_count_path = root.path().join("event-count.txt");
        let mut config = fixture_config(&count_path, false);
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["event_count_path"] =
            json!(event_count_path.to_string_lossy().to_string());

        host.apply_config(&config, tools.clone()).await.unwrap();
        host.dispatch_event(&RuntimePluginLifecycleEvent::SettingsChanged {
            plugin_id: FIXTURE_PLUGIN_ID.to_string(),
            settings: json!({"secret": "must-remain-scoped"}),
        })
        .await
        .expect("scoped settings dispatch");

        assert!(
            !event_count_path.exists(),
            "a plugin without the hook must not receive settings_changed"
        );
        let settings_delivery = database
            .prepare_plugin_event_delivery(
                "test.other-plugin",
                "host.lifecycle.settings_changed",
                10,
            )
            .expect("settings delivery query");
        assert!(
            settings_delivery.records.is_empty(),
            "plugin settings must not enter the shared lifecycle ledger"
        );

        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["enabled"] = json!(false);
        host.apply_config(&config, tools).await.unwrap();
    }

    #[tokio::test]
    async fn lifecycle_events_record_before_start_and_replay_after_enable() {
        let root = tempfile::tempdir().unwrap();
        write_runtime_fixture_package(root.path());
        let catalog = Arc::new(
            crate::runtime_process_plugin::RuntimeProcessPluginCatalog::discover_from_dir(
                root.path().to_path_buf(),
            )
            .unwrap(),
        );
        let database =
            Arc::new(AgentDatabase::new(&root.path().join("plugin-events.db")).expect("database"));
        let host = Arc::new(RuntimePluginHost::with_catalog_database_and_restart_policy(
            catalog,
            Some(database.clone()),
            PluginRestartPolicy::default(),
        ));
        let tools = Arc::new(ToolRegistry::new());
        let count_path = root.path().join("configure-count.txt");
        let event_count_path = root.path().join("event-count.txt");
        let mut config = fixture_config(&count_path, false);
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["event_count_path"] =
            json!(event_count_path.to_string_lossy().to_string());
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["enabled"] = json!(false);
        host.apply_config(&config, tools.clone()).await.unwrap();

        host.dispatch_event(&RuntimePluginLifecycleEvent::PersonaEvolved {
            current_self_description: "more patient".to_string(),
            previous_self_description: None,
            trajectory: Some("toward careful observation".to_string()),
            guiding_principles: vec!["notice before acting".to_string()],
        })
        .await
        .expect("record while disabled");
        host.dispatch_event(&RuntimePluginLifecycleEvent::PersonaEvolved {
            current_self_description: "more attentive".to_string(),
            previous_self_description: Some("more patient".to_string()),
            trajectory: Some("toward sustained observation".to_string()),
            guiding_principles: vec!["follow changes through time".to_string()],
        })
        .await
        .expect("record second event while disabled");
        assert!(database
            .get_plugin_event_cursor(FIXTURE_PLUGIN_ID, "host.lifecycle.persona_evolved")
            .unwrap()
            .is_none());

        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["enabled"] = json!(true);
        host.apply_config(&config, tools).await.unwrap();
        let cursor = database
            .get_plugin_event_cursor(FIXTURE_PLUGIN_ID, "host.lifecycle.persona_evolved")
            .unwrap()
            .expect("startup replay cursor");
        assert!(cursor.last_sequence > 0);
        assert_eq!(
            fs::read_to_string(event_count_path)
                .unwrap()
                .lines()
                .count(),
            2,
            "startup replay must drain each pending lifecycle receipt"
        );
        assert_eq!(host.statuses().await[0].state, PluginRuntimeState::Running);
    }

    #[tokio::test]
    async fn failed_lifecycle_ack_retries_through_supervised_restart() {
        let root = tempfile::tempdir().unwrap();
        write_runtime_fixture_package(root.path());
        let catalog = Arc::new(
            crate::runtime_process_plugin::RuntimeProcessPluginCatalog::discover_from_dir(
                root.path().to_path_buf(),
            )
            .unwrap(),
        );
        let database = Arc::new(AgentDatabase::new(&root.path().join("plugin-events.db")).unwrap());
        let host = Arc::new(RuntimePluginHost::with_catalog_and_database(
            catalog,
            Some(database.clone()),
        ));
        let tools = Arc::new(ToolRegistry::new());
        let count_path = root.path().join("configure-count.txt");
        let mut config = fixture_config(&count_path, false);
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["wrong_lifecycle_ack"] =
            json!(true);

        host.apply_config(&config, tools.clone()).await.unwrap();
        host.dispatch_event(&RuntimePluginLifecycleEvent::PersonaEvolved {
            current_self_description: "noticing failure".to_string(),
            previous_self_description: None,
            trajectory: None,
            guiding_principles: Vec::new(),
        })
        .await
        .expect("dispatch isolates plugin failure");

        assert!(host.loaded.read().await.is_empty());
        assert!(database
            .get_plugin_event_cursor(FIXTURE_PLUGIN_ID, "host.lifecycle.persona_evolved")
            .unwrap()
            .map_or(true, |cursor| cursor.last_sequence == 0));

        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["revision"] = json!(2);
        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["wrong_lifecycle_ack"] =
            json!(false);
        host.apply_config(&config, tools).await.unwrap();

        assert_eq!(host.statuses().await[0].state, PluginRuntimeState::Running);
        assert!(database
            .get_plugin_event_cursor(FIXTURE_PLUGIN_ID, "host.lifecycle.persona_evolved")
            .unwrap()
            .is_some_and(|cursor| cursor.last_sequence > 0));
    }

    #[tokio::test]
    async fn exited_process_restarts_after_policy_backoff() {
        let root = tempfile::tempdir().unwrap();
        write_runtime_fixture_package(root.path());
        let catalog = Arc::new(
            crate::runtime_process_plugin::RuntimeProcessPluginCatalog::discover_from_dir(
                root.path().to_path_buf(),
            )
            .unwrap(),
        );
        let policy = PluginRestartPolicy::new(
            Duration::from_millis(10),
            Duration::from_millis(20),
            3,
            Duration::from_millis(50),
            Duration::from_secs(1),
        )
        .unwrap();
        let host = Arc::new(RuntimePluginHost::with_catalog_and_restart_policy(
            catalog, policy,
        ));
        let tools = Arc::new(ToolRegistry::new());
        let count_path = root.path().join("restart-count.txt");
        let mut config = fixture_config(&count_path, true);

        host.apply_config(&config, tools.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        host.apply_config(&config, tools.clone()).await.unwrap();
        let backed_off = host.statuses().await.remove(0);
        assert_eq!(backed_off.state, PluginRuntimeState::Backoff);
        assert_eq!(backed_off.consecutive_failures, 1);
        assert!(backed_off.next_retry_at.is_some());

        tokio::time::sleep(Duration::from_millis(15)).await;
        host.apply_config(&config, tools.clone()).await.unwrap();
        let restarted = host.statuses().await.remove(0);
        assert_eq!(restarted.state, PluginRuntimeState::Running);
        assert_eq!(restarted.generation, 2);
        assert_eq!(restarted.restart_attempts, 1);
        assert_eq!(fs::read_to_string(&count_path).unwrap().lines().count(), 2);

        config.plugin_settings.get_mut(FIXTURE_PLUGIN_ID).unwrap()["enabled"] = json!(false);
        host.apply_config(&config, tools).await.unwrap();
    }

    #[tokio::test]
    async fn terminal_handshake_failure_waits_for_changed_input() {
        let root = tempfile::tempdir().unwrap();
        let package_id = "test.mismatched-package";
        write_runtime_fixture_package_with_id(root.path(), package_id);
        let catalog = Arc::new(
            crate::runtime_process_plugin::RuntimeProcessPluginCatalog::discover_from_dir(
                root.path().to_path_buf(),
            )
            .unwrap(),
        );
        let host = Arc::new(RuntimePluginHost::with_catalog(catalog));
        let tools = Arc::new(ToolRegistry::new());
        let mut config = AgentConfig::default();

        host.apply_config(&config, tools.clone()).await.unwrap();
        let failed = host.statuses().await.remove(0);
        assert_eq!(failed.state, PluginRuntimeState::Failed);
        assert_eq!(failed.generation, 1);
        assert!(failed
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("mismatched handshake id")));

        host.apply_config(&config, tools.clone()).await.unwrap();
        assert_eq!(host.statuses().await[0].generation, 1);

        config
            .plugin_settings
            .insert(package_id.to_string(), json!({"revision": 2}));
        host.apply_config(&config, tools).await.unwrap();
        let retried = host.statuses().await.remove(0);
        assert_eq!(retried.state, PluginRuntimeState::Failed);
        assert_eq!(retried.generation, 2);
    }
}
