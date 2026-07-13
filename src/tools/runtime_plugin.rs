use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::plugin_contract::PluginEffectDeclaration;
use crate::runtime_plugin_host::{RuntimePluginHost, RuntimePluginToolManifest};

use super::effect_policy::resolve_tool_effect_policy;
use super::{
    EffectiveToolPolicy, Tool, ToolApprovalMinimum, ToolCategory, ToolContext, ToolOutput,
};

pub struct RuntimePluginToolProxy {
    plugin_id: String,
    authorization_provider: String,
    manifest: RuntimePluginToolManifest,
    host: Arc<RuntimePluginHost>,
}

impl RuntimePluginToolProxy {
    pub fn new(
        plugin_id: impl Into<String>,
        plugin_version: &str,
        plugin_generation: u64,
        manifest: RuntimePluginToolManifest,
        host: Arc<RuntimePluginHost>,
    ) -> Self {
        let plugin_id = plugin_id.into();
        Self {
            authorization_provider: format!(
                "runtime_plugin:{plugin_id}@{plugin_version}#generation:{plugin_generation}"
            ),
            plugin_id,
            manifest,
            host,
        }
    }
}

#[async_trait]
impl Tool for RuntimePluginToolProxy {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn description(&self) -> &str {
        &self.manifest.description
    }

    fn parameters_schema(&self) -> Value {
        self.manifest.parameters.clone()
    }

    async fn execute(&self, params: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        self.host
            .invoke_tool(&self.plugin_id, &self.manifest.name, params, ctx)
            .await
    }

    fn requires_approval(&self) -> bool {
        self.effect_policy().approval != ToolApprovalMinimum::None
    }

    fn effects(&self) -> &[PluginEffectDeclaration] {
        &self.manifest.effects
    }

    fn effect_policy(&self) -> EffectiveToolPolicy {
        resolve_tool_effect_policy(
            &self.manifest.name,
            self.manifest.requires_approval,
            &self.manifest.effects,
        )
    }

    fn authorization_provider(&self) -> &str {
        &self.authorization_provider
    }

    fn category(&self) -> ToolCategory {
        self.manifest.category.as_tool_category()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_contract::RuntimePluginToolCategory;
    use crate::tools::effect_policy::{ToolRateLimitClass, EFFECT_EXTERNAL_PUBLISH};

    #[test]
    fn proxy_exposes_host_minimum_instead_of_raw_plugin_approval_flag() {
        let proxy = RuntimePluginToolProxy::new(
            "fixture",
            "1.0.0",
            7,
            RuntimePluginToolManifest {
                name: "publish_anywhere".to_string(),
                description: "fixture".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                requires_approval: false,
                category: RuntimePluginToolCategory::General,
                effects: vec![PluginEffectDeclaration {
                    id: EFFECT_EXTERNAL_PUBLISH.to_string(),
                    description: None,
                    requires_approval: false,
                }],
            },
            Arc::new(RuntimePluginHost::default()),
        );

        assert!(proxy.requires_approval());
        assert_eq!(proxy.effects()[0].id, EFFECT_EXTERNAL_PUBLISH);
        assert_eq!(
            proxy.effect_policy().rate_limit,
            ToolRateLimitClass::OutboundAction
        );
        assert_eq!(
            proxy.authorization_provider(),
            "runtime_plugin:fixture@1.0.0#generation:7"
        );
    }
}
