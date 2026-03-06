//! Scheduled-job tools for autonomous recurring task management.
//!
//! - `list_scheduled_jobs`: inspect currently configured recurring jobs.
//! - `create_scheduled_job`: create a new recurring job.
//! - `update_scheduled_job`: modify an existing recurring job.
//! - `delete_scheduled_job`: remove a recurring job.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::config::AgentConfig;
use crate::database::AgentDatabase;

use super::{Tool, ToolCategory, ToolContext, ToolOutput};

const DEFAULT_LIST_LIMIT: usize = 24;
const MAX_LIST_LIMIT: usize = 200;

fn open_database() -> Result<AgentDatabase> {
    let config = AgentConfig::load();
    AgentDatabase::new(&config.database_path).with_context(|| {
        format!(
            "Failed to open database at '{}' for scheduled-job tool",
            config.database_path
        )
    })
}

pub struct ListScheduledJobsTool;

impl ListScheduledJobsTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ListScheduledJobsTool {
    fn name(&self) -> &str {
        "list_scheduled_jobs"
    }

    fn description(&self) -> &str {
        "List recurring scheduled jobs, including next run time and enabled state."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of jobs to return (1-200)"
                }
            }
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .map(|value| (value as usize).clamp(1, MAX_LIST_LIMIT))
            .unwrap_or(DEFAULT_LIST_LIMIT);

        let db = match open_database() {
            Ok(db) => db,
            Err(error) => return Ok(ToolOutput::Error(error.to_string())),
        };

        match db.list_scheduled_jobs(limit) {
            Ok(jobs) => Ok(ToolOutput::Json(json!({
                "status": "ok",
                "count": jobs.len(),
                "jobs": jobs,
            }))),
            Err(error) => Ok(ToolOutput::Error(format!(
                "Failed to list scheduled jobs: {}",
                error
            ))),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}

pub struct CreateScheduledJobTool;

impl CreateScheduledJobTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CreateScheduledJobTool {
    fn name(&self) -> &str {
        "create_scheduled_job"
    }

    fn description(&self) -> &str {
        "Create a recurring scheduled job that periodically queues a task prompt."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Short human-readable name for the schedule"
                },
                "prompt": {
                    "type": "string",
                    "description": "Prompt content that will be queued each run"
                },
                "interval_minutes": {
                    "type": "integer",
                    "description": "Run interval in minutes (1-10080). Defaults to 60"
                },
                "enabled": {
                    "type": "boolean",
                    "description": "Whether the schedule is enabled immediately (default: true)"
                }
            },
            "required": ["name", "prompt"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let Some(name) = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(ToolOutput::Error(
                "Missing required 'name' parameter".to_string(),
            ));
        };

        let Some(prompt) = params
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(ToolOutput::Error(
                "Missing required 'prompt' parameter".to_string(),
            ));
        };

        let interval_minutes = params
            .get("interval_minutes")
            .and_then(Value::as_u64)
            .unwrap_or(60);
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let db = match open_database() {
            Ok(db) => db,
            Err(error) => return Ok(ToolOutput::Error(error.to_string())),
        };

        let mut job = match db.create_scheduled_job(name, prompt, interval_minutes) {
            Ok(job) => job,
            Err(error) => {
                return Ok(ToolOutput::Error(format!(
                    "Failed to create scheduled job: {}",
                    error
                )))
            }
        };

        if !enabled {
            match db.update_scheduled_job(&job.id, None, None, None, Some(false)) {
                Ok(Some(updated)) => job = updated,
                Ok(None) => {
                    return Ok(ToolOutput::Error(
                        "Scheduled job created but could not be disabled".to_string(),
                    ))
                }
                Err(error) => {
                    return Ok(ToolOutput::Error(format!(
                        "Scheduled job created but disable step failed: {}",
                        error
                    )))
                }
            }
        }

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "job": job,
        })))
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}

pub struct UpdateScheduledJobTool;

impl UpdateScheduledJobTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for UpdateScheduledJobTool {
    fn name(&self) -> &str {
        "update_scheduled_job"
    }

    fn description(&self) -> &str {
        "Update fields on an existing scheduled job (name, prompt, interval, enabled)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "ID of the scheduled job to update"
                },
                "name": {
                    "type": "string",
                    "description": "New schedule name (optional)"
                },
                "prompt": {
                    "type": "string",
                    "description": "New prompt content (optional)"
                },
                "interval_minutes": {
                    "type": "integer",
                    "description": "New interval in minutes (1-10080, optional)"
                },
                "enabled": {
                    "type": "boolean",
                    "description": "Enable or disable the schedule (optional)"
                }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let Some(job_id) = params
            .get("job_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(ToolOutput::Error(
                "Missing required 'job_id' parameter".to_string(),
            ));
        };

        let name = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let prompt = params
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let interval_minutes = params.get("interval_minutes").and_then(Value::as_u64);
        let enabled = params.get("enabled").and_then(Value::as_bool);

        if name.is_none() && prompt.is_none() && interval_minutes.is_none() && enabled.is_none() {
            return Ok(ToolOutput::Error(
                "Provide at least one field to update: name, prompt, interval_minutes, or enabled"
                    .to_string(),
            ));
        }

        let db = match open_database() {
            Ok(db) => db,
            Err(error) => return Ok(ToolOutput::Error(error.to_string())),
        };

        match db.update_scheduled_job(job_id, name, prompt, interval_minutes, enabled) {
            Ok(Some(job)) => Ok(ToolOutput::Json(json!({
                "status": "ok",
                "job": job,
            }))),
            Ok(None) => Ok(ToolOutput::Error(format!(
                "Scheduled job '{}' was not found",
                job_id
            ))),
            Err(error) => Ok(ToolOutput::Error(format!(
                "Failed to update scheduled job '{}': {}",
                job_id, error
            ))),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}

pub struct DeleteScheduledJobTool;

impl DeleteScheduledJobTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for DeleteScheduledJobTool {
    fn name(&self) -> &str {
        "delete_scheduled_job"
    }

    fn description(&self) -> &str {
        "Delete a scheduled job by ID."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "ID of the scheduled job to delete"
                }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let Some(job_id) = params
            .get("job_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(ToolOutput::Error(
                "Missing required 'job_id' parameter".to_string(),
            ));
        };

        let db = match open_database() {
            Ok(db) => db,
            Err(error) => return Ok(ToolOutput::Error(error.to_string())),
        };

        match db.delete_scheduled_job(job_id) {
            Ok(true) => Ok(ToolOutput::Json(json!({
                "status": "ok",
                "deleted": true,
                "job_id": job_id,
            }))),
            Ok(false) => Ok(ToolOutput::Error(format!(
                "Scheduled job '{}' was not found",
                job_id
            ))),
            Err(error) => Ok(ToolOutput::Error(format!(
                "Failed to delete scheduled job '{}': {}",
                job_id, error
            ))),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}
