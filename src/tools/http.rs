//! HTTP/web fetch tool with safety guardrails.
//!
//! Supports GET/POST/PUT/DELETE, blocks localhost/private destinations by default,
//! runs outbound leak checks, enforces a timeout, and truncates response bodies.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{Method, Url};
use serde_json::{json, Value};
use std::cmp::min;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use crate::http_client::build_http_client_with_timeout;

use super::safety::{detect_leaks, SafetyVerdict};
use super::{Tool, ToolCategory, ToolContext, ToolOutput};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_MAX_RESPONSE_BYTES: usize = 512 * 1024;

pub struct HttpFetchTool;

impl HttpFetchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for HttpFetchTool {
    fn name(&self) -> &str {
        "http_fetch"
    }

    fn description(&self) -> &str {
        "Make a safe HTTP request (GET/POST/PUT/DELETE) with timeout and truncated response output."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Target URL to fetch"
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "DELETE"],
                    "description": "HTTP method (default GET)"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key/value strings",
                    "additionalProperties": { "type": "string" }
                },
                "body_text": {
                    "type": "string",
                    "description": "Optional raw text request body"
                },
                "body_json": {
                    "description": "Optional JSON request body"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Request timeout in seconds (default 30, max 30)"
                },
                "max_response_bytes": {
                    "type": "integer",
                    "description": "Maximum response bytes captured (default 65536, max 524288)"
                },
                "allow_private_hosts": {
                    "type": "boolean",
                    "description": "Allow localhost/private IP destinations (default false)"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let url_input = match params.get("url").and_then(Value::as_str).map(str::trim) {
            Some(v) if !v.is_empty() => v,
            _ => {
                return Ok(ToolOutput::Error(
                    "Missing required 'url' parameter".to_string(),
                ))
            }
        };

        let method_raw = params
            .get("method")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("GET");
        let method = match method_raw.to_ascii_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "DELETE" => Method::DELETE,
            _ => {
                return Ok(ToolOutput::Error(format!(
                    "Unsupported method '{}'. Use GET/POST/PUT/DELETE.",
                    method_raw
                )))
            }
        };

        let url = match Url::parse(url_input) {
            Ok(url) => url,
            Err(e) => {
                return Ok(ToolOutput::Error(format!(
                    "Invalid URL '{}': {}",
                    url_input, e
                )))
            }
        };
        if !matches!(url.scheme(), "http" | "https") {
            return Ok(ToolOutput::Error(
                "Only http:// and https:// URLs are supported".to_string(),
            ));
        }

        let timeout_secs = params
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);
        let max_response_bytes = params
            .get("max_response_bytes")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES)
            .clamp(1, MAX_MAX_RESPONSE_BYTES);
        let allow_private_hosts = params
            .get("allow_private_hosts")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let headers = match parse_headers(params.get("headers")) {
            Ok(headers) => headers,
            Err(e) => return Ok(ToolOutput::Error(e)),
        };

        let body_text = params
            .get("body_text")
            .and_then(Value::as_str)
            .map(str::to_string);
        let body_json = params.get("body_json").cloned();
        if body_text.is_some() && body_json.is_some() {
            return Ok(ToolOutput::Error(
                "Provide only one of 'body_text' or 'body_json'".to_string(),
            ));
        }

        let mut warnings = Vec::new();
        if url.scheme() == "http" {
            warnings.push(
                "Using plain HTTP (not HTTPS). Traffic can be observed or modified in transit."
                    .to_string(),
            );
        }

        match validate_destination_safety(&url, allow_private_hosts).await {
            Ok(extra_warnings) => warnings.extend(extra_warnings),
            Err(reason) => return Ok(ToolOutput::Error(reason)),
        }

        if let Err(reason) =
            check_outbound_for_leaks(&url, &headers, body_text.as_deref(), body_json.as_ref())
        {
            return Ok(ToolOutput::Error(reason));
        }

        let client = build_http_client_with_timeout(Some(Duration::from_secs(timeout_secs)));

        let mut req = client.request(method.clone(), url.clone());
        for (key, value) in &headers {
            req = req.header(key, value);
        }
        if let Some(body) = body_text.as_ref() {
            req = req.body(body.clone());
        } else if let Some(body) = body_json.as_ref() {
            req = req.json(body);
        }

        let mut response = match req.send().await {
            Ok(resp) => resp,
            Err(e) => return Ok(ToolOutput::Error(format!("HTTP request failed: {}", e))),
        };

        let status = response.status();
        let response_headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or("<non-utf8>").to_string(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let content_type = response_headers
            .get("content-type")
            .cloned()
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let mut body_bytes: Vec<u8> = Vec::new();
        let mut total_read: usize = 0;
        let mut truncated = false;
        while let Some(chunk) = response
            .chunk()
            .await
            .context("Failed reading response body")?
        {
            total_read += chunk.len();
            if body_bytes.len() < max_response_bytes {
                let remaining = max_response_bytes - body_bytes.len();
                let take = min(remaining, chunk.len());
                body_bytes.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    truncated = true;
                    break;
                }
            } else {
                truncated = true;
                break;
            }
        }
        if total_read > body_bytes.len() {
            truncated = true;
        }

        let body_preview = String::from_utf8_lossy(&body_bytes).to_string();

        Ok(ToolOutput::Json(json!({
            "status": "ok",
            "request": {
                "url": url.as_str(),
                "method": method.as_str(),
                "timeout_secs": timeout_secs,
            },
            "response": {
                "status_code": status.as_u16(),
                "status_text": status.canonical_reason().unwrap_or(""),
                "headers": response_headers,
                "content_type": content_type,
                "body_preview": body_preview,
                "body_preview_bytes": body_bytes.len(),
                "body_total_bytes_read": total_read,
                "body_truncated": truncated,
            },
            "warnings": warnings,
        })))
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Network
    }
}

fn parse_headers(value: Option<&Value>) -> std::result::Result<BTreeMap<String, String>, String> {
    let mut headers = BTreeMap::new();
    let Some(headers_value) = value else {
        return Ok(headers);
    };
    let Some(obj) = headers_value.as_object() else {
        return Err("'headers' must be an object of string values".to_string());
    };
    for (key, value) in obj {
        let Some(val_str) = value.as_str() else {
            return Err(format!("Header '{}' must be a string", key));
        };
        headers.insert(key.clone(), val_str.to_string());
    }
    Ok(headers)
}

async fn validate_destination_safety(
    url: &Url,
    allow_private_hosts: bool,
) -> std::result::Result<Vec<String>, String> {
    if allow_private_hosts {
        return Ok(Vec::new());
    }

    let host = url
        .host_str()
        .ok_or_else(|| "URL is missing a host".to_string())?;
    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".local")
    {
        return Err(format!("Blocked local/private host '{}'", host));
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_or_local_ip(ip) {
            return Err(format!("Blocked private/local IP destination '{}'", host));
        }
        return Ok(Vec::new());
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let mut warnings = Vec::new();
    match tokio::net::lookup_host((host, port)).await {
        Ok(resolved) => {
            for addr in resolved {
                if is_private_or_local_ip(addr.ip()) {
                    return Err(format!(
                        "Blocked destination '{}' resolved to private/local address {}",
                        host,
                        addr.ip()
                    ));
                }
            }
        }
        Err(e) => {
            warnings.push(format!(
                "Could not pre-resolve host '{}' for private-IP validation ({}).",
                host, e
            ));
        }
    }

    Ok(warnings)
}

fn is_private_or_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}

fn check_outbound_for_leaks(
    url: &Url,
    headers: &BTreeMap<String, String>,
    body_text: Option<&str>,
    body_json: Option<&Value>,
) -> std::result::Result<(), String> {
    let mut payload = String::new();
    payload.push_str(url.as_str());
    payload.push('\n');
    for (k, v) in headers {
        payload.push_str(k);
        payload.push_str(": ");
        payload.push_str(v);
        payload.push('\n');
    }
    if let Some(text) = body_text {
        payload.push_str(text);
        payload.push('\n');
    }
    if let Some(json_body) = body_json {
        payload.push_str(&json_body.to_string());
        payload.push('\n');
    }

    match detect_leaks(&payload) {
        SafetyVerdict::Allow => Ok(()),
        SafetyVerdict::Warn(reason) => {
            tracing::warn!("Outbound HTTP payload leak warning: {}", reason);
            Ok(())
        }
        SafetyVerdict::Block(reason) => Err(format!("Outbound payload blocked: {}", reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ip_detection_blocks_local_ranges() {
        assert!(is_private_or_local_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_or_local_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_or_local_ip("192.168.1.4".parse().unwrap()));
        assert!(is_private_or_local_ip("::1".parse().unwrap()));
    }

    #[test]
    fn private_ip_detection_allows_public_ranges() {
        assert!(!is_private_or_local_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_private_or_local_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_or_local_ip(
            "2606:4700:4700::1111".parse().unwrap()
        ));
    }

    #[test]
    fn parse_headers_requires_string_values() {
        let bad = json!({"x-test": 5});
        assert!(parse_headers(Some(&bad)).is_err());
    }

    #[test]
    fn outbound_leak_check_blocks_secret_like_payload() {
        let url = Url::parse("https://example.com").unwrap();
        let headers = BTreeMap::new();
        let blocked = check_outbound_for_leaks(
            &url,
            &headers,
            Some("token=sk-abcdefghijklmnopqrstuvwxyz1234567890"),
            None,
        );
        assert!(blocked.is_err());
    }
}
