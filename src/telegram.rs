//! Telegram bot integration for Ponderer.
//!
//! When `TELEGRAM_BOT_TOKEN` is set, spawns a long-polling tokio task that:
//! - Receives Telegram messages and routes them into a dedicated "telegram" conversation.
//! - Waits for the agent's `ChatReply` event and sends the reply back to the user.
//!
//! Optional: set `TELEGRAM_CHAT_ID` to restrict the bot to a single authorized chat.
//!
//! No new dependencies — uses existing `reqwest` for HTTP.

use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::broadcast;

use crate::database::TELEGRAM_CONVERSATION_ID;
use crate::server::{ApiEventEnvelope, ServerState};

// ─── Telegram API types ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
}

#[derive(Deserialize)]
struct Update {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    text: Option<String>,
}

#[derive(Deserialize)]
struct TelegramChat {
    id: i64,
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Spawn the Telegram bot task if `TELEGRAM_BOT_TOKEN` is set.
/// Does nothing (returns immediately) when the env var is absent.
pub fn spawn_telegram_bot(state: Arc<ServerState>) {
    let token = match std::env::var("TELEGRAM_BOT_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return,
    };

    let allowed_chat_id: Option<i64> = std::env::var("TELEGRAM_CHAT_ID")
        .ok()
        .and_then(|s| s.trim().parse().ok());

    tokio::spawn(async move {
        tracing::info!(
            "Telegram bot active (allowed_chat_id: {:?})",
            allowed_chat_id
        );
        run_bot(state, token, allowed_chat_id).await;
    });
}

// ─── Bot loop ─────────────────────────────────────────────────────────────────

async fn run_bot(state: Arc<ServerState>, token: String, allowed_chat_id: Option<i64>) {
    let api_base = format!("https://api.telegram.org/bot{}", token);
    let client = reqwest::Client::new();
    let mut offset: i64 = 0;

    // Subscribe before processing any messages so we never miss a quick reply.
    let mut event_rx = state.ws_events.subscribe();

    loop {
        let updates = match poll_updates(&client, &api_base, offset).await {
            Some(u) => u,
            None => continue,
        };

        for update in updates {
            offset = update.update_id + 1;

            let msg = match update.message {
                Some(m) => m,
                None => continue,
            };

            let chat_id = msg.chat.id;

            if let Some(allowed) = allowed_chat_id {
                if chat_id != allowed {
                    tracing::debug!(
                        "Telegram: ignoring message from unauthorized chat {}",
                        chat_id
                    );
                    continue;
                }
            }

            let text = match msg.text {
                Some(t) if !t.trim().is_empty() => t.trim().to_string(),
                _ => continue,
            };

            tracing::info!("Telegram [chat {}]: {:?}", chat_id, text);

            // Route into the telegram conversation.
            match state
                .db
                .add_chat_message_in_conversation(TELEGRAM_CONVERSATION_ID, "operator", &text)
            {
                Ok(_) => state
                    .agent
                    .notify_operator_message_queued(TELEGRAM_CONVERSATION_ID),
                Err(e) => {
                    tracing::error!("Telegram: failed to store message: {}", e);
                    continue;
                }
            }

            // Wait for the agent's reply and relay it.
            if let Some(reply) = wait_for_reply(&mut event_rx, TELEGRAM_CONVERSATION_ID).await {
                if !reply.trim().is_empty() {
                    send_message(&client, &api_base, chat_id, &reply).await;
                }
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

async fn poll_updates(
    client: &reqwest::Client,
    api_base: &str,
    offset: i64,
) -> Option<Vec<Update>> {
    let url = format!("{}/getUpdates", api_base);
    let params = serde_json::json!({
        "offset": offset,
        "timeout": 30,
        "allowed_updates": ["message"]
    });

    let resp = match client.post(&url).json(&params).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Telegram getUpdates error: {}", e);
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            return None;
        }
    };

    let body: TelegramResponse<Vec<Update>> = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Telegram getUpdates parse error: {}", e);
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            return None;
        }
    };

    if !body.ok {
        tracing::warn!("Telegram API returned ok=false");
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        return None;
    }

    Some(body.result.unwrap_or_default())
}

/// Wait up to 120 s for a `chat_reply` event for `conversation_id`.
async fn wait_for_reply(
    event_rx: &mut broadcast::Receiver<ApiEventEnvelope>,
    conversation_id: &str,
) -> Option<String> {
    let timeout = tokio::time::Duration::from_secs(120);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                "Telegram: timed out waiting for reply (conversation={})",
                conversation_id
            );
            return None;
        }

        match tokio::time::timeout(remaining, event_rx.recv()).await {
            Ok(Ok(envelope)) => {
                if envelope.event_type == "chat_reply" {
                    let conv = envelope
                        .payload
                        .get("conversation_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if conv == conversation_id {
                        return envelope
                            .payload
                            .get("content")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!("Telegram: event receiver lagged by {} messages", n);
                // Continue — next recv will skip ahead to the current tail.
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                tracing::error!("Telegram: event channel closed");
                return None;
            }
            Err(_) => {
                tracing::warn!(
                    "Telegram: timed out waiting for reply (conversation={})",
                    conversation_id
                );
                return None;
            }
        }
    }
}

async fn send_message(client: &reqwest::Client, api_base: &str, chat_id: i64, text: &str) {
    // Telegram enforces a 4096-character limit per message.
    const MAX_LEN: usize = 4096;
    let text = if text.len() > MAX_LEN {
        &text[..MAX_LEN]
    } else {
        text
    };

    let url = format!("{}/sendMessage", api_base);
    let payload = serde_json::json!({ "chat_id": chat_id, "text": text });

    match client.post(&url).json(&payload).send().await {
        Ok(r) if r.status().is_success() => {
            tracing::debug!("Telegram: sent reply to chat {}", chat_id);
        }
        Ok(r) => {
            tracing::warn!("Telegram sendMessage failed: HTTP {}", r.status());
        }
        Err(e) => {
            tracing::error!("Telegram sendMessage error: {}", e);
        }
    }
}
