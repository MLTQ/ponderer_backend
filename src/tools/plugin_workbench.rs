use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::plugin_contract::PluginEffectDeclaration;
use crate::plugin_workbench::PluginWorkbench;

use super::effect_policy::EFFECT_PLUGIN_DRAFT_WRITE;
use super::{Tool, ToolCategory, ToolContext, ToolOutput};

pub struct PluginWorkbenchTool {
    workbench: PluginWorkbench,
    effects: Vec<PluginEffectDeclaration>,
}

impl PluginWorkbenchTool {
    pub fn new(workbench: PluginWorkbench) -> Self {
        Self {
            workbench,
            effects: vec![PluginEffectDeclaration {
                id: EFFECT_PLUGIN_DRAFT_WRITE.to_string(),
                description: Some(
                    "Writes only inside Ponderer's inert plugin workbench or disabled package store"
                        .to_string(),
                ),
                requires_approval: false,
            }],
        }
    }
}

#[derive(Debug, Deserialize)]
struct WorkbenchRequest {
    action: String,
    #[serde(default)]
    plugin_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    contents: Option<String>,
}

#[async_trait]
impl Tool for PluginWorkbenchTool {
    fn name(&self) -> &str {
        "plugin_workbench"
    }

    fn description(&self) -> &str {
        "Build protocol-v1 plugins in Ponderer's confined workbench. Create, read, write, validate, and stage drafts; staged packages remain disabled and cannot execute until separately authorized."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "create_python", "read", "write", "validate", "stage"]
                },
                "plugin_id": {"type": "string"},
                "display_name": {"type": "string"},
                "description": {"type": "string"},
                "path": {"type": "string", "description": "Path relative to the selected draft"},
                "contents": {"type": "string"}
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let request: WorkbenchRequest =
            serde_json::from_value(params).context("invalid plugin_workbench parameters")?;
        let plugin_id = || {
            request
                .plugin_id
                .as_deref()
                .context("plugin_id is required for this action")
        };
        let output = match request.action.as_str() {
            "list" => serde_json::to_value(self.workbench.list_drafts()?)?,
            "create_python" => serde_json::to_value(
                self.workbench.create_python_draft(
                    plugin_id()?,
                    request
                        .display_name
                        .as_deref()
                        .context("display_name is required for create_python")?,
                    request
                        .description
                        .as_deref()
                        .context("description is required for create_python")?,
                )?,
            )?,
            "read" => json!({
                "plugin_id": plugin_id()?,
                "path": request.path.as_deref().context("path is required for read")?,
                "contents": self.workbench.read_draft_file(
                    plugin_id()?,
                    request.path.as_deref().context("path is required for read")?,
                )?,
            }),
            "write" => json!({
                "plugin_id": plugin_id()?,
                "path": self.workbench.write_draft_file(
                    plugin_id()?,
                    request.path.as_deref().context("path is required for write")?,
                    request.contents.as_deref().context("contents is required for write")?,
                )?,
            }),
            "validate" => serde_json::to_value(self.workbench.validate_draft(plugin_id()?)?)?,
            "stage" => serde_json::to_value(self.workbench.stage_draft(plugin_id()?)?)?,
            other => anyhow::bail!("unknown plugin_workbench action '{other}'"),
        };
        Ok(ToolOutput::Json(output))
    }

    fn effects(&self) -> &[PluginEffectDeclaration] {
        &self.effects
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::tools::effect_policy::ToolApprovalMinimum;

    fn context() -> ToolContext {
        ToolContext {
            working_directory: ".".to_string(),
            username: "Ponderer".to_string(),
            conversation_id: None,
            autonomous: true,
            allowed_tools: None,
            disallowed_tools: Vec::new(),
            outbound_action_rate_limit: None,
            generation_observer: None,
        }
    }

    #[tokio::test]
    async fn model_can_iterate_and_stage_but_not_activate() {
        let root =
            std::env::temp_dir().join(format!("ponderer_workbench_tool_{}", uuid::Uuid::new_v4()));
        let tool = PluginWorkbenchTool::new(PluginWorkbench::new(
            root.join("drafts"),
            root.join("store"),
        ));
        let created = tool
            .execute(
                json!({
                    "action": "create_python",
                    "plugin_id": "dev.timekeeper",
                    "display_name": "Timekeeper",
                    "description": "Observes elapsed time"
                }),
                &context(),
            )
            .await
            .expect("create");
        assert!(created.is_success());
        let staged = tool
            .execute(
                json!({"action": "stage", "plugin_id": "dev.timekeeper"}),
                &context(),
            )
            .await
            .expect("stage");
        let ToolOutput::Json(staged) = staged else {
            panic!("expected JSON")
        };
        assert_eq!(staged["enabled"], false);
        assert!(tool.parameters_schema().to_string().contains("stage"));
        assert!(!tool.parameters_schema().to_string().contains("activate"));
        assert_eq!(tool.effect_policy().approval, ToolApprovalMinimum::None);
        let _ = std::fs::remove_dir_all(PathBuf::from(root));
    }
}
