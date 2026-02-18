use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::agent::{AgentEvent, AgentRuntimeStatus};
use crate::config::AgentConfig;
use crate::database::{
    AgentDatabase, ChatConversation, ChatConversationSummary, ChatMessage, ChatTurn,
    ChatTurnToolCall,
};
use crate::plugin::BackendPluginManifest;
use crate::runtime::BackendRuntime;

#[derive(Clone)]
pub struct ServerState {
    pub agent: Arc<crate::agent::Agent>,
    pub db: Arc<AgentDatabase>,
    pub auth: BackendAuthConfig,
    pub config: Arc<tokio::sync::RwLock<AgentConfig>>,
    pub plugin_manifests: Vec<BackendPluginManifest>,
    pub ws_events: broadcast::Sender<ApiEventEnvelope>,
}

#[derive(Debug, Clone)]
pub struct BackendAuthConfig {
    mode: AuthMode,
    token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    Required,
    Disabled,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiEventEnvelope {
    pub event_type: String,
    pub emitted_at: DateTime<Utc>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Deserialize)]
struct ListConversationsQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ListMessagesQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ListTurnsQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateConversationRequest {
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SendMessageRequest {
    content: String,
}

#[derive(Debug, Deserialize)]
struct SetPauseRequest {
    paused: bool,
}

#[derive(Debug, Serialize)]
struct SendMessageResponse {
    status: &'static str,
    message_id: String,
}

#[derive(Debug, Serialize)]
struct PauseStateResponse {
    paused: bool,
}

#[derive(Debug, Serialize)]
struct StopResponse {
    stopped: bool,
}

#[derive(Debug, Serialize)]
struct ChatTurnPromptResponse {
    turn_id: String,
    prompt_text: String,
    system_prompt_text: Option<String>,
}

pub async fn serve_backend(
    runtime: BackendRuntime,
    event_rx: flume::Receiver<AgentEvent>,
) -> Result<()> {
    let bind_addr = std::env::var("PONDERER_BACKEND_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".to_string())
        .parse::<SocketAddr>()
        .context("Invalid PONDERER_BACKEND_BIND (expected host:port)")?;

    let auth = load_auth_config()?;

    let db = runtime
        .ui_database
        .clone()
        .ok_or_else(|| anyhow!("Backend database unavailable"))?;
    let (ws_events, _) = broadcast::channel(512);

    let state = Arc::new(ServerState {
        agent: runtime.agent.clone(),
        db,
        auth,
        config: Arc::new(tokio::sync::RwLock::new(runtime.config.clone())),
        plugin_manifests: runtime.plugin_manifests.clone(),
        ws_events: ws_events.clone(),
    });

    spawn_event_bridge(event_rx, ws_events);
    runtime.spawn_agent_loop();

    let protected = Router::new()
        .route("/health", get(health))
        .route("/config", get(get_config).put(update_config))
        .route("/plugins", get(list_plugins))
        .route(
            "/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route("/conversations/:id", get(get_conversation))
        .route("/conversations/:id/summary", get(get_conversation_summary))
        .route(
            "/conversations/:id/messages",
            get(list_messages).post(send_operator_message),
        )
        .route("/conversations/:id/turns", get(list_turns))
        .route("/turns/:id/tool-calls", get(list_turn_tool_calls))
        .route("/turns/:id/prompt", get(get_turn_prompt))
        .route("/agent/status", get(get_agent_status))
        .route("/agent/pause", put(set_pause))
        .route("/agent/toggle-pause", post(toggle_pause))
        .route("/agent/stop", post(stop_agent_turn))
        .route("/ws/events", get(ws_events_route))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    let app = Router::new().nest("/v1", protected);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("Failed to bind backend server to {}", bind_addr))?;
    tracing::info!("Ponderer backend listening on http://{}", bind_addr);
    axum::serve(listener, app)
        .await
        .context("Backend server failed")?;
    Ok(())
}

fn spawn_event_bridge(
    event_rx: flume::Receiver<AgentEvent>,
    ws_events: broadcast::Sender<ApiEventEnvelope>,
) {
    tokio::spawn(async move {
        while let Ok(event) = event_rx.recv_async().await {
            let envelope = map_agent_event(event);
            let _ = ws_events.send(envelope);
        }
    });
}

fn map_agent_event(event: AgentEvent) -> ApiEventEnvelope {
    match event {
        AgentEvent::StateChanged(state) => {
            envelope("state_changed", serde_json::json!({ "state": state }))
        }
        AgentEvent::Observation(text) => {
            envelope("observation", serde_json::json!({ "text": text }))
        }
        AgentEvent::ReasoningTrace(steps) => {
            envelope("reasoning_trace", serde_json::json!({ "steps": steps }))
        }
        AgentEvent::ToolCallProgress {
            conversation_id,
            tool_name,
            output_preview,
        } => envelope(
            "tool_call_progress",
            serde_json::json!({
                "conversation_id": conversation_id,
                "tool_name": tool_name,
                "output_preview": output_preview
            }),
        ),
        AgentEvent::ChatStreaming {
            conversation_id,
            content,
            done,
        } => envelope(
            "chat_streaming",
            serde_json::json!({
                "conversation_id": conversation_id,
                "content": content,
                "done": done
            }),
        ),
        AgentEvent::ActionTaken { action, result } => envelope(
            "action_taken",
            serde_json::json!({
                "action": action,
                "result": result
            }),
        ),
        AgentEvent::OrientationUpdate(orientation) => envelope(
            "orientation_update",
            serde_json::to_value(orientation).unwrap_or_else(|_| serde_json::json!({})),
        ),
        AgentEvent::JournalWritten(summary) => {
            envelope("journal_written", serde_json::json!({ "summary": summary }))
        }
        AgentEvent::ConcernCreated { id, summary } => envelope(
            "concern_created",
            serde_json::json!({ "id": id, "summary": summary }),
        ),
        AgentEvent::ConcernTouched { id, summary } => envelope(
            "concern_touched",
            serde_json::json!({ "id": id, "summary": summary }),
        ),
        AgentEvent::Error(error) => envelope("error", serde_json::json!({ "error": error })),
    }
}

fn envelope(event_type: &str, payload: serde_json::Value) -> ApiEventEnvelope {
    ApiEventEnvelope {
        event_type: event_type.to_string(),
        emitted_at: Utc::now(),
        payload,
    }
}

fn load_auth_config() -> Result<BackendAuthConfig> {
    let mode = parse_auth_mode(std::env::var("PONDERER_BACKEND_AUTH_MODE").ok())?;
    let token = std::env::var("PONDERER_BACKEND_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    if mode == AuthMode::Required && token.is_none() {
        return Err(anyhow!(
            "PONDERER_BACKEND_TOKEN is required when auth mode is 'required'"
        ));
    }
    if mode == AuthMode::Disabled {
        tracing::warn!("Backend auth mode is disabled; all API routes are unauthenticated");
    }

    Ok(BackendAuthConfig { mode, token })
}

fn parse_auth_mode(raw: Option<String>) -> Result<AuthMode> {
    let normalized = raw
        .unwrap_or_else(|| "required".to_string())
        .trim()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "" | "required" | "on" | "enabled" | "true" => Ok(AuthMode::Required),
        "disabled" | "off" | "false" => Ok(AuthMode::Disabled),
        other => Err(anyhow!(
            "Invalid PONDERER_BACKEND_AUTH_MODE '{}'. Expected 'required' or 'disabled'",
            other
        )),
    }
}

async fn auth_middleware(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    authorize(&headers, &state.auth)?;
    Ok(next.run(request).await)
}

fn authorize(headers: &HeaderMap, auth: &BackendAuthConfig) -> Result<(), StatusCode> {
    if auth.mode == AuthMode::Disabled {
        return Ok(());
    }
    let Some(token) = auth.token.as_deref() else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    let Some(raw_header) = headers.get(header::AUTHORIZATION) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let Ok(auth_value) = raw_header.to_str() else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let expected = format!("Bearer {}", token);
    if auth_value.trim() != expected {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn get_config(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<AgentConfig>, (StatusCode, String)> {
    let config = state.config.read().await.clone();
    Ok(Json(config))
}

async fn list_plugins(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Vec<BackendPluginManifest>>, (StatusCode, String)> {
    Ok(Json(state.plugin_manifests.clone()))
}

async fn update_config(
    State(state): State<Arc<ServerState>>,
    Json(new_config): Json<AgentConfig>,
) -> Result<Json<AgentConfig>, (StatusCode, String)> {
    if let Err(error) = new_config.save() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to save config: {error}"),
        ));
    }
    state.agent.reload_config(new_config.clone()).await;
    {
        let mut guard = state.config.write().await;
        *guard = new_config.clone();
    }
    Ok(Json(new_config))
}

async fn list_conversations(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<ListConversationsQuery>,
) -> Result<Json<Vec<ChatConversation>>, (StatusCode, String)> {
    let limit = clamp_limit(query.limit, 100, 1, 1000);
    state
        .db
        .list_chat_conversations(limit)
        .map(Json)
        .map_err(internal_error)
}

async fn get_conversation(
    State(state): State<Arc<ServerState>>,
    Path(conversation_id): Path<String>,
) -> Result<Json<ChatConversation>, (StatusCode, String)> {
    match state
        .db
        .get_chat_conversation(&conversation_id)
        .map_err(internal_error)?
    {
        Some(conversation) => Ok(Json(conversation)),
        None => Err(not_found(format!(
            "conversation '{}' not found",
            conversation_id
        ))),
    }
}

async fn create_conversation(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<CreateConversationRequest>,
) -> Result<Json<ChatConversation>, (StatusCode, String)> {
    state
        .db
        .create_chat_conversation(body.title.as_deref())
        .map(Json)
        .map_err(internal_error)
}

async fn get_conversation_summary(
    State(state): State<Arc<ServerState>>,
    Path(conversation_id): Path<String>,
) -> Result<Json<Option<ChatConversationSummary>>, (StatusCode, String)> {
    require_conversation(&state, &conversation_id)?;
    state
        .db
        .get_chat_conversation_summary(&conversation_id)
        .map(Json)
        .map_err(internal_error)
}

async fn list_messages(
    State(state): State<Arc<ServerState>>,
    Path(conversation_id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<ChatMessage>>, (StatusCode, String)> {
    require_conversation(&state, &conversation_id)?;
    let limit = clamp_limit(query.limit, 200, 1, 2000);
    state
        .db
        .get_chat_history_for_conversation(&conversation_id, limit)
        .map(Json)
        .map_err(internal_error)
}

async fn send_operator_message(
    State(state): State<Arc<ServerState>>,
    Path(conversation_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, (StatusCode, String)> {
    require_conversation(&state, &conversation_id)?;

    let content = body.content.trim();
    if content.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "content cannot be empty".to_string(),
        ));
    }

    let message_id = state
        .db
        .add_chat_message_in_conversation(&conversation_id, "operator", content)
        .map_err(internal_error)?;

    Ok(Json(SendMessageResponse {
        status: "queued",
        message_id,
    }))
}

async fn list_turns(
    State(state): State<Arc<ServerState>>,
    Path(conversation_id): Path<String>,
    Query(query): Query<ListTurnsQuery>,
) -> Result<Json<Vec<ChatTurn>>, (StatusCode, String)> {
    require_conversation(&state, &conversation_id)?;
    let limit = clamp_limit(query.limit, 100, 1, 1000);
    state
        .db
        .list_chat_turns_for_conversation(&conversation_id, limit)
        .map(Json)
        .map_err(internal_error)
}

async fn list_turn_tool_calls(
    State(state): State<Arc<ServerState>>,
    Path(turn_id): Path<String>,
) -> Result<Json<Vec<ChatTurnToolCall>>, (StatusCode, String)> {
    state
        .db
        .list_chat_turn_tool_calls(&turn_id)
        .map(Json)
        .map_err(internal_error)
}

async fn get_turn_prompt(
    State(state): State<Arc<ServerState>>,
    Path(turn_id): Path<String>,
) -> Result<Json<ChatTurnPromptResponse>, (StatusCode, String)> {
    let (prompt_text, system_prompt_text) = state
        .db
        .get_chat_turn_prompt_bundle(&turn_id)
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("turn {} not found", turn_id),
            )
        })?;

    Ok(Json(ChatTurnPromptResponse {
        turn_id,
        prompt_text: prompt_text.unwrap_or_default(),
        system_prompt_text,
    }))
}

async fn get_agent_status(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<AgentRuntimeStatus>, (StatusCode, String)> {
    Ok(Json(state.agent.runtime_status().await))
}

async fn set_pause(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<SetPauseRequest>,
) -> Result<Json<PauseStateResponse>, (StatusCode, String)> {
    let paused = state.agent.set_paused(body.paused).await;
    Ok(Json(PauseStateResponse { paused }))
}

async fn toggle_pause(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<PauseStateResponse>, (StatusCode, String)> {
    state.agent.toggle_pause().await;
    let status = state.agent.runtime_status().await;
    Ok(Json(PauseStateResponse {
        paused: status.paused,
    }))
}

async fn stop_agent_turn(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<StopResponse>, (StatusCode, String)> {
    state.agent.request_stop().await;
    Ok(Json(StopResponse { stopped: true }))
}

async fn ws_events_route(
    State(state): State<Arc<ServerState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_events_socket(state, socket))
}

async fn handle_events_socket(state: Arc<ServerState>, mut socket: WebSocket) {
    let mut rx = state.ws_events.subscribe();

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        let payload = match serde_json::to_string(&event) {
                            Ok(serialized) => serialized,
                            Err(error) => {
                                tracing::warn!("Failed to serialize websocket event: {}", error);
                                continue;
                            }
                        };
                        if socket.send(Message::Text(payload)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

fn require_conversation(
    state: &ServerState,
    conversation_id: &str,
) -> Result<ChatConversation, (StatusCode, String)> {
    state
        .db
        .get_chat_conversation(conversation_id)
        .map_err(internal_error)?
        .ok_or_else(|| not_found(format!("conversation '{}' not found", conversation_id)))
}

fn clamp_limit(value: Option<usize>, default: usize, min: usize, max: usize) -> usize {
    value.unwrap_or(default).clamp(min, max)
}

fn not_found(message: String) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, message)
}

fn internal_error(error: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn authorize_accepts_matching_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token-123"),
        );
        assert!(authorize(
            &headers,
            &BackendAuthConfig {
                mode: AuthMode::Required,
                token: Some("token-123".to_string()),
            }
        )
        .is_ok());
    }

    #[test]
    fn authorize_rejects_missing_or_invalid_token() {
        let headers = HeaderMap::new();
        assert!(authorize(
            &headers,
            &BackendAuthConfig {
                mode: AuthMode::Required,
                token: Some("token-123".to_string()),
            }
        )
        .is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong"),
        );
        assert!(authorize(
            &headers,
            &BackendAuthConfig {
                mode: AuthMode::Required,
                token: Some("token-123".to_string()),
            }
        )
        .is_err());
    }

    #[test]
    fn authorize_allows_when_auth_mode_disabled() {
        let headers = HeaderMap::new();
        assert!(authorize(
            &headers,
            &BackendAuthConfig {
                mode: AuthMode::Disabled,
                token: None,
            }
        )
        .is_ok());
    }

    #[test]
    fn parse_auth_mode_defaults_to_required() {
        assert!(matches!(parse_auth_mode(None).unwrap(), AuthMode::Required));
        assert!(matches!(
            parse_auth_mode(Some("required".to_string())).unwrap(),
            AuthMode::Required
        ));
        assert!(matches!(
            parse_auth_mode(Some("disabled".to_string())).unwrap(),
            AuthMode::Disabled
        ));
        assert!(parse_auth_mode(Some("nope".to_string())).is_err());
    }

    #[test]
    fn map_agent_event_includes_event_type_and_timestamp() {
        let envelope = map_agent_event(AgentEvent::Observation("hi".to_string()));
        assert_eq!(envelope.event_type, "observation");
        assert_eq!(envelope.payload["text"], "hi");
        assert!(envelope.emitted_at <= Utc::now());
    }
}
