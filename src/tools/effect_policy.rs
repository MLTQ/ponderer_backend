use crate::plugin_contract::PluginEffectDeclaration;

pub const EFFECT_NETWORK_READ: &str = "network.read";
pub const EFFECT_NETWORK_WRITE: &str = "network.write";
pub const EFFECT_FILESYSTEM_READ: &str = "filesystem.read";
pub const EFFECT_FILESYSTEM_WRITE: &str = "filesystem.write";
pub const EFFECT_PROCESS_EXECUTE: &str = "process.execute";
pub const EFFECT_EXTERNAL_PUBLISH: &str = "external.publish";
pub const EFFECT_CAMERA_CAPTURE: &str = "camera.capture";
pub const EFFECT_MICROPHONE_CAPTURE: &str = "microphone.capture";
pub const EFFECT_SECRETS_READ: &str = "secrets.read";
pub const EFFECT_IDENTITY_PROPOSE_CHANGE: &str = "identity.propose_change";
pub const EFFECT_PLUGIN_DRAFT_WRITE: &str = "plugin.draft.write";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ToolApprovalMinimum {
    #[default]
    None,
    Autonomous,
    Always,
}

impl ToolApprovalMinimum {
    pub fn requires_approval(self, autonomous: bool) -> bool {
        match self {
            Self::None => false,
            Self::Autonomous => autonomous,
            Self::Always => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolRateLimitClass {
    #[default]
    None,
    OutboundAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectiveToolPolicy {
    pub approval: ToolApprovalMinimum,
    pub rate_limit: ToolRateLimitClass,
    pub effects: Vec<String>,
}

impl EffectiveToolPolicy {
    pub fn requires_approval(&self, autonomous: bool) -> bool {
        self.approval.requires_approval(autonomous)
    }

    pub fn is_outbound_action(&self) -> bool {
        self.rate_limit == ToolRateLimitClass::OutboundAction
    }
}

pub fn resolve_tool_effect_policy(
    _tool_name: &str,
    declared_requires_approval: bool,
    declared_effects: &[PluginEffectDeclaration],
) -> EffectiveToolPolicy {
    let mut policy = EffectiveToolPolicy::default();

    if declared_requires_approval {
        strengthen_approval(&mut policy.approval, ToolApprovalMinimum::Autonomous);
    }

    for effect in declared_effects {
        let effect_id = normalize_effect_id(&effect.id);
        if effect_id.is_empty() {
            strengthen_approval(&mut policy.approval, ToolApprovalMinimum::Autonomous);
            continue;
        }
        if !policy.effects.iter().any(|existing| existing == &effect_id) {
            policy.effects.push(effect_id.clone());
        }

        let minimum = minimum_policy_for_effect(&effect_id);
        strengthen_approval(&mut policy.approval, minimum.approval);
        if minimum.rate_limit == ToolRateLimitClass::OutboundAction {
            policy.rate_limit = ToolRateLimitClass::OutboundAction;
        }

        // A plugin may ask for a stronger gate, but `false` never lowers the
        // host minimum associated with the semantic effect ID.
        if effect.requires_approval {
            strengthen_approval(&mut policy.approval, ToolApprovalMinimum::Autonomous);
        }
    }

    policy.effects.sort();
    policy
}

fn minimum_policy_for_effect(effect_id: &str) -> EffectiveToolPolicy {
    let (approval, rate_limit) = match effect_id {
        EFFECT_NETWORK_READ | EFFECT_FILESYSTEM_READ | EFFECT_PLUGIN_DRAFT_WRITE => {
            (ToolApprovalMinimum::None, ToolRateLimitClass::None)
        }
        EFFECT_EXTERNAL_PUBLISH => (
            ToolApprovalMinimum::Autonomous,
            ToolRateLimitClass::OutboundAction,
        ),
        EFFECT_IDENTITY_PROPOSE_CHANGE | EFFECT_SECRETS_READ => {
            (ToolApprovalMinimum::Always, ToolRateLimitClass::None)
        }
        EFFECT_NETWORK_WRITE
        | EFFECT_FILESYSTEM_WRITE
        | EFFECT_PROCESS_EXECUTE
        | EFFECT_CAMERA_CAPTURE
        | EFFECT_MICROPHONE_CAPTURE => (ToolApprovalMinimum::Autonomous, ToolRateLimitClass::None),
        // Until a capability registry explicitly classifies a new effect, it
        // is allowed only behind an operator-overridable approval boundary.
        _ => (ToolApprovalMinimum::Autonomous, ToolRateLimitClass::None),
    };

    EffectiveToolPolicy {
        approval,
        rate_limit,
        effects: vec![effect_id.to_string()],
    }
}

fn strengthen_approval(current: &mut ToolApprovalMinimum, minimum: ToolApprovalMinimum) {
    if minimum > *current {
        *current = minimum;
    }
}

fn normalize_effect_id(effect_id: &str) -> String {
    effect_id.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect(id: &str, requires_approval: bool) -> PluginEffectDeclaration {
        PluginEffectDeclaration {
            id: id.to_string(),
            description: None,
            requires_approval,
        }
    }

    #[test]
    fn external_publish_minimum_cannot_be_weakened() {
        let policy = resolve_tool_effect_policy(
            "invented_name",
            false,
            &[effect(EFFECT_EXTERNAL_PUBLISH, false)],
        );

        assert_eq!(policy.approval, ToolApprovalMinimum::Autonomous);
        assert_eq!(policy.rate_limit, ToolRateLimitClass::OutboundAction);
        assert!(policy.requires_approval(true));
        assert!(!policy.requires_approval(false));
    }

    #[test]
    fn declared_approval_can_only_strengthen_read_effects() {
        let policy =
            resolve_tool_effect_policy("reader", false, &[effect(EFFECT_NETWORK_READ, true)]);

        assert_eq!(policy.approval, ToolApprovalMinimum::Autonomous);
        assert_eq!(policy.rate_limit, ToolRateLimitClass::None);
    }

    #[test]
    fn most_restrictive_effect_wins_without_losing_rate_limit() {
        let policy = resolve_tool_effect_policy(
            "mixed",
            false,
            &[
                effect(EFFECT_EXTERNAL_PUBLISH, false),
                effect(EFFECT_IDENTITY_PROPOSE_CHANGE, false),
            ],
        );

        assert_eq!(policy.approval, ToolApprovalMinimum::Always);
        assert_eq!(policy.rate_limit, ToolRateLimitClass::OutboundAction);
        assert!(policy.requires_approval(false));
    }

    #[test]
    fn unknown_effects_are_conservative_and_normalized() {
        let policy = resolve_tool_effect_policy(
            "custom",
            false,
            &[effect("  Vendor.Custom_Action  ", false)],
        );

        assert_eq!(policy.approval, ToolApprovalMinimum::Autonomous);
        assert_eq!(policy.effects, vec!["vendor.custom_action"]);
    }

    #[test]
    fn malformed_empty_effect_cannot_remove_the_conservative_gate() {
        let policy = resolve_tool_effect_policy("custom", false, &[effect("   ", false)]);

        assert_eq!(policy.approval, ToolApprovalMinimum::Autonomous);
        assert!(policy.effects.is_empty());
    }

    #[test]
    fn confined_plugin_drafting_does_not_imply_execution_authority() {
        let policy = resolve_tool_effect_policy(
            "plugin_workbench",
            false,
            &[effect(EFFECT_PLUGIN_DRAFT_WRITE, false)],
        );

        assert_eq!(policy.approval, ToolApprovalMinimum::None);
        assert_eq!(policy.rate_limit, ToolRateLimitClass::None);
    }
}
