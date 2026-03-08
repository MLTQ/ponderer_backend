use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::helpers::{summarize_chat_message_for_context, truncate_for_db_digest};
use super::AgentDatabase;

pub const DEFAULT_CHAT_SESSION_ID: &str = "default_session";
pub const DEFAULT_CHAT_CONVERSATION_ID: &str = "default";
pub const TELEGRAM_CONVERSATION_ID: &str = "telegram";
pub(super) const CHAT_TOOL_BLOCK_START: &str = "[tool_calls]";
pub(super) const CHAT_TOOL_BLOCK_END: &str = "[/tool_calls]";
pub(super) const CHAT_THINKING_BLOCK_START: &str = "[thinking]";
pub(super) const CHAT_THINKING_BLOCK_END: &str = "[/thinking]";
pub(super) const CHAT_MEDIA_BLOCK_START: &str = "[media]";
pub(super) const CHAT_MEDIA_BLOCK_END: &str = "[/media]";
pub(super) const CHAT_TURN_CONTROL_BLOCK_START: &str = "[turn_control]";
pub(super) const CHAT_TURN_CONTROL_BLOCK_END: &str = "[/turn_control]";
pub(super) const CHAT_CONCERNS_BLOCK_START: &str = "[concerns]";
pub(super) const CHAT_CONCERNS_BLOCK_END: &str = "[/concerns]";

/// A private chat message between the operator and the agent.
/// These are separate from forum posts - truly private dialogue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub conversation_id: String,
    pub role: String, // "operator" or "agent"
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub processed: bool, // Has the agent seen/responded to this?
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatTurnPhase {
    Idle,
    Processing,
    Completed,
    AwaitingApproval,
    Failed,
}

impl ChatTurnPhase {
    pub(super) fn as_db_str(self) -> &'static str {
        match self {
            ChatTurnPhase::Idle => "idle",
            ChatTurnPhase::Processing => "processing",
            ChatTurnPhase::Completed => "completed",
            ChatTurnPhase::AwaitingApproval => "awaiting_approval",
            ChatTurnPhase::Failed => "failed",
        }
    }

    pub(super) fn from_db(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "processing" => ChatTurnPhase::Processing,
            "completed" => ChatTurnPhase::Completed,
            "awaiting_approval" => ChatTurnPhase::AwaitingApproval,
            "failed" => ChatTurnPhase::Failed,
            _ => ChatTurnPhase::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConversation {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub runtime_state: ChatTurnPhase,
    pub active_turn_id: Option<String>,
    pub message_count: usize,
    pub last_message_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConversationSummary {
    pub conversation_id: String,
    pub summary_text: String,
    pub summarized_message_count: usize,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub id: String,
    pub session_id: String,
    pub conversation_id: String,
    pub iteration: i64,
    pub phase_state: ChatTurnPhase,
    pub decision: Option<String>,
    pub status: Option<String>,
    pub trigger_message_ids: Vec<String>,
    pub operator_message: Option<String>,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub tool_call_count: usize,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub agent_message_id: Option<String>,
    pub prompt_text: Option<String>,
    pub system_prompt_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurnToolCall {
    pub id: String,
    pub turn_id: String,
    pub call_index: usize,
    pub tool_name: String,
    pub arguments_json: String,
    pub output_text: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OodaTurnPacketRecord {
    pub id: String,
    pub conversation_id: String,
    pub turn_id: Option<String>,
    pub observe: String,
    pub orient: String,
    pub decide: String,
    pub act: String,
    pub created_at: DateTime<Utc>,
}

impl AgentDatabase {
    /// Add a chat message
    pub fn add_chat_message(&self, role: &str, content: &str) -> Result<String> {
        self.add_chat_message_in_conversation(DEFAULT_CHAT_CONVERSATION_ID, role, content)
    }

    fn add_chat_message_internal(
        &self,
        conversation_id: &str,
        role: &str,
        content: &str,
        turn_id: Option<&str>,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let conversation_id = if conversation_id.trim().is_empty() {
            DEFAULT_CHAT_CONVERSATION_ID
        } else {
            conversation_id.trim()
        };
        let now = Utc::now().to_rfc3339();
        let processed = if role.eq_ignore_ascii_case("operator") {
            0
        } else {
            1
        };
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO chat_sessions (id, label, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                DEFAULT_CHAT_SESSION_ID,
                "Default session",
                now.clone(),
                now.clone()
            ],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO chat_conversations (id, session_id, title, created_at, updated_at, runtime_state, active_turn_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                conversation_id,
                DEFAULT_CHAT_SESSION_ID,
                "Conversation",
                now.clone(),
                now.clone(),
                ChatTurnPhase::Idle.as_db_str(),
            ],
        )?;
        conn.execute(
            "INSERT INTO chat_messages (id, conversation_id, role, content, created_at, processed, turn_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, conversation_id, role, content, now.clone(), processed, turn_id],
        )?;
        if role.eq_ignore_ascii_case("operator") {
            conn.execute(
                "UPDATE chat_conversations
                 SET runtime_state = ?2, active_turn_id = NULL, updated_at = ?3
                 WHERE id = ?1",
                params![conversation_id, ChatTurnPhase::Idle.as_db_str(), now],
            )?;
        } else {
            conn.execute(
                "UPDATE chat_conversations
                 SET updated_at = ?2
                 WHERE id = ?1",
                params![conversation_id, now],
            )?;
        }
        Ok(id)
    }

    /// Add a chat message to a specific conversation.
    pub fn add_chat_message_in_conversation(
        &self,
        conversation_id: &str,
        role: &str,
        content: &str,
    ) -> Result<String> {
        self.add_chat_message_internal(conversation_id, role, content, None)
    }

    /// Add a chat message and attach it to a specific turn.
    pub fn add_chat_message_in_turn(
        &self,
        conversation_id: &str,
        turn_id: &str,
        role: &str,
        content: &str,
    ) -> Result<String> {
        self.add_chat_message_internal(conversation_id, role, content, Some(turn_id))
    }

    /// Create a new conversation and return it.
    pub fn create_chat_conversation(&self, title: Option<&str>) -> Result<ChatConversation> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let title = title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Chat {}", now.format("%Y-%m-%d %H:%M")));

        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO chat_sessions (id, label, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                DEFAULT_CHAT_SESSION_ID,
                "Default session",
                now_str.clone(),
                now_str.clone()
            ],
        )?;
        conn.execute(
            "INSERT INTO chat_conversations (id, session_id, title, created_at, updated_at, runtime_state, active_turn_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                id,
                DEFAULT_CHAT_SESSION_ID,
                title,
                now_str.clone(),
                now_str,
                ChatTurnPhase::Idle.as_db_str(),
            ],
        )?;

        Ok(ChatConversation {
            id,
            session_id: DEFAULT_CHAT_SESSION_ID.to_string(),
            title,
            created_at: now,
            updated_at: now,
            runtime_state: ChatTurnPhase::Idle,
            active_turn_id: None,
            message_count: 0,
            last_message_at: None,
        })
    }

    /// List recent conversations, newest activity first.
    pub fn list_chat_conversations(&self, limit: usize) -> Result<Vec<ChatConversation>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            r#"SELECT
                   c.id,
                   c.session_id,
                   c.title,
                   c.created_at,
                   c.updated_at,
                   c.runtime_state,
                   c.active_turn_id,
                   COUNT(m.id) as message_count,
                   MAX(m.created_at) as last_message_at
               FROM chat_conversations c
               LEFT JOIN chat_messages m ON m.conversation_id = c.id
               GROUP BY c.id
               ORDER BY COALESCE(MAX(m.created_at), c.updated_at) DESC
               LIMIT ?1"#,
        )?;

        let conversations = stmt
            .query_map([limit], |row| {
                let created_at_str: String = row.get(3)?;
                let updated_at_str: String = row.get(4)?;
                let runtime_state_raw: String = row.get(5)?;
                let active_turn_id: Option<String> = row.get(6)?;
                let message_count = row.get::<_, i64>(7)? as usize;
                let last_message_at_str: Option<String> = row.get(8)?;

                Ok(ChatConversation {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    title: row.get(2)?,
                    created_at: created_at_str.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    updated_at: updated_at_str.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    runtime_state: ChatTurnPhase::from_db(&runtime_state_raw),
                    active_turn_id,
                    message_count,
                    last_message_at: match last_message_at_str {
                        Some(v) => Some(v.parse().map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                8,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?),
                        None => None,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(conversations)
    }

    /// Fetch one conversation by ID.
    pub fn get_chat_conversation(&self, conversation_id: &str) -> Result<Option<ChatConversation>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            r#"SELECT
                   c.id,
                   c.session_id,
                   c.title,
                   c.created_at,
                   c.updated_at,
                   c.runtime_state,
                   c.active_turn_id,
                   COUNT(m.id) as message_count,
                   MAX(m.created_at) as last_message_at
               FROM chat_conversations c
               LEFT JOIN chat_messages m ON m.conversation_id = c.id
               WHERE c.id = ?1
               GROUP BY c.id
               LIMIT 1"#,
        )?;

        let mut rows = stmt.query([conversation_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let created_at_str: String = row.get(3)?;
        let updated_at_str: String = row.get(4)?;
        let runtime_state_raw: String = row.get(5)?;
        let active_turn_id: Option<String> = row.get(6)?;
        let message_count = row.get::<_, i64>(7)? as usize;
        let last_message_at_str: Option<String> = row.get(8)?;

        Ok(Some(ChatConversation {
            id: row.get(0)?,
            session_id: row.get(1)?,
            title: row.get(2)?,
            created_at: created_at_str.parse().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            updated_at: updated_at_str.parse().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            runtime_state: ChatTurnPhase::from_db(&runtime_state_raw),
            active_turn_id,
            message_count,
            last_message_at: match last_message_at_str {
                Some(v) => Some(v.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?),
                None => None,
            },
        }))
    }

    /// Delete a conversation and all its associated data.
    pub fn delete_chat_conversation(&self, conversation_id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        // Delete tool calls for all turns belonging to this conversation.
        conn.execute(
            "DELETE FROM chat_turn_tool_calls WHERE turn_id IN (SELECT id FROM chat_turns WHERE conversation_id = ?1)",
            params![conversation_id],
        )?;
        conn.execute(
            "DELETE FROM chat_turns WHERE conversation_id = ?1",
            params![conversation_id],
        )?;
        conn.execute(
            "DELETE FROM chat_messages WHERE conversation_id = ?1",
            params![conversation_id],
        )?;
        conn.execute(
            "DELETE FROM chat_conversation_summaries WHERE conversation_id = ?1",
            params![conversation_id],
        )?;
        conn.execute(
            "DELETE FROM ooda_turn_packets WHERE conversation_id = ?1",
            params![conversation_id],
        )?;
        conn.execute(
            "DELETE FROM chat_conversations WHERE id = ?1",
            params![conversation_id],
        )?;
        Ok(())
    }

    /// Update the title of a conversation.
    pub fn update_chat_conversation_title(&self, conversation_id: &str, title: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE chat_conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, Utc::now().to_rfc3339(), conversation_id],
        )?;
        Ok(())
    }

    /// Start a new persisted turn for a conversation.
    pub fn begin_chat_turn(
        &self,
        conversation_id: &str,
        trigger_message_ids: &[String],
        iteration: i64,
    ) -> Result<String> {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let trigger_json =
            serde_json::to_string(trigger_message_ids).unwrap_or_else(|_| "[]".to_string());
        let conn = self.lock_conn()?;

        conn.execute(
            "INSERT OR IGNORE INTO chat_sessions (id, label, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                DEFAULT_CHAT_SESSION_ID,
                "Default session",
                now.clone(),
                now.clone()
            ],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO chat_conversations (id, session_id, title, created_at, updated_at, runtime_state, active_turn_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                conversation_id,
                DEFAULT_CHAT_SESSION_ID,
                "Conversation",
                now.clone(),
                now.clone(),
                ChatTurnPhase::Idle.as_db_str(),
            ],
        )?;

        let session_id: String = conn.query_row(
            "SELECT session_id FROM chat_conversations WHERE id = ?1",
            [conversation_id],
            |row| row.get(0),
        )?;

        conn.execute(
            "INSERT INTO chat_turns (
                id, session_id, conversation_id, iteration, phase_state, decision, status,
                trigger_message_ids_json, operator_message, reason, error, tool_call_count,
                started_at, completed_at, agent_message_id, prompt_text, system_prompt_text
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6, NULL, NULL, NULL, 0, ?7, NULL, NULL, NULL, NULL)",
            params![
                turn_id,
                session_id,
                conversation_id,
                iteration,
                ChatTurnPhase::Processing.as_db_str(),
                trigger_json,
                now.clone(),
            ],
        )?;

        conn.execute(
            "UPDATE chat_conversations
             SET runtime_state = ?2, active_turn_id = ?3, updated_at = ?4
             WHERE id = ?1",
            params![
                conversation_id,
                ChatTurnPhase::Processing.as_db_str(),
                turn_id,
                now
            ],
        )?;

        Ok(turn_id)
    }

    /// Persist the full prompt payload used to generate a specific turn.
    pub fn set_chat_turn_prompt(&self, turn_id: &str, prompt_text: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE chat_turns
             SET prompt_text = ?2
             WHERE id = ?1",
            params![turn_id, prompt_text],
        )?;
        Ok(())
    }

    /// Persist user + system prompt payloads used to generate a specific turn.
    pub fn set_chat_turn_prompt_bundle(
        &self,
        turn_id: &str,
        prompt_text: &str,
        system_prompt_text: &str,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE chat_turns
             SET prompt_text = ?2, system_prompt_text = ?3
             WHERE id = ?1",
            params![turn_id, prompt_text, system_prompt_text],
        )?;
        Ok(())
    }

    /// Fetch the stored prompt payload for one turn.
    pub fn get_chat_turn_prompt(&self, turn_id: &str) -> Result<Option<String>> {
        Ok(self
            .get_chat_turn_prompt_bundle(turn_id)?
            .and_then(|(prompt_text, _)| prompt_text))
    }

    /// Fetch stored user/system prompts for one turn.
    pub fn get_chat_turn_prompt_bundle(
        &self,
        turn_id: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT prompt_text, system_prompt_text
             FROM chat_turns
             WHERE id = ?1
             LIMIT 1",
        )?;
        let mut rows = stmt.query([turn_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
        )))
    }

    /// Record a single tool call made during a turn.
    pub fn record_chat_turn_tool_call(
        &self,
        turn_id: &str,
        call_index: usize,
        tool_name: &str,
        arguments_json: &str,
        output_text: &str,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO chat_turn_tool_calls (id, turn_id, call_index, tool_name, arguments_json, output_text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                uuid::Uuid::new_v4().to_string(),
                turn_id,
                call_index as i64,
                tool_name,
                arguments_json,
                output_text,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Complete a turn and set its terminal state.
    pub fn complete_chat_turn(
        &self,
        turn_id: &str,
        phase_state: ChatTurnPhase,
        decision: &str,
        status: &str,
        operator_message: &str,
        reason: Option<&str>,
        tool_call_count: usize,
        agent_message_id: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.lock_conn()?;

        conn.execute(
            "UPDATE chat_turns
             SET phase_state = ?2,
                 decision = ?3,
                 status = ?4,
                 operator_message = ?5,
                 reason = ?6,
                 error = NULL,
                 tool_call_count = ?7,
                 completed_at = ?8,
                 agent_message_id = ?9
             WHERE id = ?1",
            params![
                turn_id,
                phase_state.as_db_str(),
                decision,
                status,
                operator_message,
                reason,
                tool_call_count as i64,
                now.clone(),
                agent_message_id,
            ],
        )?;

        conn.execute(
            "UPDATE chat_conversations
             SET runtime_state = ?2, active_turn_id = NULL, updated_at = ?3
             WHERE id = (SELECT conversation_id FROM chat_turns WHERE id = ?1)",
            params![turn_id, phase_state.as_db_str(), now],
        )?;

        Ok(())
    }

    /// Mark a turn as failed and retain the error text for post-mortem.
    pub fn fail_chat_turn(&self, turn_id: &str, error: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.lock_conn()?;

        conn.execute(
            "UPDATE chat_turns
             SET phase_state = ?2,
                 error = ?3,
                 completed_at = ?4
             WHERE id = ?1",
            params![
                turn_id,
                ChatTurnPhase::Failed.as_db_str(),
                error,
                now.clone()
            ],
        )?;

        conn.execute(
            "UPDATE chat_conversations
             SET runtime_state = ?2, active_turn_id = NULL, updated_at = ?3
             WHERE id = (SELECT conversation_id FROM chat_turns WHERE id = ?1)",
            params![turn_id, ChatTurnPhase::Failed.as_db_str(), now],
        )?;

        Ok(())
    }

    /// List recent turns for one conversation (newest first).
    pub fn list_chat_turns_for_conversation(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<ChatTurn>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT
                id, session_id, conversation_id, iteration, phase_state, decision, status,
                trigger_message_ids_json, operator_message, reason, error, tool_call_count,
                started_at, completed_at, agent_message_id, prompt_text, system_prompt_text
             FROM chat_turns
             WHERE conversation_id = ?1
             ORDER BY started_at DESC
             LIMIT ?2",
        )?;

        let turns = stmt
            .query_map(params![conversation_id, limit], |row| {
                let started_at_str: String = row.get(12)?;
                let completed_at_str: Option<String> = row.get(13)?;
                let phase_state_raw: String = row.get(4)?;
                let trigger_ids_raw: String = row.get(7)?;
                let trigger_message_ids = serde_json::from_str::<Vec<String>>(&trigger_ids_raw)
                    .unwrap_or_else(|_| Vec::new());

                Ok(ChatTurn {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    conversation_id: row.get(2)?,
                    iteration: row.get(3)?,
                    phase_state: ChatTurnPhase::from_db(&phase_state_raw),
                    decision: row.get(5)?,
                    status: row.get(6)?,
                    trigger_message_ids,
                    operator_message: row.get(8)?,
                    reason: row.get(9)?,
                    error: row.get(10)?,
                    tool_call_count: row.get::<_, i64>(11)? as usize,
                    started_at: started_at_str.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            12,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    completed_at: match completed_at_str {
                        Some(v) => Some(v.parse().map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                13,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?),
                        None => None,
                    },
                    agent_message_id: row.get(14)?,
                    prompt_text: row.get(15)?,
                    system_prompt_text: row.get(16)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(turns)
    }

    /// List tool calls for a specific turn.
    pub fn list_chat_turn_tool_calls(&self, turn_id: &str) -> Result<Vec<ChatTurnToolCall>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, turn_id, call_index, tool_name, arguments_json, output_text, created_at
             FROM chat_turn_tool_calls
             WHERE turn_id = ?1
             ORDER BY call_index ASC",
        )?;

        let calls = stmt
            .query_map([turn_id], |row| {
                let created_at_str: String = row.get(6)?;
                Ok(ChatTurnToolCall {
                    id: row.get(0)?,
                    turn_id: row.get(1)?,
                    call_index: row.get::<_, i64>(2)? as usize,
                    tool_name: row.get(3)?,
                    arguments_json: row.get(4)?,
                    output_text: row.get(5)?,
                    created_at: created_at_str.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(calls)
    }

    pub fn save_ooda_turn_packet(&self, packet: &OodaTurnPacketRecord) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO ooda_turn_packets
             (id, conversation_id, turn_id, observe, orient, decide, act, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                packet.id,
                packet.conversation_id,
                packet.turn_id,
                packet.observe,
                packet.orient,
                packet.decide,
                packet.act,
                packet.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_latest_ooda_turn_packet_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<OodaTurnPacketRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, turn_id, observe, orient, decide, act, created_at
             FROM ooda_turn_packets
             WHERE conversation_id = ?1
             ORDER BY created_at DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![conversation_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let created_at_raw: String = row.get(7)?;
        Ok(Some(OodaTurnPacketRecord {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            turn_id: row.get(2)?,
            observe: row.get(3)?,
            orient: row.get(4)?,
            decide: row.get(5)?,
            act: row.get(6)?,
            created_at: created_at_raw.parse().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        }))
    }

    pub fn get_latest_ooda_turn_packet(&self) -> Result<Option<OodaTurnPacketRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, turn_id, observe, orient, decide, act, created_at
             FROM ooda_turn_packets
             ORDER BY created_at DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let created_at_raw: String = row.get(7)?;
        Ok(Some(OodaTurnPacketRecord {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            turn_id: row.get(2)?,
            observe: row.get(3)?,
            orient: row.get(4)?,
            decide: row.get(5)?,
            act: row.get(6)?,
            created_at: created_at_raw.parse().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        }))
    }

    pub fn get_recent_ooda_turn_packets_for_conversation_before(
        &self,
        conversation_id: &str,
        before_inclusive: &DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<OodaTurnPacketRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, turn_id, observe, orient, decide, act, created_at
             FROM ooda_turn_packets
             WHERE conversation_id = ?1
               AND created_at <= ?2
             ORDER BY created_at DESC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(
                params![conversation_id, before_inclusive.to_rfc3339(), limit as i64],
                |row| {
                    let created_at_raw: String = row.get(7)?;
                    Ok(OodaTurnPacketRecord {
                        id: row.get(0)?,
                        conversation_id: row.get(1)?,
                        turn_id: row.get(2)?,
                        observe: row.get(3)?,
                        orient: row.get(4)?,
                        decide: row.get(5)?,
                        act: row.get(6)?,
                        created_at: created_at_raw.parse().map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                7,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows.into_iter().rev().collect())
    }

    pub fn get_recent_action_digest(&self, limit: usize, max_chars: usize) -> Result<String> {
        self.get_recent_action_digest_inner(None, limit, max_chars)
    }

    pub fn get_recent_action_digest_for_conversation(
        &self,
        conversation_id: &str,
        limit: usize,
        max_chars: usize,
    ) -> Result<String> {
        self.get_recent_action_digest_inner(Some(conversation_id), limit, max_chars)
    }

    fn get_recent_action_digest_inner(
        &self,
        conversation_id: Option<&str>,
        limit: usize,
        max_chars: usize,
    ) -> Result<String> {
        let conn = self.lock_conn()?;
        let limit = limit.max(1) as i64;
        let mut lines: Vec<String> = Vec::new();

        let mut stmt = conn.prepare(
            "SELECT
                ct.conversation_id,
                ct.started_at,
                ct.phase_state,
                ct.decision,
                ct.status,
                ct.tool_call_count,
                ct.reason,
                ct.error,
                COALESCE(
                  (
                    SELECT GROUP_CONCAT(tool_name, ',')
                    FROM (
                      SELECT tool_name
                      FROM chat_turn_tool_calls tc
                      WHERE tc.turn_id = ct.id
                      ORDER BY call_index ASC
                      LIMIT 4
                    )
                  ),
                  ''
                ) AS tool_preview,
                COALESCE(cm.content, '') AS agent_content
             FROM chat_turns ct
             LEFT JOIN chat_messages cm ON cm.id = ct.agent_message_id
             WHERE (?1 IS NULL OR ct.conversation_id = ?1)
             ORDER BY ct.started_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![conversation_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)? as usize,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        })?;
        for row in rows {
            let (
                convo,
                started_at,
                phase_state,
                decision,
                status,
                tool_count,
                reason,
                error,
                tool_preview,
                agent_content,
            ) = row?;
            let mut line = format!(
                "- [{}] {}phase={} decision={} status={} tools={}{}",
                started_at,
                if conversation_id.is_some() {
                    String::new()
                } else {
                    format!("conv={} ", convo)
                },
                phase_state,
                decision.unwrap_or_else(|| "-".to_string()),
                status.unwrap_or_else(|| "-".to_string()),
                tool_count,
                if tool_preview.trim().is_empty() {
                    String::new()
                } else {
                    format!(" ({})", tool_preview)
                }
            );

            let response_preview = summarize_chat_message_for_context(&agent_content);
            if let Some(error_text) = error
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                line.push_str(&format!(
                    " error=\"{}\"",
                    truncate_for_db_digest(error_text, 140)
                ));
            } else if !response_preview.is_empty() {
                line.push_str(&format!(
                    " reply=\"{}\"",
                    truncate_for_db_digest(&response_preview, 160)
                ));
            }

            if let Some(reason_text) = reason
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                line.push_str(&format!(
                    " reason=\"{}\"",
                    truncate_for_db_digest(reason_text, 120)
                ));
            }

            lines.push(line);
        }

        if lines.is_empty() {
            return Ok("None".to_string());
        }

        let mut output = String::new();
        for line in lines {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&line);
            if output.chars().count() >= max_chars.max(120) {
                output = truncate_for_db_digest(&output, max_chars.max(120));
                break;
            }
        }
        Ok(output)
    }

    /// Get unprocessed messages from the operator
    pub fn get_unprocessed_operator_messages(&self) -> Result<Vec<ChatMessage>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, role, content, created_at, processed, turn_id FROM chat_messages
             WHERE role IN ('operator', 'scheduled') AND processed = 0
             ORDER BY created_at ASC",
        )?;

        let messages = stmt
            .query_map([], |row| {
                Ok(ChatMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    created_at: row.get::<_, String>(4)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    processed: row.get::<_, i64>(5)? != 0,
                    turn_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(messages)
    }

    /// Mark a message as processed
    pub fn mark_message_processed(&self, id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute("UPDATE chat_messages SET processed = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Get recent chat history (for context)
    pub fn get_chat_history(&self, limit: usize) -> Result<Vec<ChatMessage>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, role, content, created_at, processed, turn_id FROM chat_messages
             ORDER BY created_at DESC
             LIMIT ?1",
        )?;

        let messages = stmt
            .query_map([limit], |row| {
                Ok(ChatMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    created_at: row.get::<_, String>(4)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    processed: row.get::<_, i64>(5)? != 0,
                    turn_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // Reverse to get chronological order
        Ok(messages.into_iter().rev().collect())
    }

    /// Get recent chat history for one conversation (chronological order).
    pub fn get_chat_history_for_conversation(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, role, content, created_at, processed, turn_id FROM chat_messages
             WHERE conversation_id = ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;

        let messages = stmt
            .query_map(params![conversation_id, limit], |row| {
                Ok(ChatMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    created_at: row.get::<_, String>(4)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    processed: row.get::<_, i64>(5)? != 0,
                    turn_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // Reverse to get chronological order
        Ok(messages.into_iter().rev().collect())
    }

    /// Get a chronological message window using offset from latest messages.
    ///
    /// `offset_from_latest = 0` means the newest `limit` messages.
    /// `offset_from_latest = 20` skips the most recent 20 messages, then returns
    /// the next `limit` older messages.
    pub fn get_chat_history_slice_for_conversation(
        &self,
        conversation_id: &str,
        offset_from_latest: usize,
        limit: usize,
    ) -> Result<Vec<ChatMessage>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, role, content, created_at, processed, turn_id FROM chat_messages
             WHERE conversation_id = ?1
             ORDER BY created_at DESC
             LIMIT ?2 OFFSET ?3",
        )?;

        let messages = stmt
            .query_map(params![conversation_id, limit, offset_from_latest], |row| {
                Ok(ChatMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    created_at: row.get::<_, String>(4)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    processed: row.get::<_, i64>(5)? != 0,
                    turn_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(messages.into_iter().rev().collect())
    }

    /// Count messages in one conversation.
    pub fn count_chat_messages_for_conversation(&self, conversation_id: &str) -> Result<usize> {
        let conn = self.lock_conn()?;
        let count = conn.query_row(
            "SELECT COUNT(1) FROM chat_messages WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as usize)
    }

    /// Upsert a compacted summary snapshot for a conversation.
    pub fn upsert_chat_conversation_summary(
        &self,
        conversation_id: &str,
        summary_text: &str,
        summarized_message_count: usize,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO chat_conversation_summaries (conversation_id, summary_text, summarized_message_count, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(conversation_id) DO UPDATE SET
                summary_text = excluded.summary_text,
                summarized_message_count = excluded.summarized_message_count,
                updated_at = excluded.updated_at",
            params![
                conversation_id,
                summary_text,
                summarized_message_count as i64,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Fetch compacted summary snapshot for one conversation.
    pub fn get_chat_conversation_summary(
        &self,
        conversation_id: &str,
    ) -> Result<Option<ChatConversationSummary>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT conversation_id, summary_text, summarized_message_count, updated_at
             FROM chat_conversation_summaries
             WHERE conversation_id = ?1",
        )?;
        let mut rows = stmt.query([conversation_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let updated_at_raw: String = row.get(3)?;
        let updated_at = updated_at_raw.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?;

        Ok(Some(ChatConversationSummary {
            conversation_id: row.get(0)?,
            summary_text: row.get(1)?,
            summarized_message_count: row.get::<_, i64>(2)?.max(0) as usize,
            updated_at,
        }))
    }

    /// Get chat history as formatted string for context
    pub fn get_chat_context(&self, limit: usize) -> Result<String> {
        let messages = self.get_chat_history(limit)?;
        Self::format_chat_context(messages)
    }

    /// Get chat history as formatted string for one conversation.
    pub fn get_chat_context_for_conversation(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Result<String> {
        let messages = self.get_chat_history_for_conversation(conversation_id, limit)?;
        Self::format_chat_context(messages)
    }

    fn format_chat_context(messages: Vec<ChatMessage>) -> Result<String> {
        if messages.is_empty() {
            return Ok(String::new());
        }

        let mut context = String::from("## Recent Private Chat with Operator\n\n");
        for msg in messages {
            let role_display = if msg.role == "operator" {
                "Operator"
            } else {
                "You"
            };
            let sanitized = summarize_chat_message_for_context(&msg.content);
            if sanitized.is_empty() {
                continue;
            }
            context.push_str(&format!("**{}**: {}\n\n", role_display, sanitized));
        }
        Ok(context)
    }
}
