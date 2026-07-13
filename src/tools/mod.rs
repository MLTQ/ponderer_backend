//! Tool system for local capabilities (shell, files, HTTP, etc.)
//!
//! Tools are distinct from Skills:
//! - **Runtime plugins** are external integrations that contribute tools and durable events
//! - **Tools** are local capabilities the agent can invoke during reasoning (shell, file ops, HTTP)
//!
//! Each tool declares a JSON Schema for its parameters, enabling LLM function-calling.
//! Tools are registered in a thread-safe ToolRegistry that generates OpenAI-format
//! function definitions for the LLM.

pub mod agentic;
pub mod approval;
pub mod effect_policy;
pub mod files;
pub mod http;
pub mod memory;
pub mod plugin_workbench;
pub mod runtime_plugin;
pub mod safety;
pub mod scheduled_jobs;
pub mod shell;
pub mod vision;

pub use effect_policy::{EffectiveToolPolicy, ToolApprovalMinimum, ToolRateLimitClass};

use crate::plugin_contract::PluginEffectDeclaration;
use anyhow::Result;
use async_trait::async_trait;
use effect_policy::resolve_tool_effect_policy;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Category of tool — used for grouping in UI and applying approval policies
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCategory {
    /// File system operations (read, write, list, patch)
    FileSystem,
    /// Shell/command execution
    Shell,
    /// HTTP/network requests
    Network,
    /// Memory and knowledge management
    Memory,
    /// General purpose / uncategorized
    General,
}

/// The result of executing a tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolOutput {
    /// Successful text output
    Text(String),
    /// Successful structured output
    Json(serde_json::Value),
    /// Tool execution failed
    Error(String),
    /// Tool needs user approval before proceeding
    NeedsApproval {
        tool: String,
        params: serde_json::Value,
        reason: String,
    },
}

impl ToolOutput {
    /// Convert to a string representation suitable for feeding back to the LLM
    pub fn to_llm_string(&self) -> String {
        match self {
            ToolOutput::Text(s) => s.clone(),
            ToolOutput::Json(v) => {
                serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
            }
            ToolOutput::Error(e) => format!("[ERROR] {}", e),
            ToolOutput::NeedsApproval { tool, reason, .. } => {
                format!("[NEEDS APPROVAL] Tool '{}': {}", tool, reason)
            }
        }
    }

    /// Returns true if this output represents success (Text or Json)
    pub fn is_success(&self) -> bool {
        matches!(self, ToolOutput::Text(_) | ToolOutput::Json(_))
    }
}

/// Context passed to tools during execution
pub struct ToolContext {
    /// Current working directory for file/shell operations
    pub working_directory: String,
    /// The agent's username (for attribution)
    pub username: String,
    /// Conversation scope for tools that persist resumable private state.
    pub conversation_id: Option<String>,
    /// Whether the tool is running in autonomous mode (vs interactive with user present)
    pub autonomous: bool,
    /// If set, only these tool names are callable in this context (case-insensitive)
    pub allowed_tools: Option<Vec<String>>,
    /// Tool names that are not callable in this context (case-insensitive)
    pub disallowed_tools: Vec<String>,
    /// Optional shared rolling limiter for side-effecting tool invocations.
    /// The registry reserves quota immediately before execution so a single
    /// multi-call pass cannot race or overshoot a context-level visibility check.
    pub outbound_action_rate_limit: Option<Arc<ToolInvocationRateLimit>>,
}

impl ToolContext {
    pub fn allows_tool(&self, tool_name: &str) -> bool {
        if self
            .disallowed_tools
            .iter()
            .any(|name| name.eq_ignore_ascii_case(tool_name))
        {
            return false;
        }

        match &self.allowed_tools {
            Some(allowed) => allowed
                .iter()
                .any(|name| name.eq_ignore_ascii_case(tool_name)),
            None => true,
        }
    }
}

#[derive(Debug)]
struct ToolRateLimitEntry {
    reserved_at: Instant,
}

/// A process-wide rolling limiter shared by every autonomous tool context.
/// Reservations count from the moment a call is dispatched. They remain in the
/// window even when the result is an error because a timeout or lost response
/// cannot prove that a remote side effect did not occur. This closes concurrent-
/// pass and same-pass quota overshoot without pushing policy into plugins.
#[derive(Debug)]
pub struct ToolInvocationRateLimit {
    limited_tools: HashSet<String>,
    max_actions: AtomicU32,
    window: Duration,
    entries: StdMutex<VecDeque<ToolRateLimitEntry>>,
}

impl ToolInvocationRateLimit {
    /// Legacy constructor that also recognizes a fixed list of historical tool
    /// names. Semantic `OutboundAction` policies are enforced independently.
    pub fn new(limited_tools: &[&str], max_actions: u32, window: Duration) -> Self {
        Self {
            limited_tools: limited_tools
                .iter()
                .map(|name| name.trim().to_ascii_lowercase())
                .filter(|name| !name.is_empty())
                .collect(),
            max_actions: AtomicU32::new(max_actions),
            window,
            entries: StdMutex::new(VecDeque::new()),
        }
    }

    /// Construct a limiter driven entirely by semantic tool effects.
    pub fn for_outbound_effects(max_actions: u32, window: Duration) -> Self {
        Self::new(&[], max_actions, window)
    }

    pub fn set_max_actions(&self, max_actions: u32) {
        self.max_actions.store(max_actions, Ordering::SeqCst);
    }

    pub fn active_count(&self) -> u32 {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.prune_locked(&mut entries, Instant::now());
        entries.len().try_into().unwrap_or(u32::MAX)
    }

    fn try_reserve(
        &self,
        tool_name: &str,
        rate_limit: ToolRateLimitClass,
    ) -> std::result::Result<(), ()> {
        let selected_by_legacy_name = self
            .limited_tools
            .contains(&tool_name.trim().to_ascii_lowercase());
        if rate_limit != ToolRateLimitClass::OutboundAction && !selected_by_legacy_name {
            return Ok(());
        }
        let max_actions = self.max_actions.load(Ordering::SeqCst);
        if max_actions == 0 {
            return Err(());
        }

        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.prune_locked(&mut entries, now);
        if entries.len() >= max_actions as usize {
            return Err(());
        }

        entries.push_back(ToolRateLimitEntry { reserved_at: now });
        Ok(())
    }

    fn prune_locked(&self, entries: &mut VecDeque<ToolRateLimitEntry>, now: Instant) {
        while entries
            .front()
            .is_some_and(|entry| now.duration_since(entry.reserved_at) >= self.window)
        {
            entries.pop_front();
        }
    }
}

/// A tool provides the agent with a local capability.
///
/// Unlike Skills (which poll external services), Tools are invoked on-demand
/// by the agent during its reasoning loop. Each tool declares its parameters
/// as a JSON Schema, enabling LLM function-calling.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name used in function-calling (e.g., "shell", "read_file")
    fn name(&self) -> &str;

    /// Human-readable description shown to the LLM
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's parameters.
    ///
    /// This is used directly in OpenAI-format function definitions.
    /// Example:
    /// ```json
    /// {
    ///   "type": "object",
    ///   "properties": {
    ///     "command": { "type": "string", "description": "Shell command to execute" }
    ///   },
    ///   "required": ["command"]
    /// }
    /// ```
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given parameters.
    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput>;

    /// Whether this tool requires user approval before execution.
    ///
    /// Tools that modify the filesystem, execute commands, or make network
    /// requests should return true. Read-only tools can return false.
    fn requires_approval(&self) -> bool {
        false
    }

    /// Semantic effects caused by this tool. Runtime plugins provide these
    /// through their handshake manifest; legacy built-ins may return none.
    fn effects(&self) -> &[PluginEffectDeclaration] {
        &[]
    }

    /// Host-resolved minimum policy. Effect rules are monotonically joined
    /// with the legacy approval flag, so plugin metadata cannot lower them.
    fn effect_policy(&self) -> EffectiveToolPolicy {
        resolve_tool_effect_policy(self.name(), self.requires_approval(), self.effects())
    }

    /// Stable provider identity for authorization diagnostics and binding.
    ///
    /// The registry also assigns every registration a unique generation, so
    /// built-ins can safely share this default while runtime providers should
    /// override it with their package/version/process-generation identity.
    fn authorization_provider(&self) -> &str {
        "host.builtin"
    }

    /// Category for grouping and policy application
    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }
}

/// OpenAI-format function definition for LLM function-calling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// OpenAI-format tool definition (wraps FunctionDef)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

/// A tool call parsed from LLM output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Result of a tool call, ready to feed back to the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub name: String,
    pub output: ToolOutput,
}

/// Thread-safe registry of tools available to the agent.
///
/// The registry provides:
/// - Tool lookup by name
/// - Generation of OpenAI-format function definitions for LLM prompts
/// - Dynamic registration/deregistration of tools
#[derive(Clone)]
struct RegisteredTool {
    tool: Arc<dyn Tool>,
    authorization_fingerprint: ToolAuthorizationFingerprint,
    effect_policy: EffectiveToolPolicy,
}

/// Complete authorization identity captured when a tool enters the registry.
///
/// `registration_generation` makes replacement identity unambiguous even when
/// a new provider deliberately reuses the same name, schema, and effect policy.
/// The remaining fields make the bound contract inspectable and ensure a grant
/// represents the complete contract presented when approval was given.
#[derive(Debug, Clone, PartialEq)]
struct ToolAuthorizationFingerprint {
    registration_generation: u64,
    provider: String,
    name: String,
    description: String,
    parameters: serde_json::Value,
    category: ToolCategory,
    effects: Vec<PluginEffectDeclaration>,
    effect_policy: EffectiveToolPolicy,
}

#[derive(Default)]
struct ToolRegistryState {
    tools: HashMap<String, RegisteredTool>,
    session_approved: HashMap<String, ToolAuthorizationFingerprint>,
}

pub struct ToolRegistry {
    /// Tools and approvals share one lock so registration, grant, and lookup
    /// observe one coherent authorization generation.
    state: RwLock<ToolRegistryState>,
    next_registration_generation: AtomicU64,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(ToolRegistryState::default()),
            next_registration_generation: AtomicU64::new(1),
        }
    }

    /// Grant session-level approval for a tool so it runs without prompting for the rest of the session.
    pub async fn grant_session_approval(&self, tool_name: &str) {
        let mut state = self.state.write().await;
        let Some(fingerprint) = state
            .tools
            .get(tool_name)
            .map(|registered| registered.authorization_fingerprint.clone())
        else {
            tracing::warn!("Ignored session approval for unknown tool: {}", tool_name);
            return;
        };
        state
            .session_approved
            .insert(tool_name.to_string(), fingerprint);
        tracing::info!("Session approval granted for tool: {}", tool_name);
    }

    /// Register a tool. Overwrites any existing tool with the same name.
    pub async fn register(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        let effect_policy = tool.effect_policy();
        let registration_generation = self
            .next_registration_generation
            .fetch_add(1, Ordering::Relaxed);
        let authorization_fingerprint = ToolAuthorizationFingerprint {
            registration_generation,
            provider: tool.authorization_provider().to_string(),
            name: name.clone(),
            description: tool.description().to_string(),
            parameters: tool.parameters_schema(),
            category: tool.category(),
            effects: tool.effects().to_vec(),
            effect_policy: effect_policy.clone(),
        };
        tracing::info!(
            "Registered tool: {} (provider: {}, generation: {}, category: {:?})",
            name,
            authorization_fingerprint.provider,
            registration_generation,
            tool.category()
        );
        let mut state = self.state.write().await;
        // A grant authorizes one exact registration, never whatever happens to
        // occupy the name later. Clear it in the same critical section as the
        // replacement so no caller can observe a new tool with an old grant.
        state.session_approved.remove(&name);
        state.tools.insert(
            name,
            RegisteredTool {
                tool,
                authorization_fingerprint,
                effect_policy,
            },
        );
    }

    /// Remove a tool by name.
    pub async fn deregister(&self, name: &str) -> bool {
        let mut state = self.state.write().await;
        state.session_approved.remove(name);
        state.tools.remove(name).is_some()
    }

    /// Get a tool by name.
    pub async fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.state
            .read()
            .await
            .tools
            .get(name)
            .map(|registered| Arc::clone(&registered.tool))
    }

    /// List all registered tool names.
    pub async fn list_names(&self) -> Vec<String> {
        self.state.read().await.tools.keys().cloned().collect()
    }

    /// Return the host-resolved effect policy for a registered tool.
    pub async fn tool_effect_policy(&self, name: &str) -> Option<EffectiveToolPolicy> {
        self.state
            .read()
            .await
            .tools
            .get(name)
            .map(|registered| registered.effect_policy.clone())
    }

    /// Generate OpenAI-format tool definitions for all registered tools.
    ///
    /// This output can be passed directly to the `tools` parameter
    /// of an OpenAI-compatible chat completions request.
    pub async fn tool_definitions(&self) -> Vec<ToolDef> {
        let state = self.state.read().await;
        state
            .tools
            .values()
            .map(|registered| ToolDef {
                tool_type: "function".to_string(),
                function: FunctionDef {
                    name: registered.tool.name().to_string(),
                    description: registered.tool.description().to_string(),
                    parameters: registered.tool.parameters_schema(),
                },
            })
            .collect()
    }

    /// Generate tool definitions filtered by execution context policy.
    pub async fn tool_definitions_for_context(&self, ctx: &ToolContext) -> Vec<ToolDef> {
        let state = self.state.read().await;
        state
            .tools
            .values()
            .filter(|registered| ctx.allows_tool(registered.tool.name()))
            .map(|registered| ToolDef {
                tool_type: "function".to_string(),
                function: FunctionDef {
                    name: registered.tool.name().to_string(),
                    description: registered.tool.description().to_string(),
                    parameters: registered.tool.parameters_schema(),
                },
            })
            .collect()
    }

    /// Execute a tool call, handling approval checks.
    ///
    /// Returns `ToolOutput::NeedsApproval` if the tool requires approval
    /// and the context indicates autonomous mode.
    pub async fn execute_call(&self, call: &ToolCall, ctx: &ToolContext) -> ToolCallResult {
        if !ctx.allows_tool(&call.name) {
            return ToolCallResult {
                name: call.name.clone(),
                output: ToolOutput::Error(format!(
                    "Tool '{}' is disabled for this context",
                    call.name
                )),
            };
        }

        let (tool, effect_policy, session_ok) = {
            let state = self.state.read().await;
            let Some(registered) = state.tools.get(&call.name) else {
                return ToolCallResult {
                    name: call.name.clone(),
                    output: ToolOutput::Error(format!("Unknown tool: {}", call.name)),
                };
            };
            let session_ok = state
                .session_approved
                .get(&call.name)
                .is_some_and(|approved| approved == &registered.authorization_fingerprint);
            (
                Arc::clone(&registered.tool),
                registered.effect_policy.clone(),
                session_ok,
            )
        };

        // Check the host minimum (skip only if the user granted a session approval).
        if effect_policy.requires_approval(ctx.autonomous) && !session_ok {
            let scope = match effect_policy.approval {
                ToolApprovalMinimum::Always => "for this effect",
                ToolApprovalMinimum::Autonomous => "in autonomous mode",
                ToolApprovalMinimum::None => "by policy",
            };
            return ToolCallResult {
                name: call.name.clone(),
                output: ToolOutput::NeedsApproval {
                    tool: call.name.clone(),
                    params: call.arguments.clone(),
                    reason: format!("Tool '{}' requires approval {}", call.name, scope),
                },
            };
        }

        if let Some(limit) = ctx.outbound_action_rate_limit.as_ref() {
            match limit.try_reserve(&call.name, effect_policy.rate_limit) {
                Ok(()) => {}
                Err(()) => {
                    let message = format!(
                        "Tool '{}' is temporarily disabled by the rolling outbound-action limit",
                        call.name
                    );
                    return ToolCallResult {
                        name: call.name.clone(),
                        output: ToolOutput::Error(message),
                    };
                }
            }
        }

        // Execute after reserving. A failed/ambiguous response keeps its slot:
        // only the remote system can know whether dispatch caused a side effect.
        let output = match tool.execute(call.arguments.clone(), ctx).await {
            Ok(output) => output,
            Err(e) => ToolOutput::Error(format!("Tool execution failed: {}", e)),
        };
        ToolCallResult {
            name: call.name.clone(),
            output,
        }
    }

    /// Execute multiple tool calls sequentially.
    pub async fn execute_calls(
        &self,
        calls: &[ToolCall],
        ctx: &ToolContext,
    ) -> Vec<ToolCallResult> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(self.execute_call(call, ctx).await);
        }
        results
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echoes back the input message"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to echo"
                    }
                },
                "required": ["message"]
            })
        }

        async fn execute(
            &self,
            params: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput> {
            let message = params["message"].as_str().unwrap_or("(no message)");
            Ok(ToolOutput::Text(message.to_string()))
        }
    }

    struct DangerousTool;

    struct FailingTool;

    struct SemanticPublishTool {
        effects: Vec<PluginEffectDeclaration>,
        provider: &'static str,
    }

    impl SemanticPublishTool {
        fn new(effect_id: &str) -> Self {
            Self {
                effects: vec![PluginEffectDeclaration {
                    id: effect_id.to_string(),
                    description: None,
                    requires_approval: false,
                }],
                provider: "host.builtin",
            }
        }

        fn from_provider(effect_id: &str, provider: &'static str) -> Self {
            Self {
                provider,
                ..Self::new(effect_id)
            }
        }
    }

    #[async_trait]
    impl Tool for SemanticPublishTool {
        fn name(&self) -> &str {
            "semantic_publish"
        }

        fn description(&self) -> &str {
            "Publishes through a semantic effect fixture"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn execute(
            &self,
            _params: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput> {
            Ok(ToolOutput::Text("published".to_string()))
        }

        fn effects(&self) -> &[PluginEffectDeclaration] {
            &self.effects
        }

        fn authorization_provider(&self) -> &str {
            self.provider
        }
    }

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            "failing"
        }

        fn description(&self) -> &str {
            "Always returns a structured failure"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn execute(
            &self,
            _params: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput> {
            Ok(ToolOutput::Error("deliberate failure".to_string()))
        }
    }

    #[async_trait]
    impl Tool for DangerousTool {
        fn name(&self) -> &str {
            "dangerous"
        }

        fn description(&self) -> &str {
            "A tool that requires approval"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {},
            })
        }

        async fn execute(
            &self,
            _params: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput> {
            Ok(ToolOutput::Text("executed".to_string()))
        }

        fn requires_approval(&self) -> bool {
            true
        }

        fn category(&self) -> ToolCategory {
            ToolCategory::Shell
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_directory: "/tmp".to_string(),
            username: "test".to_string(),
            conversation_id: None,
            autonomous: false,
            allowed_tools: None,
            disallowed_tools: Vec::new(),
            outbound_action_rate_limit: None,
        }
    }

    #[tokio::test]
    async fn test_registry_register_and_get() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).await;

        assert!(registry.get("echo").await.is_some());
        assert!(registry.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn test_tool_definitions_format() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).await;

        let defs = registry.tool_definitions().await;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].tool_type, "function");
        assert_eq!(defs[0].function.name, "echo");

        // Should be valid JSON that can be serialized
        let json = serde_json::to_string(&defs).unwrap();
        assert!(json.contains("echo"));
    }

    #[tokio::test]
    async fn test_execute_echo_tool() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).await;

        let call = ToolCall {
            name: "echo".to_string(),
            arguments: serde_json::json!({"message": "hello"}),
        };

        let result = registry.execute_call(&call, &test_ctx()).await;
        assert_eq!(result.name, "echo");
        assert!(result.output.is_success());
        assert_eq!(result.output.to_llm_string(), "hello");
    }

    #[tokio::test]
    async fn test_unknown_tool_returns_error() {
        let registry = ToolRegistry::new();

        let call = ToolCall {
            name: "nonexistent".to_string(),
            arguments: serde_json::json!({}),
        };

        let result = registry.execute_call(&call, &test_ctx()).await;
        assert!(!result.output.is_success());
        assert!(result.output.to_llm_string().contains("Unknown tool"));
    }

    #[tokio::test]
    async fn test_approval_required_in_autonomous_mode() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(DangerousTool)).await;

        let call = ToolCall {
            name: "dangerous".to_string(),
            arguments: serde_json::json!({}),
        };

        // In autonomous mode, should need approval
        let mut ctx = test_ctx();
        ctx.autonomous = true;
        let result = registry.execute_call(&call, &ctx).await;
        assert!(matches!(result.output, ToolOutput::NeedsApproval { .. }));

        // In interactive mode, should execute normally
        let mut ctx = test_ctx();
        ctx.autonomous = false;
        let result = registry.execute_call(&call, &ctx).await;
        assert!(result.output.is_success());
    }

    #[tokio::test]
    async fn semantic_publish_effect_enforces_approval_and_rate_limit_by_effect() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(SemanticPublishTool::new(
                effect_policy::EFFECT_EXTERNAL_PUBLISH,
            )))
            .await;
        let mut ctx = test_ctx();
        ctx.autonomous = true;
        ctx.outbound_action_rate_limit = Some(Arc::new(
            ToolInvocationRateLimit::for_outbound_effects(1, Duration::from_secs(60)),
        ));
        let call = ToolCall {
            name: "semantic_publish".to_string(),
            arguments: serde_json::json!({}),
        };

        let approval = registry.execute_call(&call, &ctx).await;
        assert!(matches!(approval.output, ToolOutput::NeedsApproval { .. }));

        registry.grant_session_approval("semantic_publish").await;
        assert!(registry.execute_call(&call, &ctx).await.output.is_success());
        let blocked = registry.execute_call(&call, &ctx).await;
        assert!(blocked
            .output
            .to_llm_string()
            .contains("rolling outbound-action limit"));

        let policy = registry
            .tool_effect_policy("semantic_publish")
            .await
            .expect("registered tool should expose policy");
        assert!(policy.is_outbound_action());
    }

    #[tokio::test]
    async fn zero_outbound_quota_disables_semantic_publish_tools() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(SemanticPublishTool::new(
                effect_policy::EFFECT_EXTERNAL_PUBLISH,
            )))
            .await;
        registry.grant_session_approval("semantic_publish").await;
        let mut ctx = test_ctx();
        ctx.autonomous = true;
        ctx.outbound_action_rate_limit = Some(Arc::new(
            ToolInvocationRateLimit::for_outbound_effects(0, Duration::from_secs(60)),
        ));
        let result = registry
            .execute_call(
                &ToolCall {
                    name: "semantic_publish".to_string(),
                    arguments: serde_json::json!({}),
                },
                &ctx,
            )
            .await;
        assert!(result
            .output
            .to_llm_string()
            .contains("rolling outbound-action limit"));
    }

    #[tokio::test]
    async fn always_approval_effect_is_enforced_in_interactive_context() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(SemanticPublishTool::new(
                effect_policy::EFFECT_IDENTITY_PROPOSE_CHANGE,
            )))
            .await;
        let call = ToolCall {
            name: "semantic_publish".to_string(),
            arguments: serde_json::json!({}),
        };

        let result = registry.execute_call(&call, &test_ctx()).await;
        assert!(matches!(result.output, ToolOutput::NeedsApproval { .. }));
    }

    #[tokio::test]
    async fn session_approval_is_invalidated_when_effect_policy_changes() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(SemanticPublishTool::new(
                effect_policy::EFFECT_EXTERNAL_PUBLISH,
            )))
            .await;
        registry.grant_session_approval("semantic_publish").await;

        registry
            .register(Arc::new(SemanticPublishTool::new(
                effect_policy::EFFECT_IDENTITY_PROPOSE_CHANGE,
            )))
            .await;
        let call = ToolCall {
            name: "semantic_publish".to_string(),
            arguments: serde_json::json!({}),
        };

        let result = registry.execute_call(&call, &test_ctx()).await;
        assert!(matches!(result.output, ToolOutput::NeedsApproval { .. }));
    }

    #[tokio::test]
    async fn session_approval_does_not_cross_same_contract_provider_replacement() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(SemanticPublishTool::from_provider(
                effect_policy::EFFECT_EXTERNAL_PUBLISH,
                "runtime_plugin:first@1.0.0#generation:1",
            )))
            .await;
        registry.grant_session_approval("semantic_publish").await;

        registry
            .register(Arc::new(SemanticPublishTool::from_provider(
                effect_policy::EFFECT_EXTERNAL_PUBLISH,
                "runtime_plugin:replacement@1.0.0#generation:1",
            )))
            .await;
        let mut ctx = test_ctx();
        ctx.autonomous = true;
        let call = ToolCall {
            name: "semantic_publish".to_string(),
            arguments: serde_json::json!({}),
        };

        let result = registry.execute_call(&call, &ctx).await;
        assert!(matches!(result.output, ToolOutput::NeedsApproval { .. }));
    }

    #[tokio::test]
    async fn deregister_then_reregister_clears_exact_instance_approval() {
        let registry = ToolRegistry::new();
        let tool: Arc<dyn Tool> = Arc::new(SemanticPublishTool::new(
            effect_policy::EFFECT_EXTERNAL_PUBLISH,
        ));
        registry.register(Arc::clone(&tool)).await;
        registry.grant_session_approval("semantic_publish").await;

        let mut ctx = test_ctx();
        ctx.autonomous = true;
        let call = ToolCall {
            name: "semantic_publish".to_string(),
            arguments: serde_json::json!({}),
        };
        assert!(registry.execute_call(&call, &ctx).await.output.is_success());

        assert!(registry.deregister("semantic_publish").await);
        registry.register(tool).await;
        let result = registry.execute_call(&call, &ctx).await;
        assert!(matches!(result.output, ToolOutput::NeedsApproval { .. }));
    }

    #[tokio::test]
    async fn rolling_limit_is_reserved_per_successful_invocation() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).await;
        let mut ctx = test_ctx();
        ctx.autonomous = true;
        ctx.outbound_action_rate_limit = Some(Arc::new(ToolInvocationRateLimit::new(
            &["echo"],
            1,
            Duration::from_secs(60),
        )));
        let call = ToolCall {
            name: "echo".to_string(),
            arguments: serde_json::json!({"message": "hello"}),
        };

        assert!(registry.execute_call(&call, &ctx).await.output.is_success());
        let blocked = registry.execute_call(&call, &ctx).await;
        assert!(!blocked.output.is_success());
        assert!(blocked
            .output
            .to_llm_string()
            .contains("rolling outbound-action limit"));
    }

    #[tokio::test]
    async fn ambiguous_failure_retains_rolling_reservation() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(FailingTool)).await;
        let limiter = Arc::new(ToolInvocationRateLimit::new(
            &["failing"],
            1,
            Duration::from_secs(60),
        ));
        let mut ctx = test_ctx();
        ctx.autonomous = true;
        ctx.outbound_action_rate_limit = Some(Arc::clone(&limiter));
        let call = ToolCall {
            name: "failing".to_string(),
            arguments: serde_json::json!({}),
        };

        assert!(!registry.execute_call(&call, &ctx).await.output.is_success());
        assert_eq!(limiter.active_count(), 1);
        let retry = registry.execute_call(&call, &ctx).await;
        assert!(retry
            .output
            .to_llm_string()
            .contains("rolling outbound-action limit"));
    }

    #[tokio::test]
    async fn test_deregister() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).await;
        assert!(registry.get("echo").await.is_some());

        registry.deregister("echo").await;
        assert!(registry.get("echo").await.is_none());
    }

    #[tokio::test]
    async fn test_context_tool_allowlist_blocks_other_tools() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool)).await;
        registry.register(Arc::new(DangerousTool)).await;

        let call = ToolCall {
            name: "dangerous".to_string(),
            arguments: serde_json::json!({}),
        };

        let mut ctx = test_ctx();
        ctx.allowed_tools = Some(vec!["echo".to_string()]);

        let result = registry.execute_call(&call, &ctx).await;
        assert!(matches!(result.output, ToolOutput::Error(_)));
        assert!(result.output.to_llm_string().contains("disabled"));

        let defs = registry.tool_definitions_for_context(&ctx).await;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].function.name, "echo");
    }
}
