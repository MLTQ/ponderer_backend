use std::time::Duration;

pub fn build_http_client() -> reqwest::Client {
    build_http_client_with_timeout(None)
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
