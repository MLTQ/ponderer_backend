mod api;
mod ui;

use tracing_subscriber::EnvFilter;

pub use ponderer_backend::character_card;
pub use ponderer_backend::comfy_client;
pub use ponderer_backend::comfy_workflow;
pub use ponderer_backend::config;

use api::ApiClient;
use config::AgentConfig;
use ui::app::AgentApp;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,ponderer=debug")),
        )
        .init();

    tracing::info!("Ponderer frontend starting...");

    let fallback_config = AgentConfig::load();
    let api_client = ApiClient::from_env();

    tracing::info!("Backend API: {}", api_client.base_url());
    if std::env::var("PONDERER_BACKEND_TOKEN")
        .ok()
        .map(|token| token.trim().is_empty())
        .unwrap_or(true)
    {
        tracing::warn!(
            "PONDERER_BACKEND_TOKEN is unset/empty; requests will fail unless backend auth mode is disabled"
        );
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 800.0])
            .with_title("Ponderer"),
        ..Default::default()
    };

    if let Err(e) = eframe::run_native(
        "Ponderer",
        native_options,
        Box::new(|_cc| Ok(Box::new(AgentApp::new(api_client, fallback_config)))),
    ) {
        tracing::error!("UI error: {}", e);
        std::process::exit(1);
    }
}
