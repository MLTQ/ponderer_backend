//! Agentic tool-calling loop.
//!
//! Replaces the single-shot "decide and act" pattern with a multi-step loop:
//! 1. Build context (system prompt + conversation + tool definitions)
//! 2. Call LLM with function-calling format
//! 3. If LLM returns tool calls, execute them
//! 4. Feed results back to LLM
//! 5. Loop until LLM returns final text or max iterations reached

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::http_client::build_http_client;

use super::safety;
use super::{ToolCall, ToolContext, ToolDef, ToolOutput, ToolRegistry};

/// Configuration for the agentic loop
#[derive(Debug, Clone)]
pub struct AgenticConfig {
    /// Maximum iterations before stopping. `None` means unlimited.
    pub max_iterations: Option<usize>,
    /// LLM API URL
    pub api_url: String,
    /// LLM model name
    pub model: String,
    /// Optional API key
    pub api_key: Option<String>,
    /// Temperature for LLM calls
    pub temperature: f32,
    /// Max tokens per LLM response
    pub max_tokens: u32,
    /// Shared generation counter used to cancel in-flight loops.
    /// If current value differs from `start_generation`, loop exits early.
    pub cancel_generation: Option<Arc<AtomicU64>>,
    /// Generation snapshot captured at loop start.
    pub start_generation: u64,
}

impl Default for AgenticConfig {
    fn default() -> Self {
        Self {
            max_iterations: Some(10),
            api_url: "http://localhost:11434/v1".to_string(),
            model: "llama3.2".to_string(),
            api_key: None,
            temperature: 0.7,
            max_tokens: 4096,
            cancel_generation: None,
            start_generation: 0,
        }
    }
}

/// A message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Tool call as returned by the LLM (OpenAI format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: LlmFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmFunctionCall {
    pub name: String,
    pub arguments: String, // JSON string
}

/// The outcome of running the agentic loop
#[derive(Debug, Clone)]
pub struct AgenticResult {
    /// Final text response from the LLM (if any)
    pub response: Option<String>,
    /// Extracted private reasoning blocks (from <think>/<thinking> tags)
    pub thinking_blocks: Vec<String>,
    /// All tool calls that were made during the loop
    pub tool_calls_made: Vec<ToolCallRecord>,
    /// Number of iterations used
    pub iterations: usize,
    /// Whether the loop hit the iteration limit
    pub hit_limit: bool,
}

/// Record of a tool call made during the loop
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub output: ToolOutput,
}

/// Per-token-ish metrics emitted while streaming assistant text.
///
/// When the provider exposes true token logprobs we forward them directly.
/// Otherwise `text` is derived from a lightweight local tokenizer over the
/// streamed text deltas so the UI still gets a novelty trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingTokenMetric {
    pub text: String,
    pub logprob: Option<f32>,
    pub entropy: Option<f32>,
    pub novelty: f32,
}

/// Incremental streaming update pushed to the caller.
#[derive(Debug, Clone)]
pub struct StreamingUpdate {
    pub content: String,
    pub done: bool,
    pub token_metrics: Vec<StreamingTokenMetric>,
}

#[derive(Debug, Clone)]
struct TokenEmission {
    text: String,
    logprob: Option<f32>,
    entropy: Option<f32>,
}

#[derive(Debug, Default)]
struct TokenNoveltyTracker {
    pending_fragment: String,
    total_tokens: u64,
    token_counts: HashMap<String, u64>,
    bigram_counts: HashMap<String, u64>,
    previous_token: Option<String>,
}

impl TokenNoveltyTracker {
    fn ingest_text_fragment(&mut self, fragment: &str) -> Vec<StreamingTokenMetric> {
        self.tokenize_fragment(fragment)
            .into_iter()
            .map(|text| {
                self.score_token(TokenEmission {
                    text,
                    logprob: None,
                    entropy: None,
                })
            })
            .collect()
    }

    fn ingest_provider_tokens(
        &mut self,
        tokens: Vec<TokenEmission>,
    ) -> Vec<StreamingTokenMetric> {
        tokens.into_iter().map(|token| self.score_token(token)).collect()
    }

    fn finish_pending(&mut self) -> Vec<StreamingTokenMetric> {
        if self.pending_fragment.trim().is_empty() {
            self.pending_fragment.clear();
            return Vec::new();
        }

        let text = std::mem::take(&mut self.pending_fragment);
        vec![self.score_token(TokenEmission {
            text,
            logprob: None,
            entropy: None,
        })]
    }

    fn tokenize_fragment(&mut self, fragment: &str) -> Vec<String> {
        let mut combined = String::new();
        combined.push_str(&self.pending_fragment);
        combined.push_str(fragment);
        self.pending_fragment.clear();

        let mut tokens = Vec::new();
        let mut current = String::new();
        for ch in combined.chars() {
            if is_metric_word_char(ch) {
                current.push(ch);
                continue;
            }

            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }

            if !ch.is_whitespace() {
                tokens.push(ch.to_string());
            }
        }

        if !current.is_empty() {
            if combined
                .chars()
                .last()
                .is_some_and(is_metric_word_char)
            {
                self.pending_fragment = current;
            } else {
                tokens.push(current);
            }
        }

        tokens
    }

    fn score_token(&mut self, token: TokenEmission) -> StreamingTokenMetric {
        let normalized = normalize_metric_token(&token.text);
        let seen_count = self.token_counts.get(&normalized).copied().unwrap_or(0) as f32;
        let total = self.total_tokens as f32;
        let vocab = self.token_counts.len() as f32 + 1.0;
        let smoothed_probability = (seen_count + 1.0) / (total + vocab);
        let frequency_novelty = (-smoothed_probability.ln() / 5.5).clamp(0.0, 1.0);

        let bigram_novelty = if let Some(previous) = self.previous_token.as_ref() {
            let key = bigram_key(previous, &normalized);
            let count = self.bigram_counts.get(&key).copied().unwrap_or(0) as f32;
            if count == 0.0 {
                1.0
            } else {
                (1.0 / (count + 1.0)).clamp(0.0, 1.0)
            }
        } else {
            0.55
        };

        let surprisal_score = token
            .logprob
            .map(|value| (-value / 5.0).clamp(0.0, 1.0))
            .unwrap_or(frequency_novelty);
        let entropy_score = token
            .entropy
            .map(|value| (value / 1.75).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let repeat_penalty = self
            .previous_token
            .as_ref()
            .filter(|previous| *previous == &normalized)
            .map(|_| 0.28)
            .unwrap_or(0.0);

        let novelty = (0.5 * frequency_novelty
            + 0.3 * surprisal_score
            + 0.15 * bigram_novelty
            + 0.05 * entropy_score
            - repeat_penalty)
            .clamp(0.0, 1.25);

        self.total_tokens += 1;
        *self.token_counts.entry(normalized.clone()).or_default() += 1;
        if let Some(previous) = self.previous_token.replace(normalized.clone()) {
            *self
                .bigram_counts
                .entry(bigram_key(&previous, &normalized))
                .or_default() += 1;
        }

        StreamingTokenMetric {
            text: token.text,
            logprob: token.logprob,
            entropy: token.entropy,
            novelty,
        }
    }
}

fn is_metric_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '\'' | '-')
}

fn normalize_metric_token(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.chars().any(char::is_alphanumeric) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

fn bigram_key(previous: &str, current: &str) -> String {
    format!("{previous}\u{1f}{current}")
}

fn approx_entropy_from_top_logprobs(value: Option<&serde_json::Value>) -> Option<f32> {
    let mut logprobs = Vec::new();
    for entry in value?.as_array()? {
        let Some(logprob) = entry.get("logprob").and_then(|item| item.as_f64()) else {
            continue;
        };
        logprobs.push(logprob as f32);
    }

    if logprobs.len() < 2 {
        return None;
    }

    let max_logprob = logprobs
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let mut weights = Vec::with_capacity(logprobs.len());
    let mut normalizer = 0.0f32;
    for logprob in logprobs {
        let weight = (logprob - max_logprob).exp();
        normalizer += weight;
        weights.push(weight);
    }
    if normalizer <= 0.0 {
        return None;
    }

    let entropy = weights
        .into_iter()
        .map(|weight| {
            let probability = weight / normalizer;
            -probability * probability.ln()
        })
        .sum::<f32>();
    Some(entropy)
}

fn parse_logprob_tokens(choice: &serde_json::Value) -> Vec<TokenEmission> {
    choice
        .get("logprobs")
        .and_then(|logprobs| logprobs.get("content"))
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let text = item.get("token").and_then(|value| value.as_str())?.trim();
                    if text.is_empty() {
                        return None;
                    }
                    Some(TokenEmission {
                        text: text.to_string(),
                        logprob: item
                            .get("logprob")
                            .and_then(|value| value.as_f64())
                            .map(|value| value as f32),
                        entropy: approx_entropy_from_top_logprobs(item.get("top_logprobs")),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn logprob_request_unsupported(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    (lower.contains("logprobs") || lower.contains("top_logprobs"))
        && (lower.contains("unsupported")
            || lower.contains("unknown")
            || lower.contains("invalid")
            || lower.contains("unexpected")
            || lower.contains("extra inputs"))
}

/// The agentic loop executor
pub struct AgenticLoop {
    config: AgenticConfig,
    registry: Arc<ToolRegistry>,
    client: reqwest::Client,
}

impl AgenticLoop {
    pub fn new(config: AgenticConfig, registry: Arc<ToolRegistry>) -> Self {
        Self {
            config,
            registry,
            client: build_http_client(),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.config
            .cancel_generation
            .as_ref()
            .map(|generation| generation.load(Ordering::SeqCst) != self.config.start_generation)
            .unwrap_or(false)
    }

    fn cancelled_result(
        &self,
        iterations: usize,
        tool_calls_made: Vec<ToolCallRecord>,
    ) -> AgenticResult {
        AgenticResult {
            response: Some("Stopped current turn at operator request.".to_string()),
            thinking_blocks: Vec::new(),
            tool_calls_made,
            iterations,
            hit_limit: false,
        }
    }

    fn cancelled_message(&self) -> Message {
        Message {
            role: "assistant".to_string(),
            content: Some("Stopped current turn at operator request.".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Run the agentic loop with the given system prompt and user message.
    ///
    /// The loop will continue until the LLM produces a final text response
    /// (no tool calls) or the maximum iteration count is reached.
    pub async fn run(
        &self,
        system_prompt: &str,
        user_message: &str,
        tool_ctx: &ToolContext,
    ) -> Result<AgenticResult> {
        self.run_with_history_internal(system_prompt, vec![], user_message, tool_ctx, None, None)
            .await
    }

    /// Run the agentic loop with existing conversation history.
    pub async fn run_with_history(
        &self,
        system_prompt: &str,
        history: Vec<Message>,
        user_message: &str,
        tool_ctx: &ToolContext,
    ) -> Result<AgenticResult> {
        self.run_with_history_internal(system_prompt, history, user_message, tool_ctx, None, None)
            .await
    }

    /// Run the agentic loop with existing conversation history and text streaming callback.
    pub async fn run_with_history_streaming(
        &self,
        system_prompt: &str,
        history: Vec<Message>,
        user_message: &str,
        tool_ctx: &ToolContext,
        on_text_stream: &dyn Fn(&StreamingUpdate),
    ) -> Result<AgenticResult> {
        self.run_with_history_streaming_and_tool_events(
            system_prompt,
            history,
            user_message,
            tool_ctx,
            on_text_stream,
            None,
        )
        .await
    }

    /// Run the agentic loop with existing conversation history while streaming text and tool events.
    pub async fn run_with_history_streaming_and_tool_events(
        &self,
        system_prompt: &str,
        history: Vec<Message>,
        user_message: &str,
        tool_ctx: &ToolContext,
        on_text_stream: &dyn Fn(&StreamingUpdate),
        on_tool_event: Option<&dyn Fn(&ToolCallRecord)>,
    ) -> Result<AgenticResult> {
        self.run_with_history_internal(
            system_prompt,
            history,
            user_message,
            tool_ctx,
            Some(on_text_stream),
            on_tool_event,
        )
        .await
    }

    async fn run_with_history_internal(
        &self,
        system_prompt: &str,
        history: Vec<Message>,
        user_message: &str,
        tool_ctx: &ToolContext,
        on_text_stream: Option<&dyn Fn(&StreamingUpdate)>,
        on_tool_event: Option<&dyn Fn(&ToolCallRecord)>,
    ) -> Result<AgenticResult> {
        // Build initial messages
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some(system_prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];

        // Add history
        messages.extend(history);

        // Add current user message
        messages.push(Message {
            role: "user".to_string(),
            content: Some(user_message.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

        // Get tool definitions
        let tool_defs = self.registry.tool_definitions_for_context(tool_ctx).await;

        let mut tool_calls_made = Vec::new();
        let mut iterations = 0;

        loop {
            if self.is_cancelled() {
                tracing::info!("Agentic loop cancelled by operator request");
                return Ok(self.cancelled_result(iterations, tool_calls_made));
            }
            iterations += 1;

            if let Some(max_iterations) = self.config.max_iterations {
                if iterations > max_iterations {
                    tracing::warn!("Agentic loop hit iteration limit ({})", max_iterations);
                    return Ok(AgenticResult {
                        response: Some(format!(
                            "[Reached maximum of {} tool-calling iterations]",
                            max_iterations
                        )),
                        thinking_blocks: Vec::new(),
                        tool_calls_made,
                        iterations: iterations - 1,
                        hit_limit: true,
                    });
                }
            }

            // Call LLM
            tracing::debug!("Agentic loop iteration {} — calling LLM", iterations);
            if let Some(callback) = on_text_stream {
                callback(&StreamingUpdate {
                    content: String::new(),
                    done: false,
                    token_metrics: Vec::new(),
                });
            }
            let llm_response = self
                .call_llm(&messages, &tool_defs, on_text_stream)
                .await
                .context("LLM call failed in agentic loop")?;

            // Check if LLM returned tool calls
            if let Some(ref tool_calls) = llm_response.tool_calls {
                if !tool_calls.is_empty() {
                    tracing::debug!("LLM requested {} tool call(s)", tool_calls.len());

                    // Add assistant message with tool calls to history
                    messages.push(llm_response.clone());

                    // Execute each tool call
                    for tc in tool_calls {
                        if self.is_cancelled() {
                            tracing::info!("Agentic loop cancelled before tool execution");
                            return Ok(self.cancelled_result(iterations, tool_calls_made));
                        }
                        let arguments: serde_json::Value =
                            serde_json::from_str(&tc.function.arguments).unwrap_or_else(|e| {
                                tracing::warn!("Failed to parse tool arguments as JSON: {}", e);
                                serde_json::json!({})
                            });

                        // Validate input
                        match safety::validate_input(&arguments) {
                            safety::SafetyVerdict::Block(reason) => {
                                let output = ToolOutput::Error(format!(
                                    "Input validation failed: {}",
                                    reason
                                ));
                                tool_calls_made.push(ToolCallRecord {
                                    tool_name: tc.function.name.clone(),
                                    arguments: arguments.clone(),
                                    output: output.clone(),
                                });
                                messages.push(Message {
                                    role: "tool".to_string(),
                                    content: Some(output.to_llm_string()),
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                });
                                continue;
                            }
                            safety::SafetyVerdict::Warn(reason) => {
                                tracing::warn!(
                                    "Safety warning for {}: {}",
                                    tc.function.name,
                                    reason
                                );
                            }
                            safety::SafetyVerdict::Allow => {}
                        }

                        // Execute tool
                        let call = ToolCall {
                            name: tc.function.name.clone(),
                            arguments: arguments.clone(),
                        };

                        let result = self.registry.execute_call(&call, tool_ctx).await;

                        // Run output through safety pipeline
                        let safe_output = match &result.output {
                            ToolOutput::Text(text) => {
                                match safety::check_output(&tc.function.name, text) {
                                    Ok(sanitized) => sanitized,
                                    Err(reason) => {
                                        format!("[BLOCKED] {}", reason)
                                    }
                                }
                            }
                            ToolOutput::Json(val) => {
                                let text = serde_json::to_string_pretty(val)
                                    .unwrap_or_else(|_| val.to_string());
                                match safety::check_output(&tc.function.name, &text) {
                                    Ok(sanitized) => sanitized,
                                    Err(reason) => format!("[BLOCKED] {}", reason),
                                }
                            }
                            other => other.to_llm_string(),
                        };

                        let record = ToolCallRecord {
                            tool_name: tc.function.name.clone(),
                            arguments,
                            output: result.output,
                        };
                        if let Some(callback) = on_tool_event {
                            callback(&record);
                        }
                        tool_calls_made.push(record);

                        // Add tool result message
                        messages.push(Message {
                            role: "tool".to_string(),
                            content: Some(safe_output),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                        });
                    }

                    // Continue loop — LLM will see tool results
                    if let Some(callback) = on_text_stream {
                        callback(&StreamingUpdate {
                            content: String::new(),
                            done: true,
                            token_metrics: Vec::new(),
                        });
                    }
                    continue;
                }
            }

            // No tool calls — LLM produced final text response
            let (response_text, thinking_blocks) = llm_response
                .content
                .as_deref()
                .map(split_visible_and_thinking)
                .map(|(visible, thinking)| (Some(visible), thinking))
                .unwrap_or_else(|| (None, Vec::new()));
            tracing::debug!("Agentic loop completed in {} iteration(s)", iterations);

            return Ok(AgenticResult {
                response: response_text,
                thinking_blocks,
                tool_calls_made,
                iterations,
                hit_limit: false,
            });
        }
    }

    /// Call the LLM with the current messages and tool definitions.
    async fn call_llm(
        &self,
        messages: &[Message],
        tool_defs: &[ToolDef],
        on_text_stream: Option<&dyn Fn(&StreamingUpdate)>,
    ) -> Result<Message> {
        if on_text_stream.is_some() {
            match self
                .call_llm_streaming(messages, tool_defs, on_text_stream)
                .await
            {
                Ok(message) => {
                    // Some LLM servers silently drop tool_calls in streaming responses even
                    // when the model intended to call a tool.  Only apply the fallback on
                    // the first LLM call of a loop iteration (where the last message is
                    // from the user/system), because subsequent calls (after tool results)
                    // are producing a final text response and won't have tool_calls.
                    let has_tool_calls =
                        message.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty());
                    let has_visible_text = message
                        .content
                        .as_deref()
                        .map(str::trim)
                        .is_some_and(|text| !text.is_empty());
                    let last_role = messages.last().map(|m| m.role.as_str()).unwrap_or("");
                    let is_first_call = last_role == "user" || last_role == "system";
                    if has_tool_calls || has_visible_text || tool_defs.is_empty() || !is_first_call
                    {
                        return Ok(message);
                    }
                    // Streaming returned no tool_calls on the initial user-facing call.
                    // Retry with non-streaming; if it finds tool_calls, streaming silently
                    // dropped them.  If it also returns no tool_calls the response is a
                    // legitimate text-only reply — return the streaming version so the
                    // text content already streamed to the caller stays consistent.
                    let streaming_message = message;
                    tracing::debug!(
                        "Streaming returned no tool_calls on initial call; \
                         retrying non-streaming to check for function calls"
                    );
                    let ns_message = self.call_llm_non_streaming(messages, tool_defs).await?;
                    let ns_has_tool_calls = ns_message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|tc| !tc.is_empty());
                    if ns_has_tool_calls {
                        tracing::info!(
                            "Non-streaming fallback found tool_calls that streaming dropped"
                        );
                        return Ok(ns_message);
                    }
                    // Both paths agree: no tool_calls.  Return the streaming message to
                    // keep the streamed text consistent with what the caller already saw.
                    return Ok(streaming_message);
                }
                Err(e) => {
                    tracing::warn!(
                        "Streaming LLM call failed, falling back to non-streaming: {}",
                        e
                    );
                }
            }
        }

        let message = self.call_llm_non_streaming(messages, tool_defs).await?;
        if let Some(callback) = on_text_stream {
            if let Some(content) = message.content.as_deref() {
                if !content.is_empty() {
                    let mut tracker = TokenNoveltyTracker::default();
                    let mut token_metrics = tracker.ingest_text_fragment(content);
                    token_metrics.extend(tracker.finish_pending());
                    callback(&StreamingUpdate {
                        content: content.to_string(),
                        done: true,
                        token_metrics,
                    });
                }
            }
        }
        Ok(message)
    }

    async fn call_llm_non_streaming(
        &self,
        messages: &[Message],
        tool_defs: &[ToolDef],
    ) -> Result<Message> {
        if self.is_cancelled() {
            return Ok(self.cancelled_message());
        }
        let url = format!("{}/chat/completions", self.config.api_url);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": self.config.temperature,
            "max_tokens": self.config.max_tokens,
        });

        // Only include tools if we have any
        if !tool_defs.is_empty() {
            body["tools"] = serde_json::to_value(tool_defs)?;
        }

        let mut req = self.client.post(&url).json(&body);

        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let response = req.send().await.context("Failed to send LLM request")?;
        if self.is_cancelled() {
            return Ok(self.cancelled_message());
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {}: {}", status, body);
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse LLM response")?;

        // Extract the assistant message from the response
        let choice = response_json["choices"]
            .as_array()
            .and_then(|arr| arr.first())
            .context("Empty choices in LLM response")?;

        let message = &choice["message"];

        // Parse into our Message type
        let content = message["content"].as_str().map(String::from);

        let tool_calls: Option<Vec<LlmToolCall>> = message
            .get("tool_calls")
            .and_then(|tc| serde_json::from_value(tc.clone()).ok());

        Ok(Message {
            role: "assistant".to_string(),
            content,
            tool_calls,
            tool_call_id: None,
        })
    }

    async fn call_llm_streaming(
        &self,
        messages: &[Message],
        tool_defs: &[ToolDef],
        on_text_stream: Option<&dyn Fn(&StreamingUpdate)>,
    ) -> Result<Message> {
        #[derive(Debug, Clone, Default)]
        struct ToolCallAccumulator {
            id: String,
            call_type: String,
            name: String,
            arguments: String,
        }

        if self.is_cancelled() {
            return Ok(self.cancelled_message());
        }
        let url = format!("{}/chat/completions", self.config.api_url);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": self.config.temperature,
            "max_tokens": self.config.max_tokens,
            "stream": true,
        });

        if !tool_defs.is_empty() {
            body["tools"] = serde_json::to_value(tool_defs)?;
        }

        let mut body_with_metrics = body.clone();
        body_with_metrics["logprobs"] = serde_json::json!(true);
        body_with_metrics["top_logprobs"] = serde_json::json!(5);

        let mut response = match self.send_streaming_request(&url, &body_with_metrics).await {
            Ok(response) => response,
            Err(error) if logprob_request_unsupported(&error.to_string()) => {
                tracing::debug!(
                    "Streaming provider rejected logprob request; retrying without token logprobs: {}",
                    error
                );
                self.send_streaming_request(&url, &body).await?
            }
            Err(error) => return Err(error),
        };
        if self.is_cancelled() {
            return Ok(self.cancelled_message());
        }

        let mut content = String::new();
        let mut tool_calls: Vec<ToolCallAccumulator> = Vec::new();
        let mut line_buffer = String::new();
        let mut saw_done = false;
        let mut novelty_tracker = TokenNoveltyTracker::default();

        while let Some(chunk) = response
            .chunk()
            .await
            .context("Failed reading streaming chunk")?
        {
            if self.is_cancelled() {
                if let Some(callback) = on_text_stream {
                    callback(&StreamingUpdate {
                        content: String::new(),
                        done: true,
                        token_metrics: Vec::new(),
                    });
                }
                return Ok(self.cancelled_message());
            }
            line_buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(newline_idx) = line_buffer.find('\n') {
                let line = line_buffer[..newline_idx].trim().to_string();
                line_buffer = line_buffer[newline_idx + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                if !line.starts_with("data:") {
                    continue;
                }

                let payload = line[5..].trim();
                if payload == "[DONE]" {
                    saw_done = true;
                    break;
                }

                let chunk_json: serde_json::Value = serde_json::from_str(payload)
                    .with_context(|| format!("Failed to parse stream payload: {}", payload))?;

                let Some(choice) = chunk_json["choices"].as_array().and_then(|arr| arr.first())
                else {
                    continue;
                };

                let token_metrics = parse_logprob_tokens(choice);
                if let Some(delta_content) = choice["delta"]["content"].as_str() {
                    content.push_str(delta_content);
                    let token_metrics = if token_metrics.is_empty() {
                        novelty_tracker.ingest_text_fragment(delta_content)
                    } else {
                        novelty_tracker.ingest_provider_tokens(token_metrics)
                    };
                    if let Some(callback) = on_text_stream {
                        callback(&StreamingUpdate {
                            content: content.clone(),
                            done: false,
                            token_metrics,
                        });
                    }
                } else if !token_metrics.is_empty() {
                    let token_metrics = novelty_tracker.ingest_provider_tokens(token_metrics);
                    if let Some(callback) = on_text_stream {
                        callback(&StreamingUpdate {
                            content: content.clone(),
                            done: false,
                            token_metrics,
                        });
                    }
                }

                if let Some(tc_deltas) = choice["delta"]["tool_calls"].as_array() {
                    for tc_delta in tc_deltas {
                        let idx = tc_delta
                            .get("index")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(tool_calls.len() as u64)
                            as usize;

                        while tool_calls.len() <= idx {
                            tool_calls.push(ToolCallAccumulator::default());
                        }
                        let acc = &mut tool_calls[idx];

                        if let Some(id) = tc_delta.get("id").and_then(|v| v.as_str()) {
                            acc.id = id.to_string();
                        }
                        if let Some(call_type) = tc_delta.get("type").and_then(|v| v.as_str()) {
                            acc.call_type = call_type.to_string();
                        }
                        if let Some(name_part) = tc_delta
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                        {
                            acc.name.push_str(name_part);
                        }
                        if let Some(args_part) = tc_delta
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                        {
                            acc.arguments.push_str(args_part);
                        }
                    }
                }
            }

            if saw_done {
                break;
            }
        }

        let final_token_metrics = novelty_tracker.finish_pending();
        if let Some(callback) = on_text_stream {
            callback(&StreamingUpdate {
                content: content.clone(),
                done: true,
                token_metrics: final_token_metrics,
            });
        }

        let parsed_tool_calls = tool_calls
            .into_iter()
            .enumerate()
            .filter_map(|(idx, tc)| {
                let name = tc.name.trim().to_string();
                if name.is_empty() {
                    return None;
                }
                Some(LlmToolCall {
                    id: if tc.id.trim().is_empty() {
                        format!("stream_tool_call_{}", idx)
                    } else {
                        tc.id
                    },
                    call_type: if tc.call_type.trim().is_empty() {
                        "function".to_string()
                    } else {
                        tc.call_type
                    },
                    function: LlmFunctionCall {
                        name,
                        arguments: if tc.arguments.trim().is_empty() {
                            "{}".to_string()
                        } else {
                            tc.arguments
                        },
                    },
                })
            })
            .collect::<Vec<_>>();

        Ok(Message {
            role: "assistant".to_string(),
            content: if content.is_empty() {
                None
            } else {
                Some(content)
            },
            tool_calls: if parsed_tool_calls.is_empty() {
                None
            } else {
                Some(parsed_tool_calls)
            },
            tool_call_id: None,
        })
    }

    async fn send_streaming_request(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response> {
        let mut req = self.client.post(url).json(body);
        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let response = req
            .send()
            .await
            .context("Failed to send streaming LLM request")?;
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Streaming LLM API error {}: {}", status, body);
    }
}

fn split_visible_and_thinking(input: &str) -> (String, Vec<String>) {
    fn extract_tag(text: String, open_tag: &str, close_tag: &str) -> (String, Vec<String>) {
        let mut rest = text;
        let mut thoughts = Vec::new();

        while let Some(start) = rest.find(open_tag) {
            let content_start = start + open_tag.len();
            if let Some(rel_end) = rest[content_start..].find(close_tag) {
                let end = content_start + rel_end;
                let thought = rest[content_start..end].trim();
                if !thought.is_empty() {
                    thoughts.push(thought.to_string());
                }
                let remove_end = end + close_tag.len();
                rest.replace_range(start..remove_end, "");
            } else {
                let thought = rest[content_start..].trim();
                if !thought.is_empty() {
                    thoughts.push(thought.to_string());
                }
                rest.replace_range(start..rest.len(), "");
            }
        }

        (rest, thoughts)
    }

    let (without_thinking_tag, mut thoughts_a) =
        extract_tag(input.to_string(), "<thinking>", "</thinking>");
    let (without_think_tag, mut thoughts_b) =
        extract_tag(without_thinking_tag, "<think>", "</think>");
    thoughts_a.append(&mut thoughts_b);

    (without_think_tag.trim().to_string(), thoughts_a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = Message {
            role: "user".to_string(),
            content: Some("Hello".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };

        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
        // tool_calls should be absent (skip_serializing_if = None)
        assert!(json.get("tool_calls").is_none());
    }

    #[test]
    fn test_tool_call_message_serialization() {
        let msg = Message {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![LlmToolCall {
                id: "call_123".to_string(),
                call_type: "function".to_string(),
                function: LlmFunctionCall {
                    name: "shell".to_string(),
                    arguments: r#"{"command": "ls"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
        };

        let json = serde_json::to_value(&msg).unwrap();
        assert!(json.get("tool_calls").is_some());
        assert_eq!(json["tool_calls"][0]["function"]["name"], "shell");
    }

    #[test]
    fn test_tool_result_message_serialization() {
        let msg = Message {
            role: "tool".to_string(),
            content: Some("file1.txt\nfile2.txt".to_string()),
            tool_calls: None,
            tool_call_id: Some("call_123".to_string()),
        };

        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_123");
    }

    #[test]
    fn test_agentic_config_default() {
        let config = AgenticConfig::default();
        assert_eq!(config.max_iterations, Some(10));
        assert_eq!(config.temperature, 0.7);
    }

    #[test]
    fn strips_thinking_blocks_from_visible_response() {
        let (visible, thoughts) =
            split_visible_and_thinking("<think>internal chain</think>\nHello there");
        assert_eq!(visible, "Hello there");
        assert_eq!(thoughts, vec!["internal chain"]);
    }

    #[test]
    fn strips_both_think_tag_variants() {
        let (visible, thoughts) =
            split_visible_and_thinking("<thinking>plan</thinking>\n<think>detail</think>\nDone");
        assert_eq!(visible, "Done");
        assert_eq!(thoughts, vec!["plan", "detail"]);
    }
}
