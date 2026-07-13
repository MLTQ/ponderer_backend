use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::PluginStateMutation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
pub enum PromptContributionSlot {
    #[serde(rename = "engaged.instructions", alias = "engaged_instructions")]
    EngagedInstructions,
    #[serde(rename = "engaged.context", alias = "engaged_context")]
    EngagedContext,
    #[serde(rename = "ambient.instructions", alias = "ambient_instructions")]
    AmbientInstructions,
    #[serde(rename = "orientation.context", alias = "orientation_context")]
    OrientationContext,
    #[serde(
        rename = "reflection.considerations",
        alias = "reflection_considerations"
    )]
    ReflectionConsiderations,
    #[serde(
        rename = "persona_evolution.considerations",
        alias = "persona_evolution_considerations"
    )]
    PersonaEvolutionConsiderations,
}

impl PromptContributionSlot {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::EngagedInstructions => "engaged.instructions",
            Self::EngagedContext => "engaged.context",
            Self::AmbientInstructions => "ambient.instructions",
            Self::OrientationContext => "orientation.context",
            Self::ReflectionConsiderations => "reflection.considerations",
            Self::PersonaEvolutionConsiderations => "persona_evolution.considerations",
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
            Self::Instruction => "instruction",
            Self::Context => "context",
            Self::Constraint => "constraint",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePluginPromptQuery {
    pub slot: PromptContributionSlot,
    #[serde(default)]
    pub context: PromptContributionContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RuntimePluginPromptResponse {
    #[serde(default)]
    pub contributions: Vec<PromptContribution>,
    #[serde(default)]
    pub state_updates: Vec<PluginStateMutation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
            Self::PersonaEvolved { .. } => "persona_evolved",
            Self::OrientationUpdated { .. } => "orientation_updated",
            Self::MessageFinalized { .. } => "message_finalized",
            Self::ReflectionCompleted { .. } => "reflection_completed",
            Self::SettingsChanged { .. } => "settings_changed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RuntimePluginEventAck {
    #[serde(default)]
    pub state_changed: bool,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub acknowledged_event_id: Option<String>,
    #[serde(default)]
    pub state_updates: Vec<PluginStateMutation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePluginEventEffect {
    pub plugin_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePluginPollEvent {
    pub id: String,
    pub source: String,
    pub author: String,
    pub body: String,
    #[serde(default)]
    pub parent_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RuntimePluginPollResponse {
    #[serde(default)]
    pub events: Vec<RuntimePluginPollEvent>,
    #[serde(default)]
    pub state_updates: Vec<PluginStateMutation>,
}

fn default_contribution_max_chars() -> usize {
    240
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_slots_emit_canonical_dotted_names() {
        let serialized = serde_json::to_string(&PromptContributionSlot::EngagedInstructions)
            .expect("slot should serialize");
        assert_eq!(serialized, "\"engaged.instructions\"");
    }

    #[test]
    fn prompt_slots_accept_legacy_snake_case_names() {
        let slot: PromptContributionSlot =
            serde_json::from_str("\"engaged_instructions\"").expect("legacy slot should decode");
        assert_eq!(slot, PromptContributionSlot::EngagedInstructions);
    }
}
