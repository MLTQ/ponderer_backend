use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::AgentConfig;
use crate::skills::Skill;
use crate::tools::ToolRegistry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendPluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub provided_tools: Vec<String>,
    pub provided_skills: Vec<String>,
}

#[async_trait]
pub trait BackendPlugin: Send + Sync {
    fn manifest(&self) -> BackendPluginManifest;

    async fn register_tools(
        &self,
        _tool_registry: Arc<ToolRegistry>,
        _config: &AgentConfig,
    ) -> Result<()> {
        Ok(())
    }

    fn build_skills(&self, _config: &AgentConfig) -> Result<Vec<Box<dyn Skill>>> {
        Ok(Vec::new())
    }
}
