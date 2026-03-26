//! Commputer Desktop App — web-based dashboard for the Commputer node.
//!
//! Block J (Items 176-200): Full desktop application that serves a web
//! frontend and proxies to the node RPC at localhost:9944.
//!
//! Architecture:
//! - Rust HTTP server (axum) serves the frontend from embedded assets
//! - Backend API endpoints handle wallet, config, and node communication
//! - Frontend is plain HTML/CSS/JS for maximum compatibility
//! - The app communicates with a RUNNING node via RPC — it does NOT run consensus

use std::sync::Arc;

mod auto_start;
mod commands;
mod notifier;
mod rpc_client;
mod server;
mod state;
mod tray;
mod update_checker;

#[tokio::main]
async fn main() {
    // Initialize tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Load config.
    let config = state::AppConfig::load();
    let dashboard_port = config.dashboard_port;

    tracing::info!("Commputer Desktop Dashboard starting...");
    tracing::info!("Node RPC: http://127.0.0.1:{}", config.rpc_port);
    tracing::info!("Dashboard: http://127.0.0.1:{}", dashboard_port);

    // Create shared app state.
    let app_state = Arc::new(server::AppState::new(config));

    // Initialize tray icon (Item 187).
    {
        let mut tray = app_state.tray.write().await;
        tray.show();
    }

    // Item 192: Check for updates on startup.
    let state_clone = Arc::clone(&app_state);
    tokio::spawn(async move {
        let checker = update_checker::UpdateChecker::new("commputer/commputer");
        let result = checker.check().await;
        if result.update_available {
            if let Some(ver) = &result.latest_version {
                tracing::info!("Update available: v{ver}");
                let mut notifier = state_clone.notifier.write().await;
                notifier.notify_update_available(ver);
            }
        }
    });

    // Item 188: Auto-start setup check.
    {
        let config = app_state.config.read().await;
        if config.auto_start {
            let exe = std::env::current_exe().unwrap_or_default();
            let auto = auto_start::AutoStart::new("commputer-desktop", exe);
            if !auto.is_enabled() {
                tracing::info!("Auto-start is configured but not installed; enabling...");
                if let Err(e) = auto.enable() {
                    tracing::warn!("Failed to enable auto-start: {e}");
                }
            }
        }
    }

    // Build the router.
    let router = server::build_router(app_state);

    // Start the server.
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", dashboard_port))
        .await
        .expect("Failed to bind dashboard port");

    tracing::info!("Dashboard ready at http://127.0.0.1:{}", dashboard_port);

    axum::serve(listener, router)
        .await
        .expect("Dashboard server failed");
}
