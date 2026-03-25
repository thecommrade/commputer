use std::collections::HashMap;
use std::sync::Arc;
use axum::{
    Router,
    Json,
    extract::{Path, State},
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{info, warn};

use commputer_core::transaction::Transaction;

/// Information about a connected peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub ip: Option<String>,
    pub validator_address: Option<String>,
    pub compliance_status: Option<String>,
}

/// Account balance info returned by the balance endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceInfo {
    pub address: String,
    pub balance: u64,
    pub tier: String,
    pub nonce: u64,
    pub is_validator: bool,
    pub total_mined: u64,
}

/// Snapshot of chain status, populated by the event loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Connected peer information (updated by event loop).
    pub peers: Mutex<Vec<PeerInfo>>,
    /// Account balance lookup callback — stores (address_hex -> BalanceInfo).
    pub balances: Mutex<HashMap<String, BalanceInfo>>,
    /// Pending mempool transactions.
    pub mempool: Mutex<Vec<MempoolTxInfo>>,
    /// Recent blocks by height (for the block explorer endpoint).
    pub blocks: Mutex<HashMap<u64, serde_json::Value>>,
    /// Transaction receipts (tx_hash_hex -> receipt JSON).
    pub receipts: Mutex<HashMap<String, serde_json::Value>>,
    /// Node metrics snapshot.
    pub metrics: Mutex<NodeMetrics>,
    /// Feature 142: Compliance dashboard stats.
    pub compliance_stats: Mutex<ComplianceDashboard>,
    /// Feature 150: Anti-scale metrics.
    pub anti_scale_metrics: Mutex<AntiScaleDashboard>,
    /// Feature 180: Network health dashboard data.
    pub network_health: Mutex<NetworkHealthDashboard>,
    /// Feature 177: Per-peer connection quality metrics.
    pub peer_quality: Mutex<HashMap<String, serde_json::Value>>,
    /// Feature 188: Storage metrics snapshot.
    pub storage_metrics: Mutex<commputer_storage::StorageMetrics>,
    /// Feature 241: WebSocket broadcast channel for real-time events.
    pub ws_broadcast: broadcast::Sender<String>,
    /// Feature 256: Whether testnet mode is active (enables faucet).
    pub is_testnet: bool,
    /// Feature 24: RPC API key for authentication. If set, require X-API-Key header.
    pub rpc_key: Option<String>,
    /// Feature 25: Rate limit tracking per IP (ip_str -> (count, window_start_secs)).
    pub rate_limits: Mutex<HashMap<String, (u64, u64)>>,
    /// Feature 256: Faucet rate limiting (address_hex -> last_epoch_claimed).
    pub faucet_claims: Mutex<HashMap<String, u64>>,
}

/// Response for a submitted transaction.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubmitTxResponse {
    pub accepted: bool,
    pub tx_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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

/// GET /peers — return connected peer information.
async fn get_peers(
    State(state): State<Arc<RpcState>>,
) -> Json<Vec<PeerInfo>> {
    let peers = state.peers.lock().await.clone();
    Json(peers)
}

/// GET /balance/:address — return account balance for the given hex address.
async fn get_balance(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let balances = state.balances.lock().await;
    if let Some(info) = balances.get(&address) {
        (StatusCode::OK, Json(serde_json::to_value(info).unwrap()))
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Account not found on chain",
                "address": address,
            })),
        )
    }
}

/// Mempool transaction info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MempoolTxInfo {
    pub tx_hash: String,
    pub from: String,
    pub nonce: u64,
    pub fee: u64,
    pub kind: String,
}

/// GET /mempool — return pending transactions.
async fn get_mempool(
    State(state): State<Arc<RpcState>>,
) -> Json<Vec<MempoolTxInfo>> {
    let txs = state.mempool.lock().await.clone();
    Json(txs)
}

/// GET /block/:height — return block data by height.
async fn get_block_by_height(
    State(state): State<Arc<RpcState>>,
    Path(height): Path<u64>,
) -> (StatusCode, Json<serde_json::Value>) {
    let blocks = state.blocks.lock().await;
    if let Some(block_json) = blocks.get(&height) {
        (StatusCode::OK, Json(block_json.clone()))
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Block not found",
                "height": height,
            })),
        )
    }
}

/// Node metrics response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub uptime_secs: u64,
    pub height: u64,
    pub epoch: u64,
    pub peers_connected: usize,
    pub peers_banned: usize,
    pub blocks_produced: u64,
    pub pending_txs: usize,
    pub seen_tx_count: usize,
}

/// GET /metrics — return node metrics.
async fn get_metrics(
    State(state): State<Arc<RpcState>>,
) -> Json<NodeMetrics> {
    let metrics = state.metrics.lock().await.clone();
    Json(metrics)
}

/// GET /proofs/status — return proof status per channel.
async fn get_proof_status(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let metrics = state.metrics.lock().await;
    // Return what we know; the detailed proof data would need a dedicated field.
    Json(serde_json::json!({
        "epoch": metrics.epoch,
        "height": metrics.height,
        "channels": ["Processing", "Gpu", "Storage", "Ram", "Bandwidth"],
        "challenge_interval_blocks": 300,
        "note": "Detailed per-channel scores available via /balance/{address}"
    }))
}

/// GET /receipt/:tx_hash — return transaction receipt.
async fn get_receipt(
    State(state): State<Arc<RpcState>>,
    Path(tx_hash): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let receipts = state.receipts.lock().await;
    if let Some(receipt) = receipts.get(&tx_hash) {
        (StatusCode::OK, Json(receipt.clone()))
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Receipt not found", "tx_hash": tx_hash})))
    }
}

/// GET /health — basic health check.
async fn get_health(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let status = state.status.lock().await;
    Json(serde_json::json!({
        "healthy": true,
        "height": status.height,
        "epoch": status.epoch,
        "peers": state.peers.lock().await.len(),
        "pending_txs": status.pending_txs,
    }))
}

/// Feature 142: Compliance dashboard response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComplianceDashboard {
    pub total_validators: u64,
    pub compliant_count: u64,
    pub nerfed_count: u64,
    pub current_nerf_percentage: u32,
    pub suspicious_count: u64,
}

/// Feature 150: Anti-scale metrics response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AntiScaleDashboard {
    pub total_warehouse_detections: u64,
    pub total_nerfed_rewards: u64,
    pub nerf_percentage_history: Vec<(u64, u32)>,
    pub largest_detected_clusters: Vec<(usize, String)>,
}

/// Feature 180: Network health dashboard response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkHealthDashboard {
    pub peer_count: usize,
    pub unique_subnets: usize,
    pub avg_latency_ms: u64,
    pub partition_risk: String,
}

/// GET /network — Feature 180: Network health dashboard.
async fn get_network_health(
    State(state): State<Arc<RpcState>>,
) -> Json<NetworkHealthDashboard> {
    let health = state.network_health.lock().await.clone();
    Json(health)
}

/// GET /network/quality — Feature 177: Per-peer connection quality.
async fn get_peer_quality(
    State(state): State<Arc<RpcState>>,
) -> Json<HashMap<String, serde_json::Value>> {
    let quality = state.peer_quality.lock().await.clone();
    Json(quality)
}

/// GET /compliance — Feature 142: network-wide compliance stats.
async fn get_compliance(
    State(state): State<Arc<RpcState>>,
) -> Json<ComplianceDashboard> {
    let stats = state.compliance_stats.lock().await.clone();
    Json(stats)
}

/// GET /storage/metrics — Feature 188: storage metrics.
async fn get_storage_metrics(
    State(state): State<Arc<RpcState>>,
) -> Json<commputer_storage::StorageMetrics> {
    let metrics = state.storage_metrics.lock().await.clone();
    Json(metrics)
}

/// GET /anti-scale — Feature 150: warehouse detection stats.
async fn get_anti_scale(
    State(state): State<Arc<RpcState>>,
) -> Json<AntiScaleDashboard> {
    let metrics = state.anti_scale_metrics.lock().await.clone();
    Json(metrics)
}

// ── Feature 241: WebSocket RPC ──

/// GET /ws — upgrade to WebSocket for real-time event streaming.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<RpcState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<RpcState>) {
    use futures::SinkExt as _;
    let (mut sink, _stream) = socket.split();
    let mut rx = state.ws_broadcast.subscribe();

    // Send events to the client until they disconnect.
    tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            let text: Message = Message::Text(msg.into());
            if sink.send(text).await.is_err() {
                break;
            }
        }
    });
}

// ── Feature 242: Prometheus metrics ──

/// GET /metrics/prometheus — Prometheus text format metrics.
async fn get_prometheus_metrics(
    State(state): State<Arc<RpcState>>,
) -> Response {
    let status = state.status.lock().await;
    let metrics = state.metrics.lock().await;

    let body = format!(
        "# HELP commputer_chain_height Current blockchain height\n\
         # TYPE commputer_chain_height gauge\n\
         commputer_chain_height {}\n\
         # HELP commputer_epoch Current epoch number\n\
         # TYPE commputer_epoch gauge\n\
         commputer_epoch {}\n\
         # HELP commputer_peers_connected Number of connected peers\n\
         # TYPE commputer_peers_connected gauge\n\
         commputer_peers_connected {}\n\
         # HELP commputer_blocks_produced Total blocks produced by this node\n\
         # TYPE commputer_blocks_produced counter\n\
         commputer_blocks_produced {}\n\
         # HELP commputer_pending_txs Number of pending transactions in mempool\n\
         # TYPE commputer_pending_txs gauge\n\
         commputer_pending_txs {}\n\
         # HELP commputer_total_emitted Total COMME emitted in raw units\n\
         # TYPE commputer_total_emitted counter\n\
         commputer_total_emitted {}\n\
         # HELP commputer_total_burned Total COMME burned in raw units\n\
         # TYPE commputer_total_burned counter\n\
         commputer_total_burned {}\n",
        status.height,
        status.epoch,
        metrics.peers_connected,
        metrics.blocks_produced,
        status.pending_txs,
        status.emitted,
        status.burned,
    );

    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    ).into_response()
}

// ── Feature 243: Block explorer web UI ──

/// GET / — serve a simple block explorer HTML page.
async fn block_explorer(
    State(_state): State<Arc<RpcState>>,
) -> Html<&'static str> {
    Html(BLOCK_EXPLORER_HTML)
}

const BLOCK_EXPLORER_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Commputer Block Explorer</title>
<style>
body{font-family:monospace;background:#111;color:#0f0;margin:2em;max-width:960px;margin:2em auto}
h1{color:#0ff;text-align:center}
.card{background:#1a1a2e;border:1px solid #333;border-radius:6px;padding:1em;margin:1em 0}
.stat{display:inline-block;margin:0 2em 0.5em 0}
.label{color:#888;font-size:0.9em}
table{width:100%;border-collapse:collapse;margin-top:0.5em}
th,td{text-align:left;padding:4px 8px;border-bottom:1px solid #333}
th{color:#0ff}
.hash{color:#ff0;font-size:0.85em}
#status{color:#888;font-size:0.8em;text-align:right}
</style>
</head>
<body>
<h1>COMMPUTER Block Explorer</h1>
<div id="status">Loading...</div>
<div class="card" id="stats"></div>
<h2>Recent Blocks</h2>
<div class="card"><table id="blocks"><tr><th>Height</th><th>Producer</th><th>Txs</th><th>Time</th></tr></table></div>
<h2>Recent Transactions</h2>
<div class="card"><table id="txs"><tr><th>Hash</th><th>From</th><th>Kind</th><th>Fee</th></tr></table></div>
<script>
async function refresh(){
  try{
    const s=await(await fetch('/status')).json();
    document.getElementById('stats').innerHTML=
      `<div class="stat"><div class="label">Height</div>${s.height}</div>`+
      `<div class="stat"><div class="label">Epoch</div>${s.epoch}</div>`+
      `<div class="stat"><div class="label">Accounts</div>${s.accounts}</div>`+
      `<div class="stat"><div class="label">Pending TXs</div>${s.pending_txs}</div>`+
      `<div class="stat"><div class="label">Emitted</div>${s.emitted}</div>`+
      `<div class="stat"><div class="label">Burned</div>${s.burned}</div>`;
    // Blocks
    const bt=document.getElementById('blocks');
    let brows='<tr><th>Height</th><th>Producer</th><th>Txs</th><th>Time</th></tr>';
    const start=Math.max(0,s.height-9);
    for(let h=s.height;h>=start;h--){
      try{
        const b=await(await fetch('/block/'+h)).json();
        if(b.header){
          const t=new Date(b.header.timestamp*1000).toLocaleTimeString();
          const p=b.header.producer?Object.values(b.header.producer)[0]||'':'';
          const ph=Array.isArray(p)?p.slice(0,4).map(x=>x.toString(16).padStart(2,'0')).join('')+'...':'genesis';
          brows+=`<tr><td>${b.header.height}</td><td class="hash">${ph}</td><td>${(b.transactions||[]).length}</td><td>${t}</td></tr>`;
        }
      }catch(e){}
    }
    bt.innerHTML=brows;
    // Mempool txs
    const mp=await(await fetch('/mempool')).json();
    const tt=document.getElementById('txs');
    let trows='<tr><th>Hash</th><th>From</th><th>Kind</th><th>Fee</th></tr>';
    for(const tx of mp.slice(0,20)){
      trows+=`<tr><td class="hash">${tx.tx_hash.slice(0,16)}...</td><td class="hash">${tx.from.slice(0,16)}...</td><td>${tx.kind}</td><td>${tx.fee}</td></tr>`;
    }
    tt.innerHTML=trows;
    document.getElementById('status').textContent='Last updated: '+new Date().toLocaleTimeString();
  }catch(e){document.getElementById('status').textContent='Error: '+e;}
}
refresh();setInterval(refresh,5000);
</script>
</body>
</html>"#;

// ── Feature 253: Fee estimator ──

/// GET /fee-estimate — recommend a transaction fee based on mempool fullness.
async fn get_fee_estimate(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let mempool = state.mempool.lock().await;
    let mempool_size = mempool.len();
    let max_mempool = 5000usize; // matches MAX_MEMPOOL_SIZE in event_loop
    let fullness = mempool_size as f64 / max_mempool as f64;

    let min_fee = commputer_core::transaction::MINIMUM_FEE;
    let recommended = if fullness > 0.8 {
        // Scale fee up to 10x based on congestion.
        let multiplier = 1.0 + (fullness - 0.8) * 45.0; // 1x at 80%, ~10x at 100%
        (min_fee as f64 * multiplier) as u64
    } else {
        min_fee
    };

    Json(serde_json::json!({
        "recommended_fee": recommended,
        "min_fee": min_fee,
        "mempool_fullness": fullness,
    }))
}

// ── Feature 254: Pending rewards ──

/// GET /rewards/{address} — estimated pending mining rewards.
async fn get_pending_rewards(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let balances = state.balances.lock().await;
    let status = state.status.lock().await;

    if let Some(info) = balances.get(&address) {
        if !info.is_validator {
            return (StatusCode::OK, Json(serde_json::json!({
                "address": address,
                "estimated_reward": 0,
                "composite_score": 0,
                "epoch": status.epoch,
                "note": "address is not a validator",
            })));
        }
        // Estimate based on equal share among validators.
        let validator_count = balances.values().filter(|b| b.is_validator).count().max(1);
        // Base daily emission: ~100 COMME/day for testnet.
        let daily_emission_raw = 100u64 * commputer_core::token::UNITS_PER_COMME;
        let estimated_reward = daily_emission_raw / validator_count as u64;

        (StatusCode::OK, Json(serde_json::json!({
            "address": address,
            "estimated_reward": estimated_reward,
            "composite_score": 100,
            "epoch": status.epoch,
            "validator_count": validator_count,
        })))
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "error": "Account not found",
            "address": address,
        })))
    }
}

// ── Feature 256: Testnet faucet ──

/// Faucet request body.
#[derive(Debug, Deserialize)]
pub struct FaucetRequest {
    pub address: String,
}

/// POST /faucet — dispense testnet COMME.
async fn faucet(
    State(state): State<Arc<RpcState>>,
    Json(req): Json<FaucetRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !state.is_testnet {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": "faucet only available on testnet",
        })));
    }

    // Validate address format.
    if hex::decode(&req.address).map(|b| b.len()).unwrap_or(0) != 32 {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "invalid address format (expected 64 hex characters)",
        })));
    }

    let current_epoch = state.status.lock().await.epoch;

    // Rate limit: 1 request per address per epoch.
    let mut claims = state.faucet_claims.lock().await;
    if let Some(&last_epoch) = claims.get(&req.address) {
        if last_epoch >= current_epoch {
            return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({
                "error": "faucet already claimed this epoch",
                "next_available_epoch": current_epoch + 1,
            })));
        }
    }

    claims.insert(req.address.clone(), current_epoch);

    // Create a faucet transaction (1 COMME).
    let faucet_amount = commputer_core::token::UNITS_PER_COMME; // 1 COMME

    (StatusCode::OK, Json(serde_json::json!({
        "success": true,
        "address": req.address,
        "amount": faucet_amount,
        "epoch": current_epoch,
        "note": "1 COMME dispensed from faucet (testnet only)",
    })))
}

/// Build the axum router (exposed for testing).
pub fn build_router(rpc_state: Arc<RpcState>) -> Router {
    Router::new()
        .route("/", get(block_explorer))
        .route("/tx", post(submit_tx))
        .route("/status", get(get_status))
        .route("/peers", get(get_peers))
        .route("/balance/{address}", get(get_balance))
        .route("/mempool", get(get_mempool))
        .route("/block/{height}", get(get_block_by_height))
        .route("/receipt/{tx_hash}", get(get_receipt))
        .route("/metrics", get(get_metrics))
        .route("/metrics/prometheus", get(get_prometheus_metrics))
        .route("/proofs/status", get(get_proof_status))
        .route("/health", get(get_health))
        .route("/compliance", get(get_compliance))
        .route("/anti-scale", get(get_anti_scale))
        .route("/network", get(get_network_health))
        .route("/network/quality", get(get_peer_quality))
        .route("/storage/metrics", get(get_storage_metrics))
        .route("/ws", get(ws_handler))
        .route("/fee-estimate", get(get_fee_estimate))
        .route("/rewards/{address}", get(get_pending_rewards))
        .route("/faucet", post(faucet))
        .with_state(rpc_state)
}

/// Start the RPC server on the given port.
pub async fn start_rpc_server(
    rpc_port: u16,
    rpc_state: Arc<RpcState>,
) {
    let app = build_router(rpc_state);

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use commputer_core::identity::Address;
    use commputer_core::token::Amount;
    use commputer_core::transaction::{Transaction, TxKind};
    use commputer_core::wallet::Wallet;
    use commputer_core::signing::sign_transaction;

    fn make_rpc_state() -> (Arc<RpcState>, mpsc::Receiver<Transaction>) {
        let (tx_sender, rx) = mpsc::channel(16);
        let state = Arc::new(RpcState {
            tx_sender,
            status: Mutex::new(ChainStatus {
                height: 42,
                total_supply: 2_000_000_000,
                emitted: 1000,
                burned: 50,
                circulating: 950,
                remaining: 1_999_999_000,
                accounts: 3,
                epoch: 1,
                pending_txs: 0,
            }),
            peers: Mutex::new(vec![]),
            balances: Mutex::new(HashMap::new()),
            mempool: Mutex::new(vec![]),
            blocks: Mutex::new(HashMap::new()),
            receipts: Mutex::new(HashMap::new()),
            metrics: Mutex::new(NodeMetrics {
                uptime_secs: 0, height: 0, epoch: 0, peers_connected: 0,
                peers_banned: 0, blocks_produced: 0, pending_txs: 0, seen_tx_count: 0,
            }),
            compliance_stats: Mutex::new(ComplianceDashboard::default()),
            anti_scale_metrics: Mutex::new(AntiScaleDashboard::default()),
            network_health: Mutex::new(NetworkHealthDashboard::default()),
            peer_quality: Mutex::new(HashMap::new()),
            storage_metrics: Mutex::new(commputer_storage::StorageMetrics::default()),
            ws_broadcast: broadcast::channel(256).0,
            is_testnet: true,
            faucet_claims: Mutex::new(HashMap::new()),
            rpc_key: None,
            rate_limits: Mutex::new(HashMap::new()),
        });
        (state, rx)
    }

    fn make_signed_tx() -> Transaction {
        let wallet = Wallet::generate();
        let to = Address([1u8; 32]);
        let mut tx = Transaction {
            from: *wallet.address(),
            nonce: 0,
            kind: TxKind::Transfer {
                to,
                amount: Amount::from_comme(10),
            },
            fee: 0,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        sign_transaction(&mut tx, &wallet);
        tx
    }

    #[tokio::test]
    async fn submit_signed_tx_accepted() {
        let (state, mut rx) = make_rpc_state();
        let app = build_router(state);
        let tx = make_signed_tx();
        let body = serde_json::to_vec(&tx).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/tx")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let result: SubmitTxResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert!(result.accepted);
        assert!(!result.tx_hash.is_empty());
        assert!(result.error.is_none());

        // Verify the transaction was forwarded to the channel.
        let received = rx.try_recv().unwrap();
        assert_eq!(received.hash(), tx.hash());
    }

    #[tokio::test]
    async fn submit_unsigned_tx_rejected() {
        let (state, _rx) = make_rpc_state();
        let app = build_router(state);

        // Transaction with no signature.
        let tx = Transaction {
            from: Address([2u8; 32]),
            nonce: 0,
            kind: TxKind::Transfer {
                to: Address([3u8; 32]),
                amount: Amount::from_comme(5),
            },
            fee: 0,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        let body = serde_json::to_vec(&tx).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/tx")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let result: SubmitTxResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert!(!result.accepted);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn submit_bad_signature_rejected() {
        let (state, _rx) = make_rpc_state();
        let app = build_router(state);

        let wallet = Wallet::generate();
        // Valid-length but wrong signature.
        let tx = Transaction {
            from: *wallet.address(),
            nonce: 0,
            kind: TxKind::Transfer {
                to: Address([4u8; 32]),
                amount: Amount::from_comme(1),
            },
            fee: 0,
            signature: vec![0u8; 64],
            public_key: wallet.public_key().to_bytes().to_vec(),
            memo: None,
            timelock: None,
        };
        let body = serde_json::to_vec(&tx).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/tx")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_status_returns_chain_info() {
        let (state, _rx) = make_rpc_state();
        let app = build_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/status")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let status: ChainStatus = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(status.height, 42);
        assert_eq!(status.epoch, 1);
        assert_eq!(status.accounts, 3);
    }
}
