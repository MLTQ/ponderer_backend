//! Authenticated private Telegram transport, supervised by the UI-owned backend.
//! Receiving never waits for generation. Replies and chosen outreach use a durable
//! outbox; an ambiguous send is NOT retried because Telegram has no idempotency key.
use crate::{
    database::communications::{telegram_conversation_id, Delivery},
    server::ServerState,
};
use chrono::{Timelike, Utc};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, task::JoinHandle};

#[derive(Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    error_code: Option<i64>,
    parameters: Option<ResponseParameters>,
}
#[derive(Deserialize)]
struct ResponseParameters {
    retry_after: Option<u64>,
}
#[derive(Deserialize)]
struct Update {
    update_id: i64,
    message: Option<TelegramMessage>,
    callback_query: Option<Callback>,
}
#[derive(Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
}
#[derive(Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct TelegramUser {
    id: i64,
    #[serde(default)]
    is_bot: bool,
}
#[derive(Deserialize)]
struct Callback {
    id: String,
    from: TelegramUser,
    message: Option<TelegramMessage>,
    data: Option<String>,
}
#[derive(Deserialize)]
struct SentMessage {
    message_id: i64,
}

#[derive(Default)]
pub struct TelegramBotManager {
    task: Mutex<Option<TelegramBotTask>>,
}
struct TelegramBotTask {
    token: String,
    owner: i64,
    join: JoinHandle<()>,
}
impl TelegramBotManager {
    pub fn new() -> Self {
        Self {
            task: Mutex::new(None),
        }
    }
    pub async fn reconfigure(&self, state: Arc<ServerState>, token: String, owner: Option<i64>) {
        let token = token.trim().to_owned();
        let owner = owner.filter(|id| *id > 0).filter(|_| !token.is_empty());
        let mut guard = self.task.lock().await;
        if guard
            .as_ref()
            .is_some_and(|t| t.token == token && Some(t.owner) == owner && !t.join.is_finished())
        {
            return;
        }
        if let Some(task) = guard.take() {
            task.join.abort();
            let _ = task.join.await; // Drop both network futures before changing endpoints.
        }
        if let Err(error) = state.db.configure_telegram_endpoint(owner, Utc::now()) {
            tracing::error!("Telegram endpoint configuration failed: {error}");
            return;
        }
        let Some(owner) = owner else {
            tracing::info!(
                "Telegram disabled: a token and positive private owner chat ID are required"
            );
            return;
        };
        let client = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .build()
        {
            Ok(c) => c,
            Err(_) => {
                tracing::error!("Telegram HTTP client initialization failed");
                return;
            }
        };
        let base = format!("https://api.telegram.org/bot{token}");
        // The public bot ID namespaces durable cursors; never persist the secret token.
        let source = format!("telegram:{}", token.split(':').next().unwrap_or("unknown"));
        let join = tokio::spawn(async move {
            // No detached children: aborting this task cancels receive AND send.
            tokio::join!(
                receive_loop(&state, &client, &base, &source, owner),
                delivery_loop(&state, &client, &base, owner)
            );
        });
        *guard = Some(TelegramBotTask { token, owner, join });
    }
}

fn authorized(message: &TelegramMessage, owner: i64) -> bool {
    message.chat.id == owner
        && message.chat.kind == "private"
        && message
            .from
            .as_ref()
            .is_some_and(|u| u.id == owner && !u.is_bot)
}

async fn receive_loop(
    state: &ServerState,
    client: &reqwest::Client,
    base: &str,
    source: &str,
    owner: i64,
) {
    loop {
        let offset = match state.db.telegram_offset(source) {
            Ok(n) => n,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let result = client
            .post(format!("{base}/getUpdates"))
            .json(&serde_json::json!({
                "offset":offset,"timeout":30,"allowed_updates":["message","callback_query"]
            }))
            .send()
            .await;
        let body = match result {
            Ok(r) => r.json::<TelegramResponse<Vec<Update>>>().await.ok(),
            Err(_) => None, // reqwest errors contain URLs, and URLs contain the bot secret.
        };
        let Some(body) = body.filter(|b| b.ok) else {
            tracing::warn!("Telegram receive failed; retrying without advancing cursor");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        };
        for update in body.result.unwrap_or_default() {
            let message = update.message.as_ref().filter(|m| authorized(m, owner));
            let text = message
                .and_then(|m| m.text.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let callback = update.callback_query.as_ref().filter(|c| {
                c.from.id == owner
                    && !c.from.is_bot
                    && c.message
                        .as_ref()
                        .is_some_and(|m| m.chat.id == owner && m.chat.kind == "private")
            });
            // Feedback is idempotent. Persist it before acknowledging this update
            // so a database failure cannot silently lose the operator's response.
            if let Some(callback) = callback {
                if let Some((feedback, id)) =
                    callback.data.as_deref().and_then(|d| d.split_once(':'))
                {
                    if matches!(feedback, "welcomed" | "dismissed")
                        && state
                            .db
                            .record_contact_feedback(id, feedback, Some(owner), Utc::now())
                            .is_err()
                    {
                        tracing::warn!("Telegram feedback could not be committed");
                        break;
                    }
                }
            }
            match state
                .db
                .ingest_telegram_update(source, update.update_id, owner, text, Utc::now())
            {
                Ok(true) => state
                    .agent
                    .notify_operator_message_queued(&telegram_conversation_id(owner)),
                Ok(false) => {}
                Err(_) => {
                    tracing::warn!("Telegram receipt could not be committed");
                    break;
                }
            }
            if let Some(callback) = callback {
                let _ = client
                    .post(format!("{base}/answerCallbackQuery"))
                    .json(&serde_json::json!({"callback_query_id":callback.id}))
                    .send()
                    .await;
            }
        }
    }
}

async fn delivery_loop(state: &ServerState, client: &reqwest::Client, base: &str, owner: i64) {
    loop {
        let mut config = state.config.read().await.clone();
        config.outreach.enabled &= config.enable_ambient_loop;
        let paused = state.agent.runtime_status().await.paused;
        let delivery = state.db.claim_delivery(
            owner,
            &config.outreach,
            chrono::Local::now().hour() as u8,
            paused,
            Utc::now(),
        );
        match delivery {
            Ok(Some(delivery)) => {
                let outcome = send_delivery(client, base, &delivery).await;
                if state
                    .db
                    .settle_delivery(
                        &delivery,
                        outcome.state,
                        outcome.message_id,
                        outcome.error,
                        outcome.retry_secs,
                        Utc::now(),
                    )
                    .is_err()
                {
                    tracing::warn!("Telegram delivery receipt could not be committed; lease will become uncertain");
                }
            }
            Err(_) => tracing::warn!("Telegram outbox unavailable"),
            Ok(None) => {}
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

struct DeliveryOutcome {
    state: &'static str,
    message_id: Option<i64>,
    error: Option<&'static str>,
    retry_secs: u64,
}
async fn send_delivery(
    client: &reqwest::Client,
    base: &str,
    delivery: &Delivery,
) -> DeliveryOutcome {
    let mut request = serde_json::json!({"chat_id":delivery.endpoint,"text":delivery.content});
    if let Some(id) = &delivery.intent_id {
        request["reply_markup"] = serde_json::json!({"inline_keyboard":[[
            {"text":"Welcome thought","callback_data":format!("welcomed:{id}")},
            {"text":"Less of this","callback_data":format!("dismissed:{id}")}
        ]]});
    }
    let response = client
        .post(format!("{base}/sendMessage"))
        .json(&request)
        .send()
        .await;
    let body = match response {
        Ok(r) => r.json::<TelegramResponse<SentMessage>>().await.ok(),
        Err(e) if e.is_connect() => {
            return DeliveryOutcome {
                state: if delivery.attempts < 4 {
                    "pending"
                } else {
                    "failed"
                },
                message_id: None,
                error: Some("Connection failed before delivery"),
                retry_secs: 30,
            }
        }
        Err(_) => None,
    };
    match body {
        Some(b) if b.ok && b.result.is_some() => DeliveryOutcome {
            state: "sent",
            message_id: b.result.map(|m| m.message_id),
            error: None,
            retry_secs: 0,
        },
        Some(b) if !b.ok && b.error_code == Some(429) && delivery.attempts < 4 => DeliveryOutcome {
            state: "pending",
            message_id: None,
            error: Some("Telegram rate limit"),
            retry_secs: b
                .parameters
                .and_then(|p| p.retry_after)
                .unwrap_or(30)
                .clamp(1, 86400),
        },
        Some(b) if !b.ok => DeliveryOutcome {
            state: "failed",
            message_id: None,
            error: Some("Telegram explicitly rejected delivery"),
            retry_secs: 0,
        },
        _ => DeliveryOutcome {
            state: "uncertain",
            message_id: None,
            error: Some("Delivery receipt unknown; not automatically retried"),
            retry_secs: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn mock_telegram(response: serde_json::Value) -> (String, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/sendMessage",
            axum::routing::post(move || {
                let response = response.clone();
                async move { axum::Json(response) }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), task)
    }
    fn delivery() -> Delivery {
        Delivery {
            id: "d".into(),
            message_id: "m".into(),
            intent_id: None,
            endpoint: "42".into(),
            content: "hello".into(),
            part: 0,
            attempts: 0,
            lease_token: "lease".into(),
        }
    }
    #[tokio::test]
    async fn only_provider_receipts_confirm_delivery_and_rate_limit_is_bounded() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for (response, expected, id, delay) in [
            (
                serde_json::json!({"ok":true,"result":{"message_id":7}}),
                "sent",
                Some(7),
                0,
            ),
            (
                serde_json::json!({"ok":false,"error_code":429,"parameters":{"retry_after":12}}),
                "pending",
                None,
                12,
            ),
            (
                serde_json::json!({"ok":false,"error_code":403}),
                "failed",
                None,
                0,
            ),
            (serde_json::json!({"ok":true}), "uncertain", None, 0),
        ] {
            let (base, task) = mock_telegram(response).await;
            let outcome = send_delivery(&client, &base, &delivery()).await;
            task.abort();
            assert_eq!(outcome.state, expected);
            assert_eq!(outcome.message_id, id);
            assert_eq!(outcome.retry_secs, delay);
        }
    }

    #[tokio::test]
    async fn send_timeout_is_uncertain_not_retryable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/sendMessage",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                axum::Json(serde_json::json!({"ok":true,"result":{"message_id":7}}))
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(30))
            .build()
            .unwrap();
        let result = send_delivery(&client, &format!("http://{address}"), &delivery()).await;
        task.abort();
        assert_eq!(result.state, "uncertain");
    }

    #[test]
    fn owner_requires_private_chat_and_matching_sender() {
        let mut message: TelegramMessage = serde_json::from_value(serde_json::json!({
            "chat":{"id":42,"type":"private"},"from":{"id":42},"text":"hello"
        }))
        .unwrap();
        assert!(authorized(&message, 42));
        message.chat.kind = "group".into();
        assert!(!authorized(&message, 42));
        message.chat.kind = "private".into();
        message.from.as_mut().unwrap().id = 43;
        assert!(!authorized(&message, 42));
        message.from = None;
        assert!(!authorized(&message, 42));
    }
}
