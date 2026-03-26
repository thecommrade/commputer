//! HTTP server that serves the frontend dashboard and provides backend API.
//! Items 176-200: Full desktop app backend as a lightweight web server.

use axum::{
    Router,
    routing::{get, post},
    response::{Html, IntoResponse},
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::commands::{
    self, WalletCreated, ComplianceDisplay, GracePeriodDisplay, TierProgress,
    MiningStatus, ProofScores, ErrorEntry, PeerVisualization, WalletExport,
};
use crate::notifier::{Notifier, NotificationType};
use crate::rpc_client::NodeClient;
use crate::state::AppConfig;
use crate::tray::{TrayIcon, TrayIconState};
use crate::update_checker::UpdateChecker;

/// Shared application state for the HTTP server.
pub struct AppState {
    pub config: RwLock<AppConfig>,
    pub node_client: NodeClient,
    pub notifier: RwLock<Notifier>,
    pub tray: RwLock<TrayIcon>,
    pub errors: RwLock<Vec<ErrorEntry>>,
    pub log_buffer: RwLock<Vec<LogLine>>,
}

/// A log line for the log viewer (Item 197).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub level: String,
    pub message: String,
    pub timestamp: u64,
}

impl AppState {
    /// Create a new app state from config.
    pub fn new(config: AppConfig) -> Self {
        let rpc_port = config.rpc_port;
        let notifications = config.notifications;
        Self {
            config: RwLock::new(config),
            node_client: NodeClient::new(rpc_port),
            notifier: RwLock::new(Notifier::new(notifications)),
            tray: RwLock::new(TrayIcon::new()),
            errors: RwLock::new(Vec::new()),
            log_buffer: RwLock::new(Vec::new()),
        }
    }

    /// Add an error to the error display (Item 193).
    pub async fn push_error(&self, error: &str) {
        let entry = commands::humanize_error(error);
        let mut errors = self.errors.write().await;
        errors.push(entry);
        // Keep only last 50 errors.
        if errors.len() > 50 {
            errors.remove(0);
        }
    }

    /// Add a log line to the buffer (Item 197).
    pub async fn push_log(&self, level: &str, message: &str) {
        let line = LogLine {
            level: level.to_string(),
            message: message.to_string(),
            timestamp: now_secs(),
        };
        let mut logs = self.log_buffer.write().await;
        logs.push(line);
        // Keep only last 500 lines.
        if logs.len() > 500 {
            logs.remove(0);
        }
    }
}

/// Build the Axum router for the desktop dashboard.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // Frontend routes
        .route("/", get(serve_index))
        .route("/app.js", get(serve_js))
        .route("/style.css", get(serve_css))
        // API routes
        .route("/api/status", get(api_status))
        .route("/api/wallet/create", post(api_create_wallet))
        .route("/api/wallet/recover", post(api_recover_wallet))
        .route("/api/wallet/export", post(api_export_wallet))
        .route("/api/wallet/info", get(api_wallet_info))
        .route("/api/mining", get(api_mining_status))
        .route("/api/peers", get(api_peers))
        .route("/api/compliance", get(api_compliance))
        .route("/api/tx/send", post(api_send_tx))
        .route("/api/tx/history", get(api_tx_history))
        .route("/api/config", get(api_get_config))
        .route("/api/config", post(api_set_config))
        .route("/api/config/contribution", post(api_set_contribution))
        .route("/api/config/theme", post(api_set_theme))
        .route("/api/config/window", post(api_set_window_state))
        .route("/api/errors", get(api_get_errors))
        .route("/api/logs", get(api_get_logs))
        .route("/api/network/visualization", get(api_network_viz))
        .route("/api/update/check", get(api_check_update))
        .route("/api/notifications", get(api_get_notifications))
        .with_state(state)
}

// --- Frontend serving ---

async fn serve_index() -> Html<&'static str> {
    Html(include_str!("../frontend/index.html"))
}

async fn serve_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/javascript")],
        include_str!("../frontend/app.js"),
    )
}

async fn serve_css() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "text/css")],
        include_str!("../frontend/style.css"),
    )
}

// --- API: Status ---

#[derive(Serialize)]
struct DashboardStatus {
    node_connected: bool,
    chain_height: u64,
    epoch: u64,
    pending_txs: usize,
    circulating: u64,
    peer_count: usize,
    node_status: String,
}

async fn api_status(State(state): State<Arc<AppState>>) -> Json<DashboardStatus> {
    let health = state.node_client.health().await;
    if !health {
        let mut tray = state.tray.write().await;
        tray.set_state(TrayIconState::Disconnected);
        return Json(DashboardStatus {
            node_connected: false,
            chain_height: 0,
            epoch: 0,
            pending_txs: 0,
            circulating: 0,
            peer_count: 0,
            node_status: "disconnected".to_string(),
        });
    }

    let status = state.node_client.status().await;
    let metrics = state.node_client.metrics().await;

    let mut tray = state.tray.write().await;
    tray.set_state(TrayIconState::Active);

    match status {
        Ok(s) => {
            let peer_count = metrics
                .ok()
                .and_then(|m| m.get("peers_connected").and_then(|v| v.as_u64()))
                .unwrap_or(0) as usize;
            Json(DashboardStatus {
                node_connected: true,
                chain_height: s.height,
                epoch: s.epoch,
                pending_txs: s.pending_txs,
                circulating: s.circulating,
                peer_count,
                node_status: "synced".to_string(),
            })
        }
        Err(e) => {
            state.push_error(&e).await;
            Json(DashboardStatus {
                node_connected: true,
                chain_height: 0,
                epoch: 0,
                pending_txs: 0,
                circulating: 0,
                peer_count: 0,
                node_status: "error".to_string(),
            })
        }
    }
}

// --- API: Wallet ---

async fn api_create_wallet(State(state): State<Arc<AppState>>) -> Json<WalletCreated> {
    let created = commands::create_wallet();
    let mut config = state.config.write().await;
    config.wallet_address = Some(created.address.clone());
    let _ = config.save();
    Json(created)
}

#[derive(Deserialize)]
struct RecoverRequest {
    seed_phrase: String,
}

async fn api_recover_wallet(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RecoverRequest>,
) -> Result<Json<WalletCreated>, (StatusCode, String)> {
    match commands::recover_wallet(&req.seed_phrase) {
        Ok(created) => {
            let mut config = state.config.write().await;
            config.wallet_address = Some(created.address.clone());
            let _ = config.save();
            Ok(Json(created))
        }
        Err(e) => Err((StatusCode::BAD_REQUEST, e)),
    }
}

#[derive(Deserialize)]
struct ExportRequest {
    seed_phrase: String,
    confirmed: bool,
}

async fn api_export_wallet(
    Json(req): Json<ExportRequest>,
) -> Result<Json<WalletExport>, (StatusCode, String)> {
    if !req.confirmed {
        return Err((StatusCode::BAD_REQUEST, "Export must be confirmed".to_string()));
    }
    match commands::export_wallet(&req.seed_phrase) {
        Ok(exported) => Ok(Json(exported)),
        Err(e) => Err((StatusCode::BAD_REQUEST, e)),
    }
}

#[derive(Serialize)]
struct WalletInfo {
    address: String,
    balance_formatted: String,
    balance_raw: u64,
    tier: String,
    tier_progress: TierProgress,
    grace_period: GracePeriodDisplay,
}

async fn api_wallet_info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = state.config.read().await;
    let address = match &config.wallet_address {
        Some(addr) => addr.clone(),
        None => return Json(serde_json::json!({"error": "No wallet configured"})),
    };

    match state.node_client.balance(&address).await {
        Ok(info) => {
            let tier_progress = commands::compute_tier_progress(info.balance);
            let compliance = state.node_client.compliance().await;
            let grace = match &compliance {
                Ok(c) => commands::compute_grace_display(c.grace_remaining_secs, c.grace_max_secs),
                Err(_) => commands::compute_grace_display(None, None),
            };

            Json(serde_json::json!({
                "address": info.address,
                "balance_formatted": commands::format_comme(info.balance),
                "balance_raw": info.balance,
                "tier": info.tier,
                "tier_progress": tier_progress,
                "grace_period": grace,
                "total_mined": info.total_mined,
            }))
        }
        Err(e) => {
            state.push_error(&e).await;
            Json(serde_json::json!({"error": e, "address": address}))
        }
    }
}

// --- API: Mining ---

async fn api_mining_status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let proof_status = state.node_client.proof_status().await;
    let status = state.node_client.status().await;

    let config = state.config.read().await;
    let total_mined = if let Some(addr) = &config.wallet_address {
        state.node_client.balance(addr).await
            .map(|b| b.total_mined)
            .unwrap_or(0)
    } else {
        0
    };

    let epoch = status.as_ref().map(|s| s.epoch).unwrap_or(0);

    let scores = match proof_status {
        Ok(ps) => ProofScores {
            cpu: ps.cpu_score,
            gpu: ps.gpu_score,
            storage: ps.storage_score,
            ram: ps.ram_score,
            bandwidth: ps.bandwidth_score,
        },
        Err(e) => {
            state.push_error(&e).await;
            ProofScores { cpu: 0, gpu: 0, storage: 0, ram: 0, bandwidth: 0 }
        }
    };

    let mining = commands::build_mining_status(epoch, total_mined, scores);
    Json(serde_json::to_value(mining).unwrap_or_default())
}

// --- API: Peers ---

async fn api_peers(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    match state.node_client.peers().await {
        Ok(peers) => {
            let viz = commands::build_peer_visualization(&peers);
            Json(serde_json::to_value(viz).unwrap_or_default())
        }
        Err(e) => {
            state.push_error(&e).await;
            Json(serde_json::json!({"error": e, "peers": [], "text_map": "", "peer_count": 0}))
        }
    }
}

// --- API: Compliance ---

async fn api_compliance(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    match state.node_client.compliance().await {
        Ok(info) => {
            let display = ComplianceDisplay {
                status: info.status,
                is_compliant: info.is_compliant,
                explanation: info.explanation,
            };
            Json(serde_json::to_value(display).unwrap_or_default())
        }
        Err(e) => {
            state.push_error(&e).await;
            Json(serde_json::json!({"error": e, "is_compliant": false, "status": "unknown"}))
        }
    }
}

// --- API: Transactions ---

#[derive(Deserialize)]
struct SendTxRequest {
    to: String,
    amount: f64,
}

async fn api_send_tx(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SendTxRequest>,
) -> Json<serde_json::Value> {
    let config = state.config.read().await;
    let from = match &config.wallet_address {
        Some(addr) => addr.clone(),
        None => return Json(serde_json::json!({"success": false, "error": "No wallet configured"})),
    };

    let amount_raw = (req.amount * commputer_core::token::UNITS_PER_COMME as f64) as u64;
    let tx = crate::rpc_client::TxSubmission {
        from,
        to: req.to,
        amount: amount_raw,
        nonce: 0, // Node will fill in the correct nonce.
    };

    match state.node_client.submit_tx(&tx).await {
        Ok(result) => Json(serde_json::to_value(result).unwrap_or_default()),
        Err(e) => {
            state.push_error(&e).await;
            Json(serde_json::json!({"success": false, "error": e}))
        }
    }
}

async fn api_tx_history(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mempool = state.node_client.mempool().await.unwrap_or_default();
    let entries: Vec<serde_json::Value> = mempool.iter().map(|entry| {
        serde_json::json!({
            "tx_hash": entry.tx_hash,
            "tx_type": entry.tx_type,
            "amount_formatted": commands::format_comme(entry.amount),
            "timestamp": entry.timestamp,
            "status": "pending",
        })
    }).collect();
    Json(serde_json::json!({"transactions": entries}))
}

// --- API: Config ---

async fn api_get_config(State(state): State<Arc<AppState>>) -> Json<AppConfig> {
    let config = state.config.read().await;
    Json(config.clone())
}

async fn api_set_config(
    State(state): State<Arc<AppState>>,
    Json(new_config): Json<AppConfig>,
) -> Result<Json<AppConfig>, (StatusCode, String)> {
    let mut config = state.config.write().await;
    *config = new_config;
    config.save().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(config.clone()))
}

#[derive(Deserialize)]
struct ContributionRequest {
    percent: u8,
}

async fn api_set_contribution(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ContributionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut config = state.config.write().await;
    config.contribution_percent = req.percent.clamp(1, 100);
    config.save().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({"contribution_percent": config.contribution_percent})))
}

#[derive(Deserialize)]
struct ThemeRequest {
    theme: String,
}

async fn api_set_theme(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ThemeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut config = state.config.write().await;
    match req.theme.as_str() {
        "dark" | "light" | "system" => {
            config.theme = req.theme.clone();
            config.save().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok(Json(serde_json::json!({"theme": config.theme})))
        }
        _ => Err((StatusCode::BAD_REQUEST, "Invalid theme".to_string())),
    }
}

async fn api_set_window_state(
    State(state): State<Arc<AppState>>,
    Json(ws): Json<crate::state::WindowState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut config = state.config.write().await;
    config.window = ws;
    config.save().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({"ok": true})))
}

// --- API: Errors & Logs ---

async fn api_get_errors(State(state): State<Arc<AppState>>) -> Json<Vec<ErrorEntry>> {
    let errors = state.errors.read().await;
    Json(errors.clone())
}

async fn api_get_logs(State(state): State<Arc<AppState>>) -> Json<Vec<LogLine>> {
    let logs = state.log_buffer.read().await;
    Json(logs.clone())
}

// --- API: Network visualization ---

async fn api_network_viz(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    match state.node_client.peers().await {
        Ok(peers) => {
            let viz = commands::build_peer_visualization(&peers);
            Json(serde_json::to_value(viz).unwrap_or_default())
        }
        Err(e) => {
            state.push_error(&e).await;
            Json(serde_json::json!({"text_map": "[Error fetching peers]", "peer_count": 0, "peers": []}))
        }
    }
}

// --- API: Update check ---

async fn api_check_update() -> Json<serde_json::Value> {
    let checker = UpdateChecker::new("commputer/commputer");
    let result = checker.check().await;
    Json(serde_json::to_value(result).unwrap_or_default())
}

// --- API: Notifications ---

async fn api_get_notifications(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let notifier = state.notifier.read().await;
    let history = notifier.history();
    Json(serde_json::to_value(history).unwrap_or_default())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_state_creation() {
        let config = AppConfig::default();
        let state = AppState::new(config);
        assert_eq!(state.node_client.base_url, "http://127.0.0.1:9944");
    }

    #[test]
    fn router_builds_without_panic() {
        let config = AppConfig::default();
        let state = Arc::new(AppState::new(config));
        let _router = build_router(state);
    }

    #[tokio::test]
    async fn push_error_stores() {
        let state = AppState::new(AppConfig::default());
        state.push_error("RPC request failed: connection refused").await;
        let errors = state.errors.read().await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("unreachable"));
    }

    #[tokio::test]
    async fn push_log_stores() {
        let state = AppState::new(AppConfig::default());
        state.push_log("info", "Test log message").await;
        let logs = state.log_buffer.read().await;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "Test log message");
    }

    #[tokio::test]
    async fn error_buffer_limit() {
        let state = AppState::new(AppConfig::default());
        for i in 0..60 {
            state.push_error(&format!("Error {i}")).await;
        }
        let errors = state.errors.read().await;
        assert!(errors.len() <= 50);
    }

    #[tokio::test]
    async fn log_buffer_limit() {
        let state = AppState::new(AppConfig::default());
        for i in 0..510 {
            state.push_log("info", &format!("Log {i}")).await;
        }
        let logs = state.log_buffer.read().await;
        assert!(logs.len() <= 500);
    }
}
