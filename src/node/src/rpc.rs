use std::sync::Arc;
use axum::{
    Router,
    Json,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use commputer_core::transaction::Transaction;

/// Snapshot of chain status, populated by the event loop.
#[derive(Debug, Clone, Serialize)]
pub struct ChainStatus {
    pub height: u64,
    pub total_supply: u64,
    pub emitted: u64,
    pub burned: u64,
    pub circulating: u64,
    pub remaining: u64,
    pub accounts: usize,
    pub epoch: u64,
    pub pending_txs: usize,
}

/// Shared state for the RPC server.
pub struct RpcState {
    /// Channel to send submitted transactions to the event loop.
    pub tx_sender: mpsc::Sender<Transaction>,
    /// Latest chain status snapshot (updated by event loop).
    pub status: Mutex<ChainStatus>,
}

/// Response for a submitted transaction.
#[derive(Serialize)]
struct SubmitTxResponse {
    accepted: bool,
    tx_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// POST /tx — submit a signed transaction.
async fn submit_tx(
    State(state): State<Arc<RpcState>>,
    Json(tx): Json<Transaction>,
) -> (StatusCode, Json<SubmitTxResponse>) {
    // Basic validation before forwarding to event loop.
    if !tx.verify() {
        return (
            StatusCode::BAD_REQUEST,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash: String::new(),
                error: Some("Signature verification failed".into()),
            }),
        );
    }

    let tx_hash = hex::encode(tx.hash().0);

    match state.tx_sender.try_send(tx) {
        Ok(()) => (
            StatusCode::OK,
            Json(SubmitTxResponse {
                accepted: true,
                tx_hash,
                error: None,
            }),
        ),
        Err(mpsc::error::TrySendError::Full(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash,
                error: Some("Transaction queue full, try again later".into()),
            }),
        ),
        Err(mpsc::error::TrySendError::Closed(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash,
                error: Some("Node is shutting down".into()),
            }),
        ),
    }
}

/// GET /status — return current chain status.
async fn get_status(
    State(state): State<Arc<RpcState>>,
) -> Json<ChainStatus> {
    let status = state.status.lock().await.clone();
    Json(status)
}

/// Start the RPC server on the given port.
pub async fn start_rpc_server(
    rpc_port: u16,
    rpc_state: Arc<RpcState>,
) {
    let app = Router::new()
        .route("/tx", post(submit_tx))
        .route("/status", get(get_status))
        .with_state(rpc_state);

    let listener = match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", rpc_port)).await {
        Ok(l) => l,
        Err(e) => {
            warn!("Failed to bind RPC server on port {}: {}", rpc_port, e);
            return;
        }
    };

    info!("RPC server listening on http://127.0.0.1:{}", rpc_port);

    if let Err(e) = axum::serve(listener, app).await {
        warn!("RPC server error: {}", e);
    }
}
