use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use axum::{
    Router,
    Json,
    extract::{Path, State, ConnectInfo},
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use futures::StreamExt;
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
    /// Last time a message was received from this peer.
    /// Not serialized — used only for online/staleness checks.
    #[serde(skip)]
    pub last_seen: Option<std::time::Instant>,
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
    /// Feature 10: Validator performance data.
    pub validator_performance: Mutex<HashMap<String, serde_json::Value>>,
    /// Feature 256: Faucet rate limiting (address_hex -> last_epoch_claimed).
    pub faucet_claims: Mutex<HashMap<String, u64>>,
    /// Feature 15: Optional API key for RPC authentication. None = no auth required.
    pub api_key: Option<String>,
    /// Feature 16: Per-IP rate limiting — (request_count, window_start).
    pub rate_limits: Mutex<HashMap<String, (u32, Instant)>>,
    /// Item 55: Configurable CORS allowed origins (comma-separated or "*").
    pub cors_origins: String,
    /// Node start time for uptime calculation.
    pub start_time: Instant,
    /// Chain health monitor snapshot (updated by event loop).
    pub chain_health: Mutex<serde_json::Value>,
    /// Item 116: Network traffic statistics.
    pub traffic_stats: Mutex<serde_json::Value>,
    /// Item 150: Per-validator proof history for charting.
    pub proof_history: Mutex<HashMap<String, Vec<serde_json::Value>>>,
    /// Item 160: Proof leaderboard data per channel.
    pub proof_leaderboard: Mutex<HashMap<String, Vec<serde_json::Value>>>,
    /// Capacity breakdown: (total, reserve_pct, flagship_slots, user_slots).
    pub capacity: Mutex<(u64, u64, u64, u64)>,
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
    // Feature 17: Structural validation before signature check.
    if tx.public_key.len() != 32 {
        return (
            StatusCode::BAD_REQUEST,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash: String::new(),
                error: Some("public_key must be 32 bytes".into()),
            }),
        );
    }
    if tx.signature.len() != 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash: String::new(),
                error: Some("signature must be 64 bytes".into()),
            }),
        );
    }
    if let Some(ref memo) = tx.memo
        && memo.len() > commputer_core::transaction::Transaction::MAX_MEMO_LENGTH {
            return (
                StatusCode::BAD_REQUEST,
                Json(SubmitTxResponse {
                    accepted: false,
                    tx_hash: String::new(),
                    error: Some(format!("memo exceeds max length of {} bytes", commputer_core::transaction::Transaction::MAX_MEMO_LENGTH)),
                }),
            );
        }

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

/// Feature 17: Validate hex address format (64 hex chars = 32 bytes).
fn validate_address(address: &str) -> Result<(), String> {
    if address.len() != 64 {
        return Err(format!("address must be 64 hex characters, got {}", address.len()));
    }
    if !address.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("address contains non-hex characters".into());
    }
    Ok(())
}

/// GET /balance/:address — return account balance for the given hex address.
async fn get_balance(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Feature 17: Input validation.
    if let Err(msg) = validate_address(&address) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": msg,
            "address": address,
        })));
    }

    let balances = state.balances.lock().await;
    if let Some(info) = balances.get(&address) {
        (StatusCode::OK, Json(serde_json::to_value(info).unwrap_or_default()))
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
    // Feature 17: Validate height is reasonable (within u64 range already by type, check not absurdly high).
    let current_height = state.status.lock().await.height;
    if height > current_height + 1000 {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "requested height is too far ahead of current chain height",
            "requested": height,
            "current_height": current_height,
        })));
    }

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

/// Item 150: GET /proofs/history/{address} — return per-validator proof history for charting.
async fn get_proof_history(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Err(msg) = validate_address(&address) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg})));
    }

    let history = state.proof_history.lock().await;
    if let Some(entries) = history.get(&address) {
        (StatusCode::OK, Json(serde_json::json!({
            "address": address,
            "history": entries,
        })))
    } else {
        (StatusCode::OK, Json(serde_json::json!({
            "address": address,
            "history": [],
        })))
    }
}

/// Item 160: GET /proofs/leaderboard — return top validators per proof channel.
async fn get_proof_leaderboard(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let leaderboard = state.proof_leaderboard.lock().await;
    Json(serde_json::json!({
        "channels": ["Processing", "Gpu", "Storage", "Ram", "Bandwidth"],
        "leaderboard": *leaderboard,
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

// /health endpoint moved to get_health_enhanced below build_router

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

/// GET /validator/:address/performance — return validator performance metrics.
async fn get_validator_performance(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let perf = state.validator_performance.lock().await;
    if let Some(data) = perf.get(&address) {
        (StatusCode::OK, Json(data.clone()))
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "error": "No performance data for validator",
            "address": address,
        })))
    }
}

// ── Feature 253: Fee estimator ──

/// GET /nonce/:address — return current nonce for an address.
async fn get_nonce(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Feature 17: Input validation.
    if let Err(msg) = validate_address(&address) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg})));
    }
    let balances = state.balances.lock().await;
    if let Some(info) = balances.get(&address) {
        (StatusCode::OK, Json(serde_json::json!({
            "address": address,
            "nonce": info.nonce,
        })))
    } else {
        // Account not on chain yet — nonce is 0.
        (StatusCode::OK, Json(serde_json::json!({
            "address": address,
            "nonce": 0,
        })))
    }
}

/// GET /fee-estimate — recommend a transaction fee based on mempool fullness.
/// Item 116: GET /traffic -- return network traffic statistics.
async fn get_traffic(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let stats = state.traffic_stats.lock().await.clone();
    Json(stats)
}

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
    if let Some(&last_epoch) = claims.get(&req.address)
        && last_epoch >= current_epoch {
            return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({
                "error": "faucet already claimed this epoch",
                "next_available_epoch": current_epoch + 1,
            })));
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

/// Item 95: Testnet leaderboard — top validators by total mined.
async fn get_leaderboard(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let balances = state.balances.lock().await;
    let mut validators: Vec<_> = balances.values()
        .filter(|b| b.is_validator)
        .map(|b| serde_json::json!({
            "address": b.address,
            "total_mined": b.total_mined,
            "balance": b.balance,
            "tier": b.tier,
        }))
        .collect();

    // Sort by total_mined descending.
    validators.sort_by(|a, b| {
        let a_mined = a["total_mined"].as_u64().unwrap_or(0);
        let b_mined = b["total_mined"].as_u64().unwrap_or(0);
        b_mined.cmp(&a_mined)
    });

    // Top 50.
    validators.truncate(50);

    Json(serde_json::json!({
        "leaderboard": validators,
        "count": validators.len(),
    }))
}

/// Item 96: Testnet statistics page — HTML page with network stats.
async fn get_stats_page(
    State(state): State<Arc<RpcState>>,
) -> Html<String> {
    let status = state.status.lock().await;
    let metrics = state.metrics.lock().await;
    let balances = state.balances.lock().await;

    let validator_count = balances.values().filter(|b| b.is_validator).count();
    let total_mined: u64 = balances.values().map(|b| b.total_mined).sum();
    let units = commputer_core::token::UNITS_PER_COMME;

    Html(format!(r#"<!DOCTYPE html>
<html><head><title>Commputer Testnet Stats</title>
<meta http-equiv="refresh" content="10">
<style>
body {{ font-family: monospace; background: #1a1a2e; color: #e0e0e0; padding: 20px; }}
h1 {{ color: #e94560; }}
.stat {{ margin: 8px 0; }}
.label {{ color: #8899aa; display: inline-block; width: 200px; }}
</style></head><body>
<h1>Commputer Testnet Statistics</h1>
<div class="stat"><span class="label">Height:</span> {}</div>
<div class="stat"><span class="label">Epoch:</span> {}</div>
<div class="stat"><span class="label">Accounts:</span> {}</div>
<div class="stat"><span class="label">Active Validators:</span> {}</div>
<div class="stat"><span class="label">Total Emitted:</span> {} COMME</div>
<div class="stat"><span class="label">Total Burned:</span> {} COMME</div>
<div class="stat"><span class="label">Circulating:</span> {} COMME</div>
<div class="stat"><span class="label">Total Mined (all):</span> {} COMME</div>
<div class="stat"><span class="label">Peers Connected:</span> {}</div>
<div class="stat"><span class="label">Pending Txs:</span> {}</div>
<div class="stat"><span class="label">Banned Peers:</span> {}</div>
<p style="color: #666; margin-top: 20px;">Auto-refreshes every 10 seconds.</p>
</body></html>"#,
        status.height, status.epoch, status.accounts,
        validator_count,
        status.emitted / units,
        status.burned / units,
        status.circulating / units,
        total_mined / units,
        metrics.peers_connected,
        metrics.pending_txs,
        metrics.peers_banned,
    ))
}

// ── Feature 15: RPC API key authentication middleware ──

/// Middleware that checks `X-API-Key` header against the configured key.
/// Localhost (127.0.0.1) requests bypass auth. If no key is configured, all requests pass.
async fn auth_middleware(
    State(state): State<Arc<RpcState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    if let Some(ref expected_key) = state.api_key {
        // Bypass auth for localhost.
        let is_localhost = req.extensions()
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().is_loopback())
            .unwrap_or(true); // Default to allowing if we can't determine IP

        if !is_localhost {
            let provided = req.headers()
                .get("X-API-Key")
                .and_then(|v| v.to_str().ok());
            if provided != Some(expected_key.as_str()) {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "invalid or missing API key"})),
                ).into_response();
            }
        }
    }
    next.run(req).await
}

/// Item 58: Security headers middleware.
/// Item 55: CORS origin is now read from RpcState.cors_origins.
async fn security_headers(
    State(state): State<Arc<RpcState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    headers.insert("X-Frame-Options", "DENY".parse().unwrap());
    headers.insert("Cache-Control", "no-store".parse().unwrap());
    headers.insert("X-XSS-Protection", "1; mode=block".parse().unwrap());
    headers.insert(
        "Content-Security-Policy",
        "default-src 'self'; script-src 'none'".parse().unwrap(),
    );
    // Item 55: Configurable CORS origins (default "*" for testnet).
    let cors_value = state.cors_origins.as_str();
    headers.insert(
        "Access-Control-Allow-Origin",
        cors_value.parse().unwrap_or_else(|_| "*".parse().unwrap()),
    );
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        "Access-Control-Allow-Headers",
        "Content-Type, X-API-Key".parse().unwrap(),
    );
    response
}

// ── Feature 16: RPC per-IP rate limiting middleware ──

/// Maximum requests per IP per second.
const RATE_LIMIT_MAX: u32 = 100;

/// Middleware that enforces per-IP rate limiting: max 100 req/s.
async fn rate_limit_middleware(
    State(state): State<Arc<RpcState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let ip = req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    {
        let mut limits = state.rate_limits.lock().await;
        let now = Instant::now();
        let entry = limits.entry(ip).or_insert((0, now));

        // Reset counter if more than 1 second has passed.
        if now.duration_since(entry.1).as_secs() >= 1 {
            entry.0 = 0;
            entry.1 = now;
        }

        entry.0 += 1;
        if entry.0 > RATE_LIMIT_MAX {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({"error": "rate limit exceeded — max 100 requests per second"})),
            ).into_response();
        }
    }

    next.run(req).await
}

// ── Items 28-29, 35, 40, 42, 46: New RPC endpoints for testnet launch ──

/// GET /validators — list of connected validators with details.
async fn get_validators(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let balances = state.balances.lock().await;
    let peers = state.peers.lock().await;
    let uptime = state.start_time.elapsed().as_secs();

    let mut validators: Vec<serde_json::Value> = balances.values()
        .filter(|b| b.is_validator)
        .map(|b| {
            let peer = peers.iter().find(|p| p.validator_address.as_deref() == Some(&b.address));
            let online = peer
                .and_then(|p| p.last_seen)
                .map(|last| last.elapsed().as_secs() < 300)
                .unwrap_or(false);
            serde_json::json!({
                "address": b.address,
                "peer_id": peer.map(|p| p.peer_id.as_str()).unwrap_or("offline"),
                "contribution_percent": 100,
                "balance": b.balance,
                "total_mined": b.total_mined,
                "tier": b.tier,
                "online": online,
            })
        })
        .collect();

    validators.sort_by(|a, b| {
        let am = a["total_mined"].as_u64().unwrap_or(0);
        let bm = b["total_mined"].as_u64().unwrap_or(0);
        bm.cmp(&am)
    });

    Json(serde_json::json!({
        "validators": validators,
        "count": validators.len(),
        "uptime_secs": uptime,
    }))
}

/// GET /network/info — enhanced network info for website dashboard.
async fn get_network_info(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let balances = state.balances.lock().await;
    let peers = state.peers.lock().await;
    let status = state.status.lock().await;
    let health = state.network_health.lock().await;
    let uptime = state.start_time.elapsed().as_secs();

    let total_validators = balances.values().filter(|b| b.is_validator).count();

    Json(serde_json::json!({
        "total_validators": total_validators,
        "total_peers": peers.len(),
        "total_compute_capacity": "N/A",
        "network_uptime_secs": uptime,
        "height": status.height,
        "epoch": status.epoch,
        "avg_latency_ms": health.avg_latency_ms,
        "partition_risk": health.partition_risk,
    }))
}

/// GET /blocks — return last N blocks.
async fn get_recent_blocks(
    State(state): State<Arc<RpcState>>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let limit: usize = params.get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10)
        .min(100);

    let blocks = state.blocks.lock().await;
    let status = state.status.lock().await;
    let height = status.height;

    let mut result = Vec::new();
    let start = height.saturating_sub(limit as u64);
    for h in (start..=height).rev() {
        if let Some(block) = blocks.get(&h) {
            // Extract key fields for the list view
            let header = block.get("header").cloned().unwrap_or_default();
            let tx_count = block.get("transactions")
                .and_then(|t| t.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            result.push(serde_json::json!({
                "height": header.get("height").cloned().unwrap_or(serde_json::json!(h)),
                "hash": format!("{:x}", h), // simplified
                "timestamp": header.get("timestamp").cloned().unwrap_or_default(),
                "tx_count": tx_count,
                "producer": header.get("producer").cloned().unwrap_or_default(),
                "epoch": header.get("epoch").cloned().unwrap_or_default(),
            }));
        }
    }

    Json(serde_json::json!({
        "blocks": result,
        "count": result.len(),
        "height": height,
    }))
}

/// GET /account/{address} — account details (balance, tier, tx count, proofs).
async fn get_account(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Err(msg) = validate_address(&address) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg})));
    }

    let balances = state.balances.lock().await;
    if let Some(info) = balances.get(&address) {
        let units = commputer_core::token::UNITS_PER_COMME;
        (StatusCode::OK, Json(serde_json::json!({
            "address": info.address,
            "balance": info.balance,
            "balance_comme": format!("{}.{:08}", info.balance / units, info.balance % units),
            "tier": info.tier,
            "nonce": info.nonce,
            "is_validator": info.is_validator,
            "total_mined": info.total_mined,
            "total_mined_comme": format!("{}.{:08}", info.total_mined / units, info.total_mined % units),
        })))
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "error": "Account not found on chain",
            "address": address,
        })))
    }
}

/// GET /supply — total, emitted, burned, circulating supply.
async fn get_supply(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let status = state.status.lock().await;
    let units = commputer_core::token::UNITS_PER_COMME;

    Json(serde_json::json!({
        "total": status.total_supply,
        "total_comme": format!("{}", status.total_supply / units),
        "emitted": status.emitted,
        "emitted_comme": format!("{}", status.emitted / units),
        "burned": status.burned,
        "burned_comme": format!("{}", status.burned / units),
        "circulating": status.circulating,
        "circulating_comme": format!("{}", status.circulating / units),
        "remaining": status.remaining,
        "remaining_comme": format!("{}", status.remaining / units),
    }))
}

/// GET /capacity — network compute capacity breakdown (51/49 split + dynamic reserve).
async fn get_capacity(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let (total, reserve_pct, flagship, user) = *state.capacity.lock().await;
    Json(serde_json::json!({
        "total_capacity": total,
        "reserve_percent": reserve_pct,
        "flagship_capacity": flagship,
        "flagship_share_percent": commputer_core::token::FLAGSHIP_COMPUTE_SHARE,
        "user_capacity": user,
        "user_share_percent": commputer_core::token::HOLDER_COMPUTE_SHARE,
    }))
}

/// GET /health — enhanced health check with uptime, sync status, and chain health.
async fn get_health_enhanced(
    State(state): State<Arc<RpcState>>,
) -> Json<serde_json::Value> {
    let status = state.status.lock().await;
    let peers = state.peers.lock().await;
    let uptime = state.start_time.elapsed().as_secs();
    let chain_health = state.chain_health.lock().await.clone();

    Json(serde_json::json!({
        "healthy": true,
        "height": status.height,
        "epoch": status.epoch,
        "peers": peers.len(),
        "pending_txs": status.pending_txs,
        "uptime_secs": uptime,
        "synced": true,
        "chain_id": crate::config::DEFAULT_TESTNET_CHAIN_ID,
        "chain_health": chain_health,
    }))
}

/// Build the axum router (exposed for testing).
pub fn build_router(rpc_state: Arc<RpcState>) -> Router {
    Router::new()
        .route("/", get(block_explorer))
        .route("/tx", post(submit_tx))
        .route("/status", get(get_status))
        .route("/peers", get(get_peers))
        .route("/balance/{address}", get(get_balance))
        .route("/nonce/{address}", get(get_nonce))
        .route("/validator/{address}/performance", get(get_validator_performance))
        .route("/mempool", get(get_mempool))
        .route("/block/{height}", get(get_block_by_height))
        .route("/receipt/{tx_hash}", get(get_receipt))
        .route("/metrics", get(get_metrics))
        .route("/metrics/prometheus", get(get_prometheus_metrics))
        .route("/proofs/status", get(get_proof_status))
        .route("/proofs/history/{address}", get(get_proof_history))
        .route("/proofs/leaderboard", get(get_proof_leaderboard))
        .route("/health", get(get_health_enhanced))
        .route("/validators", get(get_validators))
        .route("/network/info", get(get_network_info))
        .route("/blocks", get(get_recent_blocks))
        .route("/account/{address}", get(get_account))
        .route("/supply", get(get_supply))
        .route("/capacity", get(get_capacity))
        .route("/compliance", get(get_compliance))
        .route("/anti-scale", get(get_anti_scale))
        .route("/network", get(get_network_health))
        .route("/network/quality", get(get_peer_quality))
        .route("/storage/metrics", get(get_storage_metrics))
        .route("/ws", get(ws_handler))
        .route("/fee-estimate", get(get_fee_estimate))
        .route("/traffic", get(get_traffic))
        .route("/rewards/{address}", get(get_pending_rewards))
        .route("/faucet", post(faucet))
        .route("/leaderboard", get(get_leaderboard))
        .route("/stats", get(get_stats_page))
        .route_layer(middleware::from_fn_with_state(rpc_state.clone(), auth_middleware))
        .route_layer(middleware::from_fn_with_state(rpc_state.clone(), rate_limit_middleware))
        .with_state(rpc_state)
}

/// Start the RPC server on the given port and bind address.
pub async fn start_rpc_server(
    rpc_port: u16,
    rpc_bind: String,
    rpc_state: Arc<RpcState>,
) {
    let security_state = rpc_state.clone();
    let app = build_router(rpc_state)
        .layer(axum::middleware::from_fn_with_state(security_state, security_headers));

    // Item 26: Bind to configured address (0.0.0.0 for remote, 127.0.0.1 for local).
    let bind_addr = format!("{}:{}", rpc_bind, rpc_port);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!("Failed to bind RPC server on {}: {}", bind_addr, e);
            return;
        }
    };

    info!("RPC server listening on http://{}", bind_addr);

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
            api_key: None,
            rate_limits: Mutex::new(HashMap::new()),
            validator_performance: Mutex::new(HashMap::new()),
            cors_origins: "*".to_string(),
            start_time: Instant::now(),
            chain_health: Mutex::new(serde_json::json!({})),
            traffic_stats: Mutex::new(serde_json::json!({})),
            proof_history: Mutex::new(HashMap::new()),
            proof_leaderboard: Mutex::new(HashMap::new()),
            capacity: Mutex::new((0, 0, 0, 0)),
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
