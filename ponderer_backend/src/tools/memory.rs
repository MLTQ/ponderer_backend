//! Memory-oriented tools for agentic recall and note-taking.
//!
//! - `search_memory`: query persisted working memory entries.
//! - `write_memory`: create or update a working-memory note.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::config::AgentConfig;
use crate::database::AgentDatabase;

use super::{Tool, ToolCategory, ToolContext, ToolOutput};

const DEFAULT_SEARCH_LIMIT: usize = 8;
const MAX_SEARCH_LIMIT: usize = 50;

fn open_database() -> Result<AgentDatabase> {
    let config = AgentConfig::load();
    AgentDatabase::new(&config.database_path).with_context(|| {
        format!(
            "Failed to open memory database at '{}'",
            config.database_path
        )
    })
}

pub struct MemorySearchTool;

impl MemorySearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "search_memory"
    }

    fn description(&self) -> &str {
        "Search persistent working memory notes by query text."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Text query to match against memory keys and content"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (1-50)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if query.is_empty() {
            return Ok(ToolOutput::Error(
                "Missing required 'query' parameter".to_string(),
            ));
        }

        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| (v as usize).clamp(1, MAX_SEARCH_LIMIT))
            .unwrap_or(DEFAULT_SEARCH_LIMIT);

        let db = match open_database() {
            Ok(db) => db,
            Err(e) => return Ok(ToolOutput::Error(e.to_string())),
        };
        let matches = match db.search_working_memory(query, limit) {
            Ok(items) => items,
            Err(e) => return Ok(ToolOutput::Error(format!("Memory search failed: {}", e))),
        };

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "query": query,
            "match_count": matches.len(),
            "matches": matches.into_iter().map(|entry| json!({
                "key": entry.key,
                "content": entry.content,
                "updated_at": entry.updated_at.to_rfc3339(),
            })).collect::<Vec<_>>()
        })))
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}

pub struct MemoryWriteTool;

impl MemoryWriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "write_memory"
    }

    fn description(&self) -> &str {
        "Write a persistent memory note by key. Supports replace or append mode."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Memory key to write"
                },
                "content": {
                    "type": "string",
                    "description": "Memory note content"
                },
                "mode": {
                    "type": "string",
                    "enum": ["replace", "append"],
                    "description": "replace overwrites; append adds content to the existing note"
                }
            },
            "required": ["key", "content"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let key = match params.get("key").and_then(Value::as_str).map(str::trim) {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing required 'key' parameter".to_string(),
                ))
            }
        };
        let content = match params.get("content").and_then(Value::as_str).map(str::trim) {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing required 'content' parameter".to_string(),
                ))
            }
        };
        let mode = params
            .get("mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("replace");
        if mode != "replace" && mode != "append" {
            return Ok(ToolOutput::Error(
                "Invalid 'mode' parameter. Use 'replace' or 'append'.".to_string(),
            ));
        }

        let db = match open_database() {
            Ok(db) => db,
            Err(e) => return Ok(ToolOutput::Error(e.to_string())),
        };

        let final_content = if mode == "append" {
            let existing = db
                .get_working_memory(key)?
                .map(|entry| entry.content)
                .unwrap_or_default();
            if existing.trim().is_empty() {
                content.to_string()
            } else {
                format!("{}\n{}", existing.trim_end(), content)
            }
        } else {
            content.to_string()
        };

        if let Err(e) = db.set_working_memory(key, &final_content) {
            return Ok(ToolOutput::Error(format!("Failed to write memory: {}", e)));
        }
        if let Err(e) =
            db.append_daily_activity_log(&format!("write_memory key='{}' mode='{}'", key, mode))
        {
            tracing::warn!("Failed to append memory-write activity log: {}", e);
        }

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "key": key,
            "mode": mode,
            "content": final_content,
        })))
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}
