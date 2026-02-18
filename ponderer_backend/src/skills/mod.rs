pub mod graphchan;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// An event produced by a skill during polling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SkillEvent {
    /// New content arrived (e.g., forum posts, messages, notifications)
    NewContent {
        /// Unique ID for deduplication
        id: String,
        /// Human-readable source (e.g., thread title, channel name)
        source: String,
        /// Who produced this content
        author: String,
        /// The content body
        body: String,
        /// Optional parent/context IDs for threading
        parent_ids: Vec<String>,
    },
}

/// The result of executing a skill action
#[derive(Debug, Clone)]
pub enum SkillResult {
    /// Action completed successfully
    Success { message: String },
    /// Action failed
    Error { message: String },
}

/// Context passed to skills during polling
pub struct SkillContext {
    pub username: String,
}

/// A skill provides the agent with a capability to interact with an external system.
#[async_trait]
pub trait Skill: Send + Sync {
    /// Human-readable name for this skill
    fn name(&self) -> &str;

    /// Description of what this skill does (shown to agent in prompts)
    fn description(&self) -> &str;

    /// Run the skill's main poll iteration (called each agent cycle).
    /// Returns new events since last poll.
    async fn poll(&self, ctx: &SkillContext) -> Result<Vec<SkillEvent>>;

    /// Execute a specific action requested by the agent.
    /// `action` is the action name, `params` contains action-specific data.
    async fn execute(&self, action: &str, params: &serde_json::Value) -> Result<SkillResult>;

    /// List the actions this skill supports (for prompt generation)
    fn available_actions(&self) -> Vec<SkillActionDef>;
}

/// Describes an action a skill can perform (used in prompt generation)
#[derive(Debug, Clone)]
pub struct SkillActionDef {
    pub name: String,
    pub description: String,
    pub params_description: String,
}
