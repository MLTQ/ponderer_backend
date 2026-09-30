use std::time::Duration;

/// Default deadline for ordinary backend HTTP requests, including LLM calls.
pub const DEFAULT_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Long prefill and generation are allowed only for the UI-owned GGUF provider.
/// The deadline stays bounded; UI closure still kills the full inference chain.
pub const LOCAL_GGUF_REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);

fn llm_request_timeout(api_url: &str, model: &str) -> Duration {
    let managed_local = model == crate::affect_lab::LOCAL_MODEL_ALIAS
        && reqwest::Url::parse(api_url).is_ok_and(|url| {
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        });
    if managed_local {
        LOCAL_GGUF_REQUEST_TIMEOUT
    } else {
        DEFAULT_HTTP_REQUEST_TIMEOUT
    }
}

pub fn build_llm_http_client(api_url: &str, model: &str) -> reqwest::Client {
    build_http_client_with_timeout(Some(llm_request_timeout(api_url, model)))
}

pub fn build_http_client() -> reqwest::Client {
    build_http_client_with_timeout(Some(DEFAULT_HTTP_REQUEST_TIMEOUT))
}

pub fn build_http_client_with_timeout(timeout: Option<Duration>) -> reqwest::Client {
    let allow_system_proxy = std::env::var("PONDERER_ENABLE_SYSTEM_PROXY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if allow_system_proxy {
        if let Ok(Ok(client)) = std::panic::catch_unwind(|| attempt_build(timeout, false)) {
            return client;
        }

        tracing::warn!(
            "HTTP client initialization with system proxy discovery failed; retrying with no_proxy"
        );
    }

    match std::panic::catch_unwind(|| attempt_build(timeout, true)) {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            panic!(
                "Failed to initialize HTTP client (no_proxy fallback returned error): {}",
                error
            );
        }
        Err(_) => {
            panic!("Failed to initialize HTTP client (no_proxy fallback panicked)");
        }
    }
}

fn attempt_build(
    timeout: Option<Duration>,
    no_proxy: bool,
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder();
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    if no_proxy {
        builder = builder.no_proxy();
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn explicit_request_timeout_is_applied() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let client = build_http_client_with_timeout(Some(Duration::from_millis(25)));
        let error = client
            .get(format!("http://{address}"))
            .send()
            .await
            .expect_err("request should time out");

        assert!(error.is_timeout(), "unexpected request error: {error}");
        server.abort();
    }

    #[test]
    fn ordinary_client_timeout_is_bounded_but_llm_friendly() {
        assert!(DEFAULT_HTTP_REQUEST_TIMEOUT >= Duration::from_secs(30));
        assert!(DEFAULT_HTTP_REQUEST_TIMEOUT <= Duration::from_secs(300));
    }

    #[test]
    fn long_deadline_is_scoped_to_the_managed_loopback_model() {
        let alias = crate::affect_lab::LOCAL_MODEL_ALIAS;
        assert_eq!(
            llm_request_timeout("http://127.0.0.1:12345/v1", alias),
            LOCAL_GGUF_REQUEST_TIMEOUT
        );
        assert_eq!(
            llm_request_timeout("https://provider.example/v1", alias),
            DEFAULT_HTTP_REQUEST_TIMEOUT
        );
        assert_eq!(
            llm_request_timeout("http://127.0.0.1:12345/v1", "other-model"),
            DEFAULT_HTTP_REQUEST_TIMEOUT
        );
        assert_eq!(
            llm_request_timeout("not a URL", alias),
            DEFAULT_HTTP_REQUEST_TIMEOUT
        );
    }
}
