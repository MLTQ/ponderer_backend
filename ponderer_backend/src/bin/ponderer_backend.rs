use anyhow::{Context, Result};
use flume::unbounded;
use ponderer_backend::config::AgentConfig;
use ponderer_backend::runtime::BackendRuntime;
use ponderer_backend::server::serve_backend;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,ponderer_backend=debug")),
        )
        .init();

    let config = AgentConfig::load();
    let (event_tx, event_rx) = unbounded();
    let runtime = BackendRuntime::bootstrap(config, event_tx)
        .context("failed to bootstrap backend runtime")?;

    tracing::info!(
        "Starting standalone backend service (set PONDERER_BACKEND_TOKEN + optional PONDERER_BACKEND_BIND; auth mode via PONDERER_BACKEND_AUTH_MODE)"
    );

    let server_rt = tokio::runtime::Runtime::new().context("failed to start server runtime")?;
    server_rt.block_on(serve_backend(runtime, event_rx))
}
