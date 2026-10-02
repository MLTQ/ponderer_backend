use anyhow::{Context, Result};
use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::generation_telemetry::{
    GenerationObserver, GenerationOutcome, GenerationSession, TokenNoveltyTracker,
};
use crate::http_client::build_llm_http_client;

const VISION_MAX_DIMENSION: u32 = 1280;
const VISION_MAX_BYTES_MULTIMODAL: usize = 512 * 1024;
const VISION_MAX_BYTES_INLINE_FALLBACK: usize = 64 * 1024;

#[derive(Clone)]
pub struct LlmClient {
    api_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
    generation_observer: Option<GenerationObserver>,
}

#[derive(Debug, Deserialize)]
pub struct DecisionResponse {
    pub should_respond: bool,
    pub reasoning: String,
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

impl LlmClient {
    pub fn new(api_url: String, api_key: String, model: String) -> Self {
        let client = build_llm_http_client(&api_url, &model);
        Self {
            api_url,
            api_key,
            model,
            client,
            generation_observer: None,
        }
    }

    pub fn with_generation_observer(mut self, observer: GenerationObserver) -> Self {
        self.generation_observer = Some(observer);
        self
    }

    /// Generate a completion using the OpenAI API format
    pub async fn generate(&self, messages: Vec<Message>) -> Result<String> {
        self.generate_with_model(messages, &self.model).await
    }

    /// Generate a completion with a specific model
    pub async fn generate_with_model(&self, messages: Vec<Message>, model: &str) -> Result<String> {
        let mut session = self.begin_generation();
        let url = chat_completions_url(&self.api_url);

        let mut request = ChatCompletionRequest {
            model: model.to_string(),
            messages,
            temperature: Some(0.7),
            max_tokens: Some(2000),
            stream: session.is_some(),
        };

        let mut req = self.client.post(&url).json(&request);

        // Add API key header if provided (not needed for local models)
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let mut response = req.send().await.context("Failed to send LLM request")?;
        if request.stream && response.status() == reqwest::StatusCode::BAD_REQUEST {
            let body = response.text().await.unwrap_or_default();
            let lower = body.to_lowercase();
            if lower.contains("stream")
                && (lower.contains("not supported") || lower.contains("unsupported"))
            {
                request.stream = false;
                let mut fallback = self.client.post(&url).json(&request);
                if !self.api_key.is_empty() {
                    fallback = fallback.bearer_auth(&self.api_key);
                }
                response = fallback
                    .send()
                    .await
                    .context("Failed to send non-streaming LLM fallback")?;
            } else {
                anyhow::bail!("LLM API returned error 400: {body}");
            }
        }

        // Check for HTTP errors and include response body for debugging
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read body".to_string());
            anyhow::bail!("LLM API returned error {}: {}", status, body);
        }

        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return Self::read_text_stream(response, session.as_mut()).await;
        }
        let completion: Value = response
            .json()
            .await
            .context("Failed to parse LLM response")?;
        let message = &completion["choices"][0]["message"];
        if let Some(observer) = session.as_ref() {
            observer.emit_assistant_output(message);
        }
        let content = message["content"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("No response from LLM"))?;
        if let Some(observer) = session.as_mut() {
            let mut tracker = TokenNoveltyTracker::default();
            observer.emit_samples(tracker.ingest_text_fragment(&content));
            observer.emit_samples(tracker.finish_pending());
            observer.finish(GenerationOutcome::Completed);
        }

        Ok(content)
    }

    async fn read_text_stream(
        mut response: reqwest::Response,
        mut session: Option<&mut GenerationSession>,
    ) -> Result<String> {
        let mut pending = Vec::new();
        let mut content = String::new();
        let mut tracker = TokenNoveltyTracker::default();
        let mut done = false;
        let mut finished = false;
        while let Some(chunk) = response
            .chunk()
            .await
            .context("Failed reading LLM output stream")?
        {
            pending.extend_from_slice(&chunk);
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                let line = std::str::from_utf8(&pending[..end])
                    .context("Invalid UTF-8 in LLM SSE line")?
                    .trim()
                    .to_string();
                pending.drain(..=end);
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                if data == "[DONE]" {
                    done = true;
                    break;
                }
                let value: Value = serde_json::from_str(data).context("Invalid LLM SSE JSON")?;
                finished |= value["choices"][0]["finish_reason"].is_string();
                let delta = &value["choices"][0]["delta"];
                if let Some(observer) = session.as_ref() {
                    observer.emit_assistant_output(delta);
                }
                if let Some(text) = delta["content"].as_str() {
                    content.push_str(text);
                    if let Some(observer) = session.as_ref() {
                        observer.emit_samples(tracker.ingest_text_fragment(text));
                    }
                }
            }
            if done {
                break;
            }
        }
        if !done && !finished {
            anyhow::bail!("LLM stream closed before a completion marker");
        }
        if content.is_empty() {
            anyhow::bail!("LLM stream ended without textual content");
        }
        if let Some(observer) = session.as_mut() {
            observer.emit_samples(tracker.finish_pending());
            observer.finish(GenerationOutcome::Completed);
        }
        Ok(content)
    }

    /// Ask the LLM to decide whether to respond to a post
    pub async fn decide_to_respond(
        &self,
        messages: Vec<Message>,
        decision_model: Option<&str>,
    ) -> Result<DecisionResponse> {
        let model = decision_model.unwrap_or(&self.model);
        self.generate_json(messages, Some(model)).await
    }

    /// Generate a JSON response using the LLM
    pub async fn generate_json<T>(&self, messages: Vec<Message>, model: Option<&str>) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let model = model.unwrap_or(&self.model);
        let response = self.generate_with_model(messages, model).await?;
        self.parse_json::<T>(&response)
            .context("Failed to parse JSON response")
    }

    /// Evaluate an image with vision model
    pub async fn evaluate_image(
        &self,
        image_bytes: &[u8],
        prompt: &str,
        context: &str,
    ) -> Result<ImageEvaluation> {
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: "You are evaluating AI-generated images. \
                         Determine if the image matches the intended prompt and context, and suggest \
                         improvements if needed.".to_string(),
            },
            Message {
                role: "user".to_string(),
                content: format!(
                    "Context: {}\n\n\
                     Image Prompt: {}\n\n\
                     Evaluate this generated image. Does it match the prompt and fit the context? \
                     Should we use it, or generate a better one?\n\n\
                     Respond with JSON:\n\
                     {{\n  \
                       \"satisfactory\": true/false,\n  \
                       \"reasoning\": \"explanation\",\n  \
                       \"suggested_prompt_refinement\": \"improved prompt if not satisfactory, null otherwise\"\n\
                     }}",
                    context,
                    prompt
                ),
            },
        ];

        let (processed_bytes, mime_type) =
            preprocess_image_for_vision(image_bytes, VISION_MAX_BYTES_MULTIMODAL)
                .context("Failed to preprocess image for vision request")?;
        let image_base64 = base64::engine::general_purpose::STANDARD.encode(&processed_bytes);

        let response = self
            .generate_vision_with_image(messages.clone(), &image_base64, mime_type)
            .await?;
        match self.parse_json::<ImageEvaluation>(&response) {
            Ok(parsed) => Ok(parsed),
            Err(primary_error) => {
                // Fallback for providers that don't support multimodal message content arrays.
                let fallback_response = self
                    .generate_vision_with_image_legacy_inline(
                        messages,
                        image_bytes,
                        VISION_MAX_BYTES_INLINE_FALLBACK,
                    )
                    .await
                    .context("Legacy inline vision fallback failed")?;
                self.parse_json::<ImageEvaluation>(&fallback_response)
                    .context(format!(
                        "Failed to parse vision JSON response (multimodal parse error: {})",
                        primary_error
                    ))
            }
        }
    }

    async fn generate_vision_with_image(
        &self,
        messages: Vec<Message>,
        image_base64: &str,
        mime_type: &str,
    ) -> Result<String> {
        let session = self.begin_generation();
        let url = chat_completions_url(&self.api_url);

        let mut request_messages = Vec::with_capacity(messages.len().max(1));
        let mut image_attached = false;
        for (idx, msg) in messages.iter().enumerate() {
            let is_last = idx + 1 == messages.len();
            if is_last && msg.role.eq_ignore_ascii_case("user") {
                image_attached = true;
                request_messages.push(json!({
                    "role": msg.role,
                    "content": [
                        { "type": "text", "text": msg.content },
                        {
                            "type": "image_url",
                            "image_url": { "url": format!("data:{};base64,{}", mime_type, image_base64) }
                        }
                    ]
                }));
            } else {
                request_messages.push(json!({
                    "role": msg.role,
                    "content": msg.content
                }));
            }
        }

        if !image_attached {
            request_messages.push(json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "Evaluate this image." },
                    {
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", mime_type, image_base64) }
                    }
                ]
            }));
        }

        let request = json!({
            "model": self.model,
            "messages": request_messages,
            "temperature": 0.2,
            "max_tokens": 1000
        });

        let mut req = self.client.post(&url).json(&request);

        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let response = req.send().await.context("Failed to send vision request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read body".to_string());
            anyhow::bail!("Vision API returned error {}: {}", status, body);
        }

        let completion: Value = response
            .json()
            .await
            .context("Failed to parse vision response")?;
        let message = completion
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .ok_or_else(|| anyhow::anyhow!("No response from vision model"))?;
        let content = extract_message_content(message.get("content"))
            .ok_or_else(|| anyhow::anyhow!("No textual content from vision model"))?;

        Self::complete_generation(session, &content);

        Ok(content)
    }

    async fn generate_vision_with_image_legacy_inline(
        &self,
        messages: Vec<Message>,
        original_image_bytes: &[u8],
        max_inline_bytes: usize,
    ) -> Result<String> {
        let session = self.begin_generation();
        let (processed_bytes, _) =
            preprocess_image_for_vision(original_image_bytes, max_inline_bytes)
                .context("Failed to preprocess image for inline fallback")?;
        let image_base64 = base64::engine::general_purpose::STANDARD.encode(processed_bytes);

        let mut messages = messages;
        if let Some(last_msg) = messages.last_mut() {
            last_msg.content = format!("[IMAGE_BASE64: {}]\n\n{}", image_base64, last_msg.content);
        }

        let url = chat_completions_url(&self.api_url);
        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages,
            temperature: Some(0.2),
            max_tokens: Some(1000),
            stream: false,
        };

        let mut req = self.client.post(&url).json(&request);
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let response = req
            .send()
            .await
            .context("Failed to send inline fallback vision request")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Inline fallback vision API error {}: {}", status, body);
        }

        let completion: ChatCompletionResponse = response
            .json()
            .await
            .context("Failed to parse inline fallback vision response")?;

        let content = completion
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| anyhow::anyhow!("No response from inline fallback vision model"))?;
        Self::complete_generation(session, &content);
        Ok(content)
    }

    fn begin_generation(&self) -> Option<GenerationSession> {
        self.generation_observer
            .as_ref()
            .map(GenerationObserver::start)
    }

    fn complete_generation(mut session: Option<GenerationSession>, content: &str) {
        let Some(session) = session.as_mut() else {
            return;
        };
        session.finish_with_text(content);
    }

    fn parse_json<T>(&self, response: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        parse_json_response(response)
    }
}

fn parse_json_response<T>(response: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    for candidate in json_parse_candidates(response) {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }

        if let Ok(parsed) = serde_json::from_str::<T>(candidate) {
            return Ok(parsed);
        }

        // Some providers double-encode JSON as a JSON string; decode and retry.
        if let Ok(inner_json) = serde_json::from_str::<String>(candidate) {
            if let Ok(parsed) = serde_json::from_str::<T>(inner_json.trim()) {
                return Ok(parsed);
            }
        }
    }

    let cleaned = strip_reasoning_wrappers(response);
    let fallback = extract_balanced_json_value(cleaned).unwrap_or(cleaned);
    serde_json::from_str::<T>(fallback.trim()).context(format!(
        "Failed to parse JSON. Extracted: {} | Original: {}",
        fallback,
        response.chars().take(500).collect::<String>()
    ))
}

fn json_parse_candidates(response: &str) -> Vec<&str> {
    let mut candidates = Vec::new();
    let cleaned = strip_reasoning_wrappers(response);

    candidates.push(response);
    candidates.push(cleaned);

    if let Some(fenced) = extract_first_code_fence_body(cleaned) {
        candidates.push(fenced);
    }

    if let Some(extracted) = extract_balanced_json_value(cleaned) {
        candidates.push(extracted);
    }

    if let Some(fenced) = extract_first_code_fence_body(cleaned) {
        if let Some(extracted) = extract_balanced_json_value(fenced) {
            candidates.push(extracted);
        }
    }

    candidates
}

fn strip_reasoning_wrappers(text: &str) -> &str {
    let mut cleaned = text.trim();
    for close_tag in ["</thinking>", "</think>"] {
        if let Some(idx) = cleaned.rfind(close_tag) {
            cleaned = cleaned[idx + close_tag.len()..].trim();
        }
    }
    cleaned
}

fn extract_first_code_fence_body(text: &str) -> Option<&str> {
    let start = text.find("```")?;
    let after_start = &text[start + 3..];
    let end_rel = after_start.find("```")?;
    let inner = after_start[..end_rel].trim();
    if inner.is_empty() {
        return None;
    }

    let first = inner.lines().next().unwrap_or_default().trim();
    let first_lower = first.to_ascii_lowercase();
    if first_lower == "json" || first_lower == "jsonc" {
        let newline_idx = inner.find('\n')?;
        let trimmed = inner[newline_idx + 1..].trim();
        (!trimmed.is_empty()).then_some(trimmed)
    } else {
        Some(inner)
    }
}

fn extract_balanced_json_value(text: &str) -> Option<&str> {
    let mut start_idx = None;
    for (idx, ch) in text.char_indices() {
        if ch == '{' || ch == '[' {
            start_idx = Some(idx);
            break;
        }
    }
    let start = start_idx?;
    let slice = &text[start..];

    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for (rel_idx, ch) in slice.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                let expected = stack.pop()?;
                if ch != expected {
                    return None;
                }
                if stack.is_empty() {
                    let end = start + rel_idx + ch.len_utf8();
                    return Some(&text[start..end]);
                }
            }
            _ => {}
        }
    }

    None
}

fn chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{}/chat/completions", trimmed)
    } else {
        format!("{}/v1/chat/completions", trimmed)
    }
}

fn preprocess_image_for_vision(
    image_bytes: &[u8],
    max_bytes: usize,
) -> Result<(Vec<u8>, &'static str)> {
    let original = image::load_from_memory(image_bytes)
        .context("Unable to decode image bytes for vision preprocessing")?;
    let rgba = original.to_rgba8();
    let mut current = image::DynamicImage::ImageRgba8(rgba);
    if current.width().max(current.height()) > VISION_MAX_DIMENSION {
        current = current.resize(
            VISION_MAX_DIMENSION,
            VISION_MAX_DIMENSION,
            FilterType::Triangle,
        );
    }

    let mut best: Option<Vec<u8>> = None;
    for scale_step in 0..5u32 {
        let scaled = if scale_step == 0 {
            current.clone()
        } else {
            let factor = 0.8_f32.powi(scale_step as i32);
            let width = ((current.width() as f32) * factor).round().max(320.0) as u32;
            let height = ((current.height() as f32) * factor).round().max(320.0) as u32;
            current.resize(width, height, FilterType::Triangle)
        };

        for quality in [80u8, 70, 60, 50, 40, 30] {
            let encoded = encode_jpeg(&scaled, quality)
                .context("Failed to encode image as JPEG for vision preprocessing")?;
            if best.as_ref().is_none_or(|b| encoded.len() < b.len()) {
                best = Some(encoded.clone());
            }
            if encoded.len() <= max_bytes {
                return Ok((encoded, "image/jpeg"));
            }
        }
    }

    let best = best.ok_or_else(|| anyhow::anyhow!("Unable to preprocess image for vision"))?;
    Ok((best, "image/jpeg"))
}

fn encode_jpeg(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
    encoder
        .encode_image(image)
        .context("JPEG encoding failed")?;
    Ok(out)
}

fn extract_message_content(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(items) = content.as_array() {
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ImageEvaluation {
    pub satisfactory: bool,
    pub reasoning: String,
    pub suggested_prompt_refinement: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{chat_completions_url, extract_message_content, parse_json_response};
    use serde::Deserialize;
    use serde_json::json;

    #[tokio::test]
    async fn background_generation_streams_raw_output_and_handles_explicit_stream_rejection() {
        use crate::generation_telemetry::{GenerationEvent, GenerationObserver, GenerationSource};
        use axum::{response::IntoResponse, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        for fallback in [false, true] {
            let router = Router::new().route(
                "/v1/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| async move {
                    if body["stream"] == true && fallback {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            "stream is not supported",
                        )
                            .into_response();
                    }
                    let text = "  灯 café\n";
                    if body["stream"] == true {
                        (
                            [("content-type", "text/event-stream")],
                            format!(
                                "data: {}\n\ndata: [DONE]\n\n",
                                json!({"choices":[{"delta":{"reasoning_content":"provider reasoning", "content":text}}]})
                            ),
                        )
                            .into_response()
                    } else {
                        Json(json!({"choices":[{"message":{"role":"assistant","reasoning_content":"provider reasoning","content":text}}]}))
                            .into_response()
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let events = Arc::new(Mutex::new(Vec::new()));
            let capture = events.clone();
            let client =
                super::LlmClient::new(format!("http://{address}"), "".into(), "fixture".into())
                    .with_generation_observer(GenerationObserver::new(
                        GenerationSource::Orientation,
                        None,
                        Arc::new(move |event| capture.lock().unwrap().push(event)),
                    ));
            let result = client
                .generate(vec![super::Message {
                    role: "user".into(),
                    content: "test".into(),
                }])
                .await
                .unwrap();
            server.abort();
            assert_eq!(result, "  灯 café\n");
            let events = events.lock().unwrap();
            let raw = events
                .iter()
                .filter_map(|event| match event {
                    GenerationEvent::Text {
                        text,
                        source: GenerationSource::Orientation,
                        channel,
                        ..
                    } if channel == "content" => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            assert_eq!(raw, result);
            assert!(events.iter().any(|event| matches!(event, GenerationEvent::Text { channel, text, .. } if channel == "reasoning_content" && text == "provider reasoning")));
            assert!(matches!(
                events.last(),
                Some(GenerationEvent::Finished { .. })
            ));
        }
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct JsonProbe {
        value: String,
        count: u32,
    }

    #[tokio::test]
    async fn incomplete_stream_retains_raw_output_but_finishes_as_failed() {
        use crate::generation_telemetry::{
            GenerationEvent, GenerationObserver, GenerationOutcome, GenerationSource,
        };
        use axum::{routing::post, Router};
        use std::sync::{Arc, Mutex};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    [("content-type", "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"partial 灯\"}}]}\n\n",
                )
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let capture = events.clone();
        let client =
            super::LlmClient::new(format!("http://{address}"), "".into(), "fixture".into())
                .with_generation_observer(GenerationObserver::new(
                    GenerationSource::Orientation,
                    None,
                    Arc::new(move |event| capture.lock().unwrap().push(event)),
                ));
        let result = client
            .generate(vec![super::Message {
                role: "user".into(),
                content: "test".into(),
            }])
            .await;
        server.abort();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("completion marker"));
        let events = events.lock().unwrap();
        assert!(events.iter().any(
            |event| matches!(event, GenerationEvent::Text { text, .. } if text == "partial 灯")
        ));
        assert!(matches!(
            events.last(),
            Some(GenerationEvent::Finished {
                outcome: GenerationOutcome::Failed,
                ..
            })
        ));
    }

    #[test]
    fn normalizes_openai_chat_completion_url() {
        assert_eq!(
            chat_completions_url("http://localhost:11434"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://localhost:11434/v1"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://localhost:11434/v1/"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://localhost:11434/v1/chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn extracts_message_content_from_multimodal_array() {
        let content = json!([
            { "type": "text", "text": "first" },
            { "type": "image_url", "image_url": { "url": "data:image/jpeg;base64,..." } },
            { "type": "text", "text": "second" }
        ]);
        let extracted = extract_message_content(Some(&content)).expect("extract text");
        assert_eq!(extracted, "first\nsecond");
    }

    #[test]
    fn parse_json_accepts_markdown_fenced_json() {
        let response = "```json\n{\"value\":\"ok\",\"count\":2}\n```";
        let parsed: JsonProbe = parse_json_response(response).expect("parse fenced JSON");
        assert_eq!(
            parsed,
            JsonProbe {
                value: "ok".to_string(),
                count: 2
            }
        );
    }

    #[test]
    fn parse_json_accepts_thinking_plus_fenced_json() {
        let response =
            "<think>internal reasoning</think>\n```json\n{\"value\":\"ok\",\"count\":5}\n```";
        let parsed: JsonProbe =
            parse_json_response(response).expect("parse thinking + fenced JSON");
        assert_eq!(
            parsed,
            JsonProbe {
                value: "ok".to_string(),
                count: 5
            }
        );
    }

    #[test]
    fn parse_json_accepts_double_encoded_json_string() {
        let response = "\"{\\\"value\\\":\\\"ok\\\",\\\"count\\\":3}\"";
        let parsed: JsonProbe = parse_json_response(response).expect("parse double-encoded JSON");
        assert_eq!(
            parsed,
            JsonProbe {
                value: "ok".to_string(),
                count: 3
            }
        );
    }
}
