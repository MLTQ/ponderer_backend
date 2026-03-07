//! Memory-oriented tools for agentic recall and note-taking.
//!
//! - `search_memory`: query persisted working memory entries.
//! - `write_memory`: create or update a working-memory note.
//! - `write_session_handoff`: write a cross-session continuity note injected at the top of next-session context.
//! - `private_chat_mode`: inspect/set/toggle runtime private-chat mode (`agentic` vs `direct`).
//! - `scratch_note`: read/write/append/clear a task-scoped scratchpad (ephemeral, cleared when task is done).
//! - `flag_uncertainty`: non-blocking heads-up to the operator before acting under uncertainty.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::AgentEvent;
use crate::config::{
    normalize_private_chat_mode, AgentConfig, PRIVATE_CHAT_MODE_AGENTIC, PRIVATE_CHAT_MODE_DIRECT,
};
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
            "bytes_written": final_content.len(),
        })))
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}

/// The fixed working-memory key used to store the cross-session handoff note.
pub const SESSION_HANDOFF_KEY: &str = "session-handoff";
pub const PRIVATE_CHAT_MODE_STATE_KEY: &str = "private-chat-mode";

pub struct WriteSessionHandoffTool;

impl WriteSessionHandoffTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WriteSessionHandoffTool {
    fn name(&self) -> &str {
        "write_session_handoff"
    }

    fn description(&self) -> &str {
        "Write a one-shot handoff note for your next session. Use this when wrapping up work to \
         capture: what you were doing, how far you got, the immediate next step, and any open \
         questions or blockers. The note is injected at the very top of your next session's context \
         and then automatically cleared — so if you want continuity across sessions, you must call \
         this tool again at the end of each session. One clean note per wrap-up; do not call mid-task."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The handoff note. Include: what you were working on, progress so far, the next concrete step, and any open questions or blockers. Be specific enough that you can resume cold."
                }
            },
            "required": ["content"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let content = match params.get("content").and_then(Value::as_str).map(str::trim) {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing required 'content' parameter".to_string(),
                ))
            }
        };

        let db = match open_database() {
            Ok(db) => db,
            Err(e) => return Ok(ToolOutput::Error(e.to_string())),
        };

        if let Err(e) = db.set_working_memory(SESSION_HANDOFF_KEY, content) {
            return Ok(ToolOutput::Error(format!(
                "Failed to save handoff note: {}",
                e
            )));
        }

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "message": "Handoff note saved. It will be injected at the top of your context when you return.",
        })))
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}

pub struct PrivateChatModeTool;

impl PrivateChatModeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for PrivateChatModeTool {
    fn name(&self) -> &str {
        "private_chat_mode"
    }

    fn description(&self) -> &str {
        "Get or change private-chat execution mode. \
         Modes: 'agentic' (multi-turn autonomous continuation) or 'direct' (single-turn reply with optional tool use)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "toggle"],
                    "description": "Operation to perform. Defaults to 'get'."
                },
                "mode": {
                    "type": "string",
                    "enum": ["agentic", "direct"],
                    "description": "Required for action='set'."
                }
            }
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("get")
            .to_ascii_lowercase();

        let db = match open_database() {
            Ok(db) => db,
            Err(error) => return Ok(ToolOutput::Error(error.to_string())),
        };

        let current_mode = db
            .get_state(PRIVATE_CHAT_MODE_STATE_KEY)
            .ok()
            .flatten()
            .map(|value| normalize_private_chat_mode(&value))
            .unwrap_or_else(|| normalize_private_chat_mode(&AgentConfig::load().private_chat_mode));

        if action == "get" {
            return Ok(ToolOutput::Json(json!({
                "status": "ok",
                "mode": current_mode,
            })));
        }

        let target_mode = if action == "toggle" {
            if current_mode == PRIVATE_CHAT_MODE_DIRECT {
                PRIVATE_CHAT_MODE_AGENTIC.to_string()
            } else {
                PRIVATE_CHAT_MODE_DIRECT.to_string()
            }
        } else if action == "set" {
            let Some(mode) = params.get("mode").and_then(Value::as_str) else {
                return Ok(ToolOutput::Error(
                    "Missing required 'mode' parameter for action='set'.".to_string(),
                ));
            };
            let normalized = normalize_private_chat_mode(mode);
            if normalized != mode.trim().to_ascii_lowercase() {
                return Ok(ToolOutput::Error(
                    "Invalid 'mode' parameter. Use 'agentic' or 'direct'.".to_string(),
                ));
            }
            normalized
        } else {
            return Ok(ToolOutput::Error(
                "Invalid 'action' parameter. Use 'get', 'set', or 'toggle'.".to_string(),
            ));
        };

        if let Err(error) = db.set_state(PRIVATE_CHAT_MODE_STATE_KEY, &target_mode) {
            return Ok(ToolOutput::Error(format!(
                "Failed to update private chat mode state: {}",
                error
            )));
        }

        let mut persisted = false;
        let mut persist_error: Option<String> = None;
        let mut config = AgentConfig::load();
        config.private_chat_mode = target_mode.clone();
        match config.save() {
            Ok(_) => persisted = true,
            Err(error) => persist_error = Some(error.to_string()),
        }

        if let Err(error) = db.append_daily_activity_log(&format!(
            "private_chat_mode action='{}' from='{}' to='{}'",
            action, current_mode, target_mode
        )) {
            tracing::warn!("Failed to append private-chat-mode activity log: {}", error);
        }

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "action": action,
            "previous_mode": current_mode,
            "mode": target_mode,
            "persisted_to_config": persisted,
            "persist_error": persist_error,
        })))
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}

/// The working-memory key used for the active task scratchpad.
pub const SCRATCHPAD_KEY: &str = "scratchpad";

pub struct ScratchNoteTool;

impl ScratchNoteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ScratchNoteTool {
    fn name(&self) -> &str {
        "scratch_note"
    }

    fn description(&self) -> &str {
        "Read or update your active task scratchpad — ephemeral working notes for the current task. \
         Use mode='replace' to overwrite, 'append' to add to what's there, 'clear' to wipe it when \
         the task is done, or 'read' to retrieve the current contents. \
         Persists across turns within a session but meant to be cleared when you finish a task. \
         Good for: what you know so far, steps completed/remaining, things tried that didn't work, \
         what you'd do next if interrupted."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "Content to write (required for replace/append; omit for read/clear)"
                },
                "mode": {
                    "type": "string",
                    "enum": ["replace", "append", "clear", "read"],
                    "description": "replace: overwrite; append: add to existing; clear: wipe when done; read: retrieve current contents"
                }
            },
            "required": ["mode"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let mode = params
            .get("mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("read");

        let db = match open_database() {
            Ok(db) => db,
            Err(e) => return Ok(ToolOutput::Error(e.to_string())),
        };

        match mode {
            "read" => {
                let content = db
                    .get_working_memory(SCRATCHPAD_KEY)
                    .ok()
                    .flatten()
                    .map(|e| e.content)
                    .unwrap_or_default();
                if content.trim().is_empty() {
                    Ok(ToolOutput::Json(
                        json!({ "status": "ok", "content": "", "empty": true }),
                    ))
                } else {
                    Ok(ToolOutput::Json(
                        json!({ "status": "ok", "content": content, "empty": false }),
                    ))
                }
            }
            "clear" => {
                if let Err(e) = db.set_working_memory(SCRATCHPAD_KEY, "") {
                    return Ok(ToolOutput::Error(format!(
                        "Failed to clear scratchpad: {}",
                        e
                    )));
                }
                Ok(ToolOutput::Json(
                    json!({ "status": "ok", "message": "Scratchpad cleared." }),
                ))
            }
            "replace" | "append" => {
                let content = match params.get("content").and_then(Value::as_str).map(str::trim) {
                    Some(v) if !v.is_empty() => v,
                    _ => {
                        return Ok(ToolOutput::Error(
                            "'content' is required for replace/append modes".to_string(),
                        ))
                    }
                };
                let final_content = if mode == "append" {
                    let existing = db
                        .get_working_memory(SCRATCHPAD_KEY)
                        .ok()
                        .flatten()
                        .map(|e| e.content)
                        .unwrap_or_default();
                    if existing.trim().is_empty() {
                        content.to_string()
                    } else {
                        format!("{}\n{}", existing.trim_end(), content)
                    }
                } else {
                    content.to_string()
                };
                if let Err(e) = db.set_working_memory(SCRATCHPAD_KEY, &final_content) {
                    return Ok(ToolOutput::Error(format!(
                        "Failed to write scratchpad: {}",
                        e
                    )));
                }
                Ok(ToolOutput::Json(json!({
                    "status": "ok",
                    "mode": mode,
                    "content": final_content,
                })))
            }
            other => Ok(ToolOutput::Error(format!(
                "Unknown mode '{}'. Use replace, append, clear, or read.",
                other
            ))),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
}

/// Non-blocking uncertainty signal — tells the operator "heads up, about to do X".
pub struct FlagUncertaintyTool {
    event_tx: flume::Sender<AgentEvent>,
}

impl FlagUncertaintyTool {
    pub fn new(event_tx: flume::Sender<AgentEvent>) -> Self {
        Self { event_tx }
    }
}

#[async_trait]
impl Tool for FlagUncertaintyTool {
    fn name(&self) -> &str {
        "flag_uncertainty"
    }

    fn description(&self) -> &str {
        "Signal that you're about to act on something you're ~90% confident about but want the \
         operator to know. Returns immediately so you can proceed without waiting. Use before \
         significant or hard-to-reverse actions when you have a reasonable plan but aren't certain \
         it's exactly right. Do NOT use as a substitute for the approval gate on genuinely risky \
         operations that require explicit permission."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "What you're uncertain about (the specific doubt or assumption)"
                },
                "planned_action": {
                    "type": "string",
                    "description": "What you're about to do despite the uncertainty"
                }
            },
            "required": ["question", "planned_action"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let question = match params
            .get("question")
            .and_then(Value::as_str)
            .map(str::trim)
        {
            Some(v) if !v.is_empty() => v.to_string(),
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing required 'question' parameter".to_string(),
                ))
            }
        };
        let planned_action = match params
            .get("planned_action")
            .and_then(Value::as_str)
            .map(str::trim)
        {
            Some(v) if !v.is_empty() => v.to_string(),
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing required 'planned_action' parameter".to_string(),
                ))
            }
        };

        let _ = self.event_tx.send(AgentEvent::UncertaintyFlagged {
            question,
            planned_action,
        });

        Ok(ToolOutput::Json(json!({
            "status": "noted",
            "message": "Uncertainty flagged to operator. Proceed with your planned action.",
        })))
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::General
    }
}
