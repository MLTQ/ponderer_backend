use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginEffectDeclaration {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginRuntimeState {
    #[default]
    Disabled,
    Unavailable,
    Starting,
    Running,
    Degraded,
    Backoff,
    CircuitOpen,
    Stopping,
    Stopped,
    Failed,
    Quarantined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRuntimeStatus {
    pub plugin_id: String,
    #[serde(default)]
    pub desired_enabled: bool,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub state: PluginRuntimeState,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub restart_attempts: u32,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub negotiated_protocol_version: Option<u32>,
    #[serde(default)]
    pub process_id: Option<u32>,
    #[serde(default)]
    pub state_changed_at: Option<String>,
    #[serde(default)]
    pub last_started_at: Option<String>,
    #[serde(default)]
    pub last_stopped_at: Option<String>,
    #[serde(default)]
    pub last_healthy_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_at: Option<String>,
    #[serde(default)]
    pub next_retry_at: Option<String>,
}
