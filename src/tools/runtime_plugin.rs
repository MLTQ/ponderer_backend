use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::runtime_plugin_host::{RuntimePluginHost, RuntimePluginToolManifest};

use super::{Tool, ToolCategory, ToolContext, ToolOutput};

pub struct RuntimePluginToolProxy {
    plugin_id: String,
    manifest: RuntimePluginToolManifest,
    host: Arc<RuntimePluginHost>,
}

impl RuntimePluginToolProxy {
    pub fn new(
        plugin_id: impl Into<String>,
        manifest: RuntimePluginToolManifest,
        host: Arc<RuntimePluginHost>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
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

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        self.host
            .invoke_tool(&self.plugin_id, &self.manifest.name, params)
            .await
    }

    fn requires_approval(&self) -> bool {
        self.manifest.requires_approval
    }

    fn category(&self) -> ToolCategory {
        self.manifest.category.as_tool_category()
    }
}
