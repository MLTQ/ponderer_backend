//! Bridge tools that expose existing Skill actions to the agentic tool loop.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::config::AgentConfig;
use crate::skills::graphchan::GraphchanSkill;
use crate::skills::{Skill, SkillResult};

use super::{Tool, ToolCategory, ToolContext, ToolOutput};

pub struct GraphchanSkillTool;

impl GraphchanSkillTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GraphchanSkillTool {
    fn name(&self) -> &str {
        "graphchan_skill"
    }

    fn description(&self) -> &str {
        "Execute Graphchan skill actions (`reply`, `list_threads`) through the unified tool loop."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Graphchan skill action name (reply | list_threads)"
                },
                "params": {
                    "type": "object",
                    "description": "Action-specific parameters"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, params: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let action = match params.get("action").and_then(Value::as_str).map(str::trim) {
            Some(v) if !v.is_empty() => v.to_string(),
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing required 'action' parameter".to_string(),
                ))
            }
        };
        let mut action_params = params
            .get("params")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

        let config = AgentConfig::load();
        if config.graphchan_api_url.trim().is_empty() {
            return Ok(ToolOutput::Error(
                "Graphchan is not configured (graphchan_api_url is empty).".to_string(),
            ));
        }
        let skill = GraphchanSkill::new(config.graphchan_api_url);

        if action == "reply"
            && action_params
                .get("username")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            if let Some(obj) = action_params.as_object_mut() {
                obj.insert("username".to_string(), Value::String(ctx.username.clone()));
            }
        }

        match skill.execute(&action, &action_params).await {
            Ok(SkillResult::Success { message }) => Ok(ToolOutput::Json(json!({
                "status": "ok",
                "skill": "graphchan",
                "action": action,
                "message": message,
            }))),
            Ok(SkillResult::Error { message }) => Ok(ToolOutput::Error(format!(
                "Graphchan action '{}' failed: {}",
                action, message
            ))),
            Err(e) => Ok(ToolOutput::Error(format!(
                "Graphchan action '{}' errored: {}",
                action, e
            ))),
        }
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Network
    }
}
