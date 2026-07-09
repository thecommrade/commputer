use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use axum::{
    Router,
    Json,
    extract::{DefaultBodyLimit, Path, State, ConnectInfo},
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

/// CANONICAL RPC LOCK ORDER (deadlock safety).
///
/// `RpcState` holds many independent `tokio::sync::Mutex` guards. Several public,
/// unauthenticated handlers acquire two or more of them while holding the first
/// across the `.await` that acquires the next. If two handlers acquired the same
/// pair in opposite orders, concurrent requests could deadlock permanently and
/// wedge the whole RPC surface (every later handler that locks those mutexes then
/// parks forever). To make that impossible, EVERY handler that needs more than one
/// of these mutexes MUST acquire them in this fixed order (lower first):
///
///   status → metrics → balances → peers → blocks → network_health → chain_health
///
/// (Other single-purpose mutexes — mempool, receipts, faucet_claims, rate_limits,
/// etc. — are only ever held one at a time, so they are exempt.) When adding a new
/// handler, either follow this order or take one lock, copy out what you need, drop
/// the guard, then take the next.
///
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
    // W5.7 F-1: structural validation BEFORE signature verify (cheap,
    // catches body-bombs and malformed Batch / MultiSig shapes).
    if let Err(reason) = tx.validate_shape() {
        return (
            StatusCode::BAD_REQUEST,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash: String::new(),
                error: Some(reason.into()),
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
/// W5.7 F-5: the `ip` field is redacted on this public route to prevent
/// validator-topology enumeration. Operators with a legitimate need read
/// IPs from node logs. A future /peers/full behind --rpc-key is out of scope.
async fn get_peers(
    State(state): State<Arc<RpcState>>,
) -> Json<Vec<PeerInfo>> {
    let peers = state.peers.lock().await.clone();
    let redacted: Vec<PeerInfo> = peers
        .into_iter()
        .map(|p| PeerInfo { ip: None, ..p })
        .collect();
    Json(redacted)
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

/// Security: maximum number of concurrent public `/ws` connections. `/ws` is an
/// unauthenticated route and each upgrade pins a file descriptor + a spawned task
/// + a broadcast receiver; without a cap a client can open unbounded persistent
/// connections and exhaust fds/memory. Generous — real dashboards/light-clients
/// use a handful per host.
pub const MAX_WS_CONNECTIONS: usize = 512;

/// Security: number of currently-open `/ws` connections (process-global).
static WS_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

/// RAII slot for a live `/ws` connection. Reserving one increments the global
/// counter; dropping it (connection closed, upgrade aborted, or send timeout)
/// decrements it — so the count stays accurate on every exit path.
struct WsConnGuard;

impl Drop for WsConnGuard {
    fn drop(&mut self) {
        WS_CONNECTIONS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Try to reserve a `/ws` connection slot. Returns `Some(guard)` when under the
/// cap (caller proceeds with the upgrade), or `None` when the cap is reached
/// (caller must reject). The reservation is released when the returned guard is
/// dropped.
fn try_reserve_ws_slot() -> Option<WsConnGuard> {
    let prev = WS_CONNECTIONS.fetch_add(1, Ordering::AcqRel);
    if prev >= MAX_WS_CONNECTIONS {
        // Over the cap — undo the reservation and reject.
        WS_CONNECTIONS.fetch_sub(1, Ordering::AcqRel);
        None
    } else {
        Some(WsConnGuard)
    }
}

/// GET /ws — upgrade to WebSocket for real-time event streaming.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<RpcState>>,
) -> Response {
    // Security: cap concurrent public WS connections (fd/memory exhaustion DoS).
    // The reserved slot is handed to handle_ws, which holds it until the socket
    // closes; if the upgrade never completes the guard is dropped and the slot
    // is released.
    let Some(guard) = try_reserve_ws_slot() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "websocket connection limit reached",
        )
            .into_response();
    };
    ws.on_upgrade(move |socket| handle_ws(socket, state, guard))
}

async fn handle_ws(socket: WebSocket, state: Arc<RpcState>, guard: WsConnGuard) {
    use futures::SinkExt as _;
    let (mut sink, _stream) = socket.split();
    let mut rx = state.ws_broadcast.subscribe();

    // Send events to the client until they disconnect. `guard` is moved into the
    // task and released when the loop ends, keeping the concurrent-connection
    // count accurate.
    tokio::spawn(async move {
        let _guard = guard;
        while let Ok(msg) = rx.recv().await {
            let text: Message = Message::Text(msg.into());
            // Security: bound how long a single send may block so a client that
            // completes the upgrade but never reads cannot pin the fd/task
            // forever (backpressure parks sink.send indefinitely otherwise).
            match tokio::time::timeout(std::time::Duration::from_secs(30), sink.send(text)).await {
                Ok(Ok(())) => {}
                // Send error or slow-consumer timeout — drop the connection.
                _ => break,
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
    // Canonical lock order (see RpcState): status before balances.
    let status = state.status.lock().await;
    let balances = state.balances.lock().await;

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

/// A-batch item 6: build + sign a faucet Transfer of exactly 1 COMME to `to`,
/// using `nonce` as the tx nonce. Fee is MINIMUM_FEE because Transfers are not
/// fee-exempt in the mempool. Signed with `sign_transaction` (no chain_id — the
/// signature form `tx.verify()` checks), so a faucet tx passes the mempool gate.
///
/// INERT SUBSTRATE: this is the money-path builder the D6 faucet dispenser will
/// call once a faucet wallet is provisioned. It is NOT yet reachable from a
/// handler — wiring it requires adding `faucet_wallet`/`faucet_next_nonce` fields
/// to `RpcState`, which forces edits to the RpcState struct literal in the
/// PROTECTED `main.rs`; that is the founder-gated D6 step at the alpha reset (see
/// src/staging/docs/real_faucet_blueprint.md). Kept `#[allow(dead_code)]` so the
/// verified builder ships now with zero new warnings and the D6 protected edit is
/// minimal. Verified by `build_faucet_transfer_makes_valid_signed_1_comme`.
#[allow(dead_code)]
fn build_faucet_transfer(
    wallet: &commputer_core::wallet::Wallet,
    to: commputer_core::identity::Address,
    nonce: u64,
) -> Transaction {
    let mut tx = Transaction {
        from: *wallet.address(),
        nonce,
        kind: commputer_core::transaction::TxKind::Transfer {
            to,
            // 1 COMME in raw units.
            amount: commputer_core::token::Amount::from_raw(
                commputer_core::token::UNITS_PER_COMME,
            ),
        },
        fee: commputer_core::transaction::MINIMUM_FEE,
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    };
    commputer_core::signing::sign_transaction(&mut tx, wallet);
    tx
}

/// Provision the live testnet faucet from the environment (E10/E11 + P8).
///
/// Reads the faucet seed phrase from `COMMPUTER_FAUCET_SEED`, derives the faucet
/// signing wallet, and computes its next nonce from on-chain state. Security
/// contract:
///   * The phrase is `zeroize`d out of memory AND `remove_var`'d from the
///     environment immediately after use, so it cannot leak to child processes,
///     a later reader, or a core dump. The phrase (and any error carrying it) is
///     NEVER logged.
///   * FAIL-CLOSED (P8): with the env var set, the node refuses to bind unless the
///     seed is a valid 24-word phrase AND the derived address equals the compiled
///     `testnet_genesis::ALPHA_FAUCET_ADDRESS_HEX`. This guarantees the funded /
///     mempool-exempted identity and the live signing wallet are the same key —
///     a mismatch would strand the nonce counter and brick the faucet.
///   * With the env var UNSET the faucet is disabled: returns `Ok((None, 0))`.
///
/// Returns `(Some(wallet), next_nonce)` when provisioned. INERT until the PROTECTED
/// `main.rs` boot path calls it and threads the pair into `RpcState`
/// (`faucet_wallet` / `faucet_next_nonce`) at the alpha reset — those struct fields
/// and the `/faucet` handler rewire ride the protected commit, not this helper.
#[allow(dead_code)]
pub fn provision_faucet_from_env(
    state: &commputer_storage::state::ChainState,
) -> anyhow::Result<(Option<commputer_core::wallet::Wallet>, u64)> {
    use zeroize::Zeroize;

    // Absent env var ⇒ faucet disabled. No phrase to scrub.
    let mut phrase = match std::env::var("COMMPUTER_FAUCET_SEED") {
        Ok(p) => p,
        Err(_) => return Ok((None, 0)),
    };

    // Remove it from the environment immediately so it cannot leak to child
    // processes or a later env reader.
    // SAFETY: invoked once during single-threaded boot, before the RPC/worker
    // tasks that could race on the environment are spawned (Rust 2024 marks env
    // mutation `unsafe` solely because of cross-thread data races).
    unsafe {
        std::env::remove_var("COMMPUTER_FAUCET_SEED");
    }

    // Derive the wallet, then scrub the phrase from memory regardless of outcome.
    // The error path deliberately discards the underlying parse error so the
    // (possibly phrase-bearing) message never reaches a log.
    let wallet_result = commputer_core::wallet::Wallet::from_seed_phrase(&phrase);
    phrase.zeroize();

    let wallet = wallet_result.map_err(|_| {
        anyhow::anyhow!(
            "COMMPUTER_FAUCET_SEED is set but is not a valid 24-word seed phrase; refusing to bind"
        )
    })?;

    // P8 fail-closed: compiled faucet address MUST equal the derived signing wallet.
    let derived_hex = hex::encode(wallet.address().0);
    match crate::testnet_genesis::ALPHA_FAUCET_ADDRESS_HEX {
        Some(expected) if expected == derived_hex => {}
        _ => {
            return Err(anyhow::anyhow!(
                "COMMPUTER_FAUCET_SEED wallet does not match compiled ALPHA_FAUCET_ADDRESS_HEX; refusing to bind"
            ));
        }
    }

    // Seed the dispenser nonce from on-chain state (0 if the account is unseen).
    let next_nonce = state
        .accounts
        .get(wallet.address())
        .map(|a| a.nonce)
        .unwrap_or(0);

    Ok((Some(wallet), next_nonce))
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
    let claims = state.faucet_claims.lock().await;
    if let Some(&last_epoch) = claims.get(&req.address)
        && last_epoch >= current_epoch {
            return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({
                "error": "faucet already claimed this epoch",
                "next_available_epoch": current_epoch + 1,
            })));
        }

    // W5.7 F-6: HONESTY FIX. The faucet has no provisioned signing wallet:
    // RpcState carries no faucet keypair, and no funded faucet/treasury
    // account exists in genesis. Previously this handler inserted the
    // rate-limit claim and returned {success:true, "1 COMME dispensed"}
    // WITHOUT ever building, signing, or queueing a Transfer — it lied.
    // Until a faucet wallet is wired (requires a protected-file change to
    // main.rs RpcState construction + a funded faucet account in genesis),
    // return 503 instead of a false success. Do NOT consume the per-epoch
    // claim slot on a request we cannot fulfill.
    drop(claims);

    (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
        "error": "faucet not provisioned",
        "detail": "no faucet wallet is configured on this node; tokens cannot be dispensed",
        "address": req.address,
        "epoch": current_epoch,
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

/// A4-auth-loopback: code-level opt-out for the legacy loopback auth bypass.
///
/// `false` (default, secure) — when an API key is configured the key is
/// required for EVERY caller, including loopback (127.0.0.1 / ::1) and callers
/// whose source IP cannot be determined. This is the correct default: a
/// configured key is an explicit decision to require auth, and "trusted
/// because it came from loopback" is unsafe on multi-tenant / containerized
/// hosts where many unrelated processes share 127.0.0.1.
///
/// `true` — restores the old convenience: loopback callers skip the key check.
/// Only flip this if you fully control every process on the host AND accept
/// that any of them can drive the RPC unauthenticated. There is intentionally
/// no CLI flag for this (src/node/src/main.rs is protected); the opt-out is a
/// single auditable source line that ships closed.
const ALLOW_LOOPBACK_BYPASS: bool = false;

/// Middleware that checks `X-API-Key` header against the configured key.
///
/// If no key is configured, all requests pass (unchanged default).
///
/// A4-auth-loopback: if a key IS configured, the key is required for every
/// caller. Loopback no longer bypasses auth unless `ALLOW_LOOPBACK_BYPASS` is
/// set to `true` at build time. When the bypass is disabled (default) a caller
/// with no determinable source IP is treated as untrusted and must present the
/// key (fail-closed).
async fn auth_middleware(
    State(state): State<Arc<RpcState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    if let Some(ref expected_key) = state.api_key {
        // A4-auth-loopback: by default, enforce for everyone. Only when the
        // build-time opt-out is enabled do we exempt loopback callers.
        let exempt = if ALLOW_LOOPBACK_BYPASS {
            req.extensions()
                .get::<ConnectInfo<std::net::SocketAddr>>()
                .map(|ci| ci.0.ip().is_loopback())
                // Cannot determine the source IP -> fail closed (NOT exempt),
                // even under the opt-out. The old code defaulted this to `true`
                // (exempt), which is exactly the hole this patch closes.
                .unwrap_or(false)
        } else {
            false
        };

        if !exempt {
            let provided = req.headers()
                .get("X-API-Key")
                .and_then(|v| v.to_str().ok());
            // A-batch item 3: constant-time key comparison. A byte-wise `==` on
            // the secret leaks its matching prefix through timing; constant_time_eq
            // compares equal-length inputs without early-exit. (Like the standard
            // `subtle`/`constant_time_eq`, it does short-circuit on a length
            // mismatch, so the key *length* is not hidden — negligible for a
            // high-entropy random key; the prefix-timing-oracle is what matters.)
            // A missing header (None) is a straight reject (fail-closed).
            let authorized = match provided {
                Some(p) => commputer_core::audit::constant_time_eq(
                    p.as_bytes(),
                    expected_key.as_bytes(),
                ),
                None => false,
            };
            if !authorized {
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
///
/// A-batch item 4: CORS headers were removed from here and moved to
/// `cors_middleware` (installed inside `build_router`). The old code emitted the
/// raw `cors_origins` string as `Access-Control-Allow-Origin`, which comma-joins
/// a multi-origin allowlist into a single spec-invalid header; `cors_middleware`
/// echoes exactly one allowlisted origin (or `*`) and adds an OPTIONS preflight.
/// State is retained on the signature (unused) so `start_rpc_server`'s
/// `from_fn_with_state` wiring is untouched.
async fn security_headers(
    State(_state): State<Arc<RpcState>>,
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
    response
}

// ── A-batch item 4: CORS (allowlist echo + OPTIONS preflight) ──

/// Decide the single `Access-Control-Allow-Origin` value to emit for a request.
///
/// * Configured allowlist `"*"` (the testnet default) → always `Some("*")`.
/// * Otherwise the allowlist is a comma-separated set of exact origins: echo the
///   request `Origin` ONLY when it matches an entry exactly.
///
/// Fail-closed: a missing/unmatched `Origin` under a non-wildcard allowlist
/// returns `None` (no ACAO header emitted). Never comma-joins multiple origins.
fn cors_allow_origin(cors_origins: &str, request_origin: Option<&str>) -> Option<String> {
    let allow = cors_origins.trim();
    if allow == "*" {
        return Some("*".to_string());
    }
    let origin = request_origin?;
    for entry in allow.split(',') {
        if entry.trim() == origin {
            return Some(origin.to_string());
        }
    }
    None
}

/// Write the CORS response headers. When `allow_origin` is `None`, no
/// `Access-Control-Allow-Origin` is written (fail-closed).
fn apply_cors_headers(headers: &mut axum::http::HeaderMap, allow_origin: Option<&str>) {
    if let Some(origin) = allow_origin
        && let Ok(v) = origin.parse::<axum::http::HeaderValue>()
    {
        headers.insert("Access-Control-Allow-Origin", v);
        // A per-origin ACAO must not be cached across origins.
        headers.insert("Vary", axum::http::HeaderValue::from_static("Origin"));
    }
    headers.insert(
        "Access-Control-Allow-Methods",
        axum::http::HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        "Access-Control-Allow-Headers",
        axum::http::HeaderValue::from_static("Content-Type, X-API-Key"),
    );
}

/// CORS middleware. Installed as the OUTERMOST layer of `build_router`, so an
/// `OPTIONS` preflight is answered here BEFORE auth/rate-limit run — a browser
/// preflight carries no credentials and must not be rejected by the admin-key
/// gate. Non-preflight responses get the echoed ACAO appended on the way out.
async fn cors_middleware(
    State(state): State<Arc<RpcState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let request_origin = req.headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let allow_origin = cors_allow_origin(&state.cors_origins, request_origin.as_deref());

    if req.method() == axum::http::Method::OPTIONS {
        let mut resp = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(axum::body::Body::empty())
            .expect("static preflight response is valid");
        apply_cors_headers(resp.headers_mut(), allow_origin.as_deref());
        return resp;
    }

    let mut response = next.run(req).await;
    apply_cors_headers(response.headers_mut(), allow_origin.as_deref());
    response
}

// ── Feature 16: RPC per-IP rate limiting middleware ──

/// Maximum requests per IP per second.
const RATE_LIMIT_MAX: u32 = 100;
/// W5.7 F-2: bound the rate_limits map so it cannot OOM the node under
/// CGNAT churn or a spoofed-source flood. When this cap is reached we
/// sweep entries older than EVICT_AFTER and drop them.
const MAX_RATE_LIMIT_ENTRIES: usize = 100_000;
const EVICT_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// A-batch item 2: trusted-proxy predicate for client-IP derivation. DEFAULT
/// POLICY: only loopback is trusted — the D3 TLS reverse proxy runs on the same
/// host as the node, so it reaches the RPC over 127.0.0.1/::1. Forwarding headers
/// (`X-Forwarded-For` / `CF-Connecting-IP`) are honored ONLY when the socket peer
/// is trusted; a direct remote peer can never spoof its rate-limit identity via a
/// header. No config/main.rs change: the trust set is this single source line.
fn is_trusted_proxy(ip: &std::net::IpAddr) -> bool {
    ip.is_loopback()
}

/// A-batch item 2: derive the per-IP rate-limit key from the request. When the
/// socket peer is a trusted proxy, take the rightmost `X-Forwarded-For` entry
/// that is itself NOT a trusted proxy (this peels any chained trusted hops so the
/// real client is used), then fall back to `CF-Connecting-IP`. When the peer is
/// untrusted, use the socket IP verbatim and IGNORE all forwarding headers. A
/// request with no determinable socket peer collapses to a shared "unknown"
/// bucket (fail-closed to shared, never to per-header trust).
fn rate_limit_client_ip(req: &axum::http::Request<axum::body::Body>) -> String {
    let Some(socket_ip) = req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
    else {
        return "unknown".to_string();
    };

    if is_trusted_proxy(&socket_ip) {
        if let Some(xff) = req.headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            for part in xff.split(',').rev() {
                if let Ok(ip) = part.trim().parse::<std::net::IpAddr>()
                    && !is_trusted_proxy(&ip)
                {
                    return ip.to_string();
                }
            }
        }
        if let Some(cf) = req.headers()
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
            && let Ok(ip) = cf.trim().parse::<std::net::IpAddr>()
        {
            return ip.to_string();
        }
    }

    socket_ip.to_string()
}

/// Middleware that enforces per-IP rate limiting: max 100 req/s.
async fn rate_limit_middleware(
    State(state): State<Arc<RpcState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let ip = rate_limit_client_ip(&req);

    {
        let mut limits = state.rate_limits.lock().await;
        let now = Instant::now();

        // W5.7 F-2: bounded eviction. Cheap below the cap; one O(n) sweep
        // at the cap. Amortized O(1) per request even under hostile churn.
        if limits.len() >= MAX_RATE_LIMIT_ENTRIES {
            limits.retain(|_, (_, ts)| now.duration_since(*ts) < EVICT_AFTER);
        }

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
    // Canonical lock order (see RpcState): status → balances → peers → network_health.
    let status = state.status.lock().await;
    let balances = state.balances.lock().await;
    let peers = state.peers.lock().await;
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

    // Canonical lock order (see RpcState): status before blocks.
    let status = state.status.lock().await;
    let blocks = state.blocks.lock().await;
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

/// GET /peers/full — A-batch item 5 (D3): UN-redacted peer list including IPs.
/// ADMIN-ONLY — lives in the keyed admin tier (auth_middleware), so
/// validator-topology IPs are never exposed on the public surface. The public
/// /peers route (get_peers) redacts the same field.
async fn get_peers_full(
    State(state): State<Arc<RpcState>>,
) -> Json<Vec<PeerInfo>> {
    let peers = state.peers.lock().await.clone();
    Json(peers)
}

/// Build the axum router (exposed for testing).
///
/// A-batch item 5 (D3 public/keyed split). Two route tiers merged into one app:
///
/// * PUBLIC — read-only GETs + POST /tx + POST /faucet + the /ws event stream.
///   Rate-limit layer only; intentionally reachable WITHOUT an API key so a
///   public testnet is usable behind the D3 TLS reverse proxy.
/// * ADMIN — operator diagnostics that leak topology/host internals
///   (/metrics, /metrics/prometheus, /storage/metrics, /traffic,
///   /network/quality, /compliance, /anti-scale, /peers/full). Rate-limit AND
///   auth_middleware, so these require the admin key whenever one is configured.
///
/// /ws is deliberately PUBLIC: it only streams already-public broadcast events
/// (the same data the public GET routes expose), so it needs no key.
///
/// cors_middleware is applied as the OUTERMOST layer (over the whole merged app)
/// so OPTIONS preflight is answered before auth/rate-limit.
pub fn build_router(rpc_state: Arc<RpcState>) -> Router {
    // PUBLIC tier — rate-limited, no auth.
    let public = Router::new()
        .route("/", get(block_explorer))
        .route(
            "/tx",
            post(submit_tx).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/status", get(get_status))
        .route("/peers", get(get_peers))
        .route("/balance/{address}", get(get_balance))
        .route("/nonce/{address}", get(get_nonce))
        .route("/validator/{address}/performance", get(get_validator_performance))
        .route("/mempool", get(get_mempool))
        .route("/block/{height}", get(get_block_by_height))
        .route("/receipt/{tx_hash}", get(get_receipt))
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
        .route("/network", get(get_network_health))
        .route("/ws", get(ws_handler))
        .route("/fee-estimate", get(get_fee_estimate))
        .route("/rewards/{address}", get(get_pending_rewards))
        .route("/faucet", post(faucet))
        .route("/leaderboard", get(get_leaderboard))
        .route("/stats", get(get_stats_page))
        .route_layer(middleware::from_fn_with_state(rpc_state.clone(), rate_limit_middleware));

    // ADMIN tier — rate-limited AND key-gated (auth_middleware).
    let admin = Router::new()
        .route("/metrics", get(get_metrics))
        .route("/metrics/prometheus", get(get_prometheus_metrics))
        .route("/storage/metrics", get(get_storage_metrics))
        .route("/traffic", get(get_traffic))
        .route("/network/quality", get(get_peer_quality))
        .route("/compliance", get(get_compliance))
        .route("/anti-scale", get(get_anti_scale))
        .route("/peers/full", get(get_peers_full))
        .route_layer(middleware::from_fn_with_state(rpc_state.clone(), auth_middleware))
        .route_layer(middleware::from_fn_with_state(rpc_state.clone(), rate_limit_middleware));

    public
        .merge(admin)
        .layer(middleware::from_fn_with_state(rpc_state.clone(), cors_middleware))
        .with_state(rpc_state)
}

/// W5.7 F-4: Deployment-safety gate. Returns `Err` when the RPC server would be
/// exposed insecurely.
///
/// A-batch item 5 (D3): under the public/keyed route split the admin diagnostics
/// tier lives in the SAME listener as the public tier, gated by the admin key.
/// This guard preserves the F-4 intent — never expose the KEYLESS admin tier on
/// a public interface — precisely: a non-loopback bind is permitted only when an
/// admin key is set (admin tier then requires the key; the public read tier is
/// intentionally open). With no key, only loopback is allowed (there the keyless
/// admin tier is reachable only from the local host). An unparseable bind string
/// is treated as non-loopback (fail-closed). The returned String is a
/// human-readable reason suitable for logging.
///
/// OPERATOR CAVEAT (D3): this guard only inspects the node's OWN bind address, not
/// a reverse proxy in front of it. Under the D3 topology (TLS proxy → loopback RPC),
/// binding loopback with NO admin key passes this guard, but a public proxy would
/// then expose the keyless admin tier to the internet. When fronting the node with
/// a proxy, an admin key MUST be set (or the admin routes withheld at the proxy).
/// The operator runbook has to state this — the code cannot detect the proxy.
fn rpc_bind_guard(rpc_bind: &str, api_key_is_set: bool) -> Result<(), String> {
    if api_key_is_set {
        return Ok(());
    }
    match rpc_bind.parse::<std::net::IpAddr>() {
        Ok(ip) if ip.is_loopback() => Ok(()),
        Ok(ip) => Err(format!(
            "refusing to start RPC server: bind address {} is not loopback and no API key is configured (auth disabled). Set an API key or bind to 127.0.0.1.",
            ip
        )),
        Err(_) => Err(format!(
            "refusing to start RPC server: bind address '{}' is not a valid IP and no API key is configured (auth disabled). Set an API key or bind to 127.0.0.1.",
            rpc_bind
        )),
    }
}

/// Start the RPC server on the given port and bind address.
pub async fn start_rpc_server(
    rpc_port: u16,
    rpc_bind: String,
    rpc_state: Arc<RpcState>,
) {
    // W5.7 F-4: refuse to start if bound to a non-loopback address with auth
    // disabled (no API key). This prevents an unauthenticated RPC surface from
    // being silently exposed to the network.
    if let Err(reason) = rpc_bind_guard(&rpc_bind, rpc_state.api_key.is_some()) {
        tracing::error!("{}", reason);
        return;
    }

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

    // A-batch item 1: inject ConnectInfo<SocketAddr> so the per-request source
    // address is available in `req.extensions()`. Without this the rate limiter
    // (rate_limit_middleware) and the auth loopback check see NO ConnectInfo and
    // every caller collapses into one shared "unknown" bucket. auth_middleware
    // is unaffected in the secure default (ALLOW_LOOPBACK_BYPASS=false): it never
    // consults ConnectInfo, and a present ConnectInfo does not weaken it.
    let make_service =
        app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    if let Err(e) = axum::serve(listener, make_service).await {
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

    /// W5.7 F-1: oversized Batch should be rejected by validate_shape
    /// before signature verify, returning 400 BAD_REQUEST.
    #[tokio::test]
    async fn submit_tx_with_oversized_batch_rejected() {
        let (state, _rx) = make_rpc_state();
        let app = build_router(state);

        let wallet = Wallet::generate();
        let inner = TxKind::Transfer {
            to: Address([1u8; 32]),
            amount: Amount::from_comme(1),
        };
        let ops: Vec<TxKind> = (0..(Transaction::MAX_BATCH_SIZE + 1))
            .map(|_| inner.clone()).collect();
        let mut tx = Transaction {
            from: *wallet.address(),
            nonce: 0,
            kind: TxKind::Batch { operations: ops },
            fee: 100_000,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        sign_transaction(&mut tx, &wallet);
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
        assert_eq!(result.error.as_deref(), Some("batch exceeds MAX_BATCH_SIZE"));
    }

    /// W5.7 F-1: a 200 KB junk body on /tx must be rejected by the
    /// DefaultBodyLimit layer BEFORE serde_json::Json deserialization,
    /// so the server doesn't burn CPU/RAM on a malformed payload.
    #[tokio::test]
    async fn submit_tx_body_bomb_rejected_by_layer() {
        let (state, _rx) = make_rpc_state();
        let app = build_router(state);

        let body = vec![b'x'; 200 * 1024];
        let req = Request::builder()
            .method("POST")
            .uri("/tx")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert!(
            resp.status().is_client_error(),
            "expected 4xx, got {}",
            resp.status()
        );
    }

    /// W5.7 F-5: regression test for /peers IP-leak.
    /// Pre-fix the public /peers route returned the full Vec<PeerInfo>
    /// including each peer's IP. After the fix the ip field must be None
    /// regardless of internal state.
    #[tokio::test]
    async fn get_peers_redacts_ip() {
        let (state, _rx) = make_rpc_state();

        // Populate internal state with a real IP so we can prove the route strips it.
        {
            let mut peers = state.peers.lock().await;
            peers.push(PeerInfo {
                peer_id: "12D3KooWTestPeerForRedaction".into(),
                ip: Some("203.0.113.42".into()),
                validator_address: Some("ab".repeat(32)),
                compliance_status: Some("Compliant".into()),
                last_seen: None,
            });
        }

        let app = build_router(state);
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/peers")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let returned: Vec<PeerInfo> = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(returned.len(), 1, "expected one peer in response");
        assert!(
            returned[0].ip.is_none(),
            "ip MUST be redacted on /peers; got {:?}",
            returned[0].ip
        );
        // Non-IP fields still pass through.
        assert_eq!(returned[0].peer_id, "12D3KooWTestPeerForRedaction");
        assert_eq!(returned[0].compliance_status.as_deref(), Some("Compliant"));
    }

    /// W5.7 F-2: regression test for unbounded rate_limits HashMap.
    /// Pre-fix every unique source IP lived forever and the limiter was
    /// itself a DoS vector. After the fix the map must stay at-or-below
    /// MAX_RATE_LIMIT_ENTRIES even after a flood of distinct IPs.
    #[tokio::test]
    async fn rate_limit_map_is_bounded() {
        let (state, _rx) = make_rpc_state();
        let app = build_router(state.clone());

        // Drive the limiter map directly past the cap with stale timestamps.
        {
            let mut limits = state.rate_limits.lock().await;
            let now = std::time::Instant::now();
            for n in 0..(MAX_RATE_LIMIT_ENTRIES + 10_000) {
                let ip = format!("10.{}.{}.{}", (n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff);
                limits.insert(ip, (1, now - std::time::Duration::from_secs(60)));
            }
        }

        // Trigger one live request so the eviction sweep fires.
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/status")
            .body(axum::body::Body::empty())
            .unwrap();
        let _ = app.oneshot(req).await.unwrap();

        let limits = state.rate_limits.lock().await;
        assert!(
            limits.len() <= MAX_RATE_LIMIT_ENTRIES,
            "expected rate_limits.len() <= {}, got {}",
            MAX_RATE_LIMIT_ENTRIES,
            limits.len()
        );
    }

    #[test]
    fn rpc_bind_guard_refuses_non_loopback_without_api_key() {
        // W5.7 F-4: non-loopback bind + no API key => refuse to start.
        assert!(rpc_bind_guard("0.0.0.0", false).is_err());
        assert!(rpc_bind_guard("192.168.1.10", false).is_err());
        assert!(rpc_bind_guard("::", false).is_err());
        // Unparseable bind string is treated as non-loopback (fail-closed).
        assert!(rpc_bind_guard("not-an-ip", false).is_err());

        // Loopback binds are always allowed, even with auth disabled.
        assert!(rpc_bind_guard("127.0.0.1", false).is_ok());
        assert!(rpc_bind_guard("::1", false).is_ok());

        // With an API key set, any bind address is permitted.
        assert!(rpc_bind_guard("0.0.0.0", true).is_ok());
        assert!(rpc_bind_guard("192.168.1.10", true).is_ok());
        assert!(rpc_bind_guard("127.0.0.1", true).is_ok());

        // Confirm the refusal reason mentions the offending address.
        let err = rpc_bind_guard("0.0.0.0", false).unwrap_err();
        assert!(err.contains("0.0.0.0"));
        assert!(err.to_lowercase().contains("refusing"));
    }

    #[tokio::test]
    async fn faucet_does_not_lie_when_unprovisioned() {
        // W5.7 F-6: an unprovisioned faucet must NOT return a false success.
        // make_rpc_state() builds an is_testnet=true state with no faucet
        // wallet, so the request passes the testnet gate and hits the
        // honesty path. It must return 503 and must NOT claim dispensal,
        // and must NOT have queued any transfer on tx_sender.
        let (state, mut rx) = make_rpc_state();
        let app = build_router(state);

        let addr_hex = hex::encode([7u8; 32]);
        let body = serde_json::to_vec(&serde_json::json!({ "address": addr_hex })).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/faucet")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        // Must not claim success, and must surface the provisioning error.
        assert!(v.get("success").is_none(), "unprovisioned faucet must not report success");
        assert_eq!(v["error"], "faucet not provisioned");

        // No transfer should have been queued to the event loop.
        assert!(rx.try_recv().is_err(), "faucet must not queue a tx when unprovisioned");
    }

    // ── A4-auth-loopback regression tests ──
    //
    // The bug: when an api_key was set, loopback callers (and callers with no
    // determinable IP) skipped the X-API-Key check. These tests pin the new
    // policy: a configured key is required for loopback callers too, while the
    // no-key default remains a pure pass-through.
    //
    // A-batch item 5 (D3): these probe an ADMIN route (/metrics) rather than
    // /status. Under the public/keyed split /status is a PUBLIC route (no auth),
    // so the auth middleware is now only reachable via an admin route. The
    // security invariants asserted here (loopback bypass closed, fail-closed on
    // no ConnectInfo, key required) are unchanged — only the probe route moved.

    use std::net::SocketAddr;

    /// Helper: a request to GET /metrics (an ADMIN, key-gated route) carrying an
    /// explicit loopback ConnectInfo in its extensions (what the real TCP accept
    /// path injects). `key`: Some(..) sets the X-API-Key header; None omits it.
    fn loopback_admin_request(key: Option<&str>) -> Request<Body> {
        let loopback: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        let mut builder = Request::builder().method("GET").uri("/metrics");
        if let Some(k) = key {
            builder = builder.header("X-API-Key", k);
        }
        let mut req = builder.body(Body::empty()).unwrap();
        // Inject the source address exactly as into_make_service_with_connect_info
        // would at runtime, so auth_middleware's is_loopback() path is exercised.
        req.extensions_mut().insert(ConnectInfo(loopback));
        req
    }

    /// With a key configured, a LOOPBACK request that omits X-API-Key MUST be
    /// rejected (401). This is the bypass the patch closes.
    #[tokio::test]
    async fn loopback_without_key_is_rejected_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        // Configure an API key. State Arc is unique here (refcount 1), so
        // get_mut succeeds before build_router clones it into the router.
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let resp = app
            .oneshot(loopback_admin_request(None))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "loopback caller with no X-API-Key MUST be rejected when a key is configured \
             (this is the A4 loopback bypass)"
        );
    }

    /// With a key configured, a LOOPBACK request that presents the WRONG key
    /// MUST be rejected (401). Guards against any "present-but-unchecked" path.
    #[tokio::test]
    async fn loopback_with_wrong_key_is_rejected_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let resp = app
            .oneshot(loopback_admin_request(Some("wrong-key")))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "loopback caller with a wrong key MUST be rejected when a key is configured"
        );
    }

    /// With a key configured, a LOOPBACK request that presents the CORRECT key
    /// MUST be accepted (200) and reach the handler.
    #[tokio::test]
    async fn loopback_with_correct_key_is_accepted_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let resp = app
            .oneshot(loopback_admin_request(Some("s3cret-key")))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "loopback caller presenting the correct key MUST be accepted"
        );
    }

    /// Default path (no key configured): the middleware is a pass-through.
    /// A loopback request with NO X-API-Key MUST still be accepted (200).
    /// This pins that the patch does not regress the unauthenticated default.
    #[tokio::test]
    async fn loopback_passes_through_when_no_key_set() {
        // make_rpc_state() defaults api_key = None.
        let (state, _rx) = make_rpc_state();
        assert!(state.api_key.is_none(), "helper default must be no-key");
        let app = build_router(state);

        let resp = app
            .oneshot(loopback_admin_request(None))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "with no key configured, loopback requests pass through unchanged"
        );
    }

    /// Runtime-path guard: with a key configured and NO ConnectInfo present at
    /// all (the actual condition under the current `axum::serve` wiring, which
    /// does not inject ConnectInfo), the request MUST still be rejected (401).
    /// Pins the fail-closed "no determinable IP" behavior.
    #[tokio::test]
    async fn no_connect_info_is_rejected_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        // No ConnectInfo inserted — mirrors the live runtime path. Probes the
        // admin route (/metrics) since the auth gate now lives there (D3 split).
        let req = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "request with no determinable source IP MUST be rejected when a key is configured"
        );
    }

    /// Cross-check: a non-loopback caller with no key is ALSO rejected when a
    /// key is configured. This isn't new behavior (it worked before), but it
    /// pins that the refactor didn't accidentally narrow enforcement to the
    /// loopback branch only.
    #[tokio::test]
    async fn remote_without_key_is_rejected_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let remote: SocketAddr = "203.0.113.7:40000".parse().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(remote));

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "non-loopback caller with no key MUST be rejected when a key is configured"
        );
    }

    // ── A-batch item 5 (D3 public/keyed split) tests ──

    /// PUBLIC route: /status must be reachable with NO key even when an admin key
    /// IS configured. This is the whole point of the split — the public read tier
    /// stays open behind the D3 proxy while admin diagnostics require the key.
    #[tokio::test]
    async fn d3_public_status_open_without_key_when_key_set() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "public /status MUST stay open without a key even when an admin key is set"
        );
    }

    /// ADMIN route: /metrics must be rejected (401) without the key when a key is
    /// configured.
    #[tokio::test]
    async fn d3_admin_metrics_requires_key() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "admin /metrics MUST require the key when one is configured"
        );
    }

    /// ADMIN route: /metrics returns 200 with the correct key.
    #[tokio::test]
    async fn d3_admin_metrics_accepts_correct_key() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/metrics")
            .header("X-API-Key", "s3cret-key")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "admin /metrics MUST be reachable with the correct key"
        );
    }

    /// ADMIN /peers/full is key-gated AND returns the UN-redacted IP (the public
    /// /peers strips it). Pins that the split did not accidentally expose the
    /// admin route, and that it truly serves the sensitive field once authorized.
    #[tokio::test]
    async fn d3_peers_full_admin_gated_and_returns_ip() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        {
            let mut peers = state.peers.lock().await;
            peers.push(PeerInfo {
                peer_id: "12D3KooWFullPeer".into(),
                ip: Some("198.51.100.7".into()),
                validator_address: Some("cd".repeat(32)),
                compliance_status: Some("Compliant".into()),
                last_seen: None,
            });
        }
        let app = build_router(state);

        // Without a key: 401.
        let unauth = Request::builder()
            .method("GET")
            .uri("/peers/full")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(unauth).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "/peers/full MUST be key-gated"
        );

        // With the key: 200 and the IP is present (un-redacted).
        let authed = Request::builder()
            .method("GET")
            .uri("/peers/full")
            .header("X-API-Key", "s3cret-key")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(authed).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let returned: Vec<PeerInfo> = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(returned.len(), 1);
        assert_eq!(
            returned[0].ip.as_deref(),
            Some("198.51.100.7"),
            "/peers/full MUST return the un-redacted IP once authorized"
        );
    }

    // ── A-batch item 2 (proxy-aware client IP) tests ──

    /// A trusted proxy (loopback socket peer) → the rightmost non-trusted
    /// X-Forwarded-For entry is used as the rate-limit identity.
    #[tokio::test]
    async fn rate_limit_client_ip_trusts_xff_from_loopback() {
        let loopback: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri("/status")
            // client, then a chained trusted (loopback) hop on the right; the
            // rightmost NON-trusted entry (the real client) must win.
            .header("x-forwarded-for", "203.0.113.9, 127.0.0.1")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(loopback));
        assert_eq!(rate_limit_client_ip(&req), "203.0.113.9");
    }

    /// CF-Connecting-IP is honored from a trusted proxy when no usable XFF entry
    /// exists.
    #[tokio::test]
    async fn rate_limit_client_ip_uses_cf_header_from_loopback() {
        let loopback: SocketAddr = "[::1]:9999".parse().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri("/status")
            .header("cf-connecting-ip", "203.0.113.55")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(loopback));
        assert_eq!(rate_limit_client_ip(&req), "203.0.113.55");
    }

    /// A NON-trusted (remote) socket peer → forwarding headers are IGNORED and
    /// the socket IP is used. A remote client cannot spoof its rate-limit bucket.
    #[tokio::test]
    async fn rate_limit_client_ip_ignores_xff_from_remote() {
        let remote: SocketAddr = "203.0.113.7:40000".parse().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri("/status")
            .header("x-forwarded-for", "10.0.0.1")
            .header("cf-connecting-ip", "10.0.0.2")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(remote));
        assert_eq!(
            rate_limit_client_ip(&req),
            "203.0.113.7",
            "forwarding headers from a non-trusted peer MUST be ignored"
        );
    }

    /// No ConnectInfo → single shared "unknown" bucket (fail-closed to shared).
    #[tokio::test]
    async fn rate_limit_client_ip_unknown_without_connect_info() {
        let req = Request::builder()
            .method("GET")
            .uri("/status")
            .header("x-forwarded-for", "10.0.0.1")
            .body(Body::empty())
            .unwrap();
        assert_eq!(rate_limit_client_ip(&req), "unknown");
    }

    // ── A-batch item 4 (CORS allowlist + preflight) tests ──

    #[test]
    fn cors_allow_origin_wildcard_and_allowlist() {
        // Wildcard: always "*", regardless of request Origin.
        assert_eq!(cors_allow_origin("*", None).as_deref(), Some("*"));
        assert_eq!(cors_allow_origin("*", Some("https://x.example")).as_deref(), Some("*"));

        // Multi-origin allowlist: echo only an exact match; fail-closed otherwise.
        let allow = "https://a.example, https://b.example";
        assert_eq!(cors_allow_origin(allow, Some("https://a.example")).as_deref(), Some("https://a.example"));
        assert_eq!(cors_allow_origin(allow, Some("https://b.example")).as_deref(), Some("https://b.example"));
        assert_eq!(cors_allow_origin(allow, Some("https://evil.example")), None);
        // No Origin header under a non-wildcard allowlist → no ACAO.
        assert_eq!(cors_allow_origin(allow, None), None);
    }

    /// OPTIONS preflight is answered with 204 and CORS headers, WITHOUT a key,
    /// even when an admin key is configured (browsers preflight w/o credentials).
    #[tokio::test]
    async fn cors_preflight_returns_204_with_headers() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        // Default cors_origins is "*".
        let app = build_router(state);

        let req = Request::builder()
            .method("OPTIONS")
            .uri("/metrics") // even an admin path preflights without the key
            .header("origin", "https://app.example")
            .header("access-control-request-method", "GET")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap()),
            Some("*"),
            "wildcard testnet default must echo * on preflight"
        );
        assert!(resp.headers().get("access-control-allow-methods").is_some());
    }

    /// Non-wildcard allowlist: an allowed Origin is echoed; a disallowed Origin
    /// gets NO Access-Control-Allow-Origin header (fail-closed).
    #[tokio::test]
    async fn cors_allowlist_echoes_allowed_and_omits_disallowed() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .cors_origins = "https://allowed.example".to_string();
        let app = build_router(state);

        // Allowed origin → echoed exactly (never comma-joined).
        let allowed = Request::builder()
            .method("GET")
            .uri("/status")
            .header("origin", "https://allowed.example")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(allowed).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap()),
            Some("https://allowed.example"),
        );

        // Disallowed origin → no ACAO header at all.
        let disallowed = Request::builder()
            .method("GET")
            .uri("/status")
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(disallowed).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get("access-control-allow-origin").is_none(),
            "a disallowed origin MUST NOT receive an Access-Control-Allow-Origin header"
        );
    }

    // ── A-batch item 6 (faucet money-path builder) test ──

    /// The inert faucet substrate builds a valid, signed 1-COMME Transfer with the
    /// requested nonce, MINIMUM_FEE, and correct recipient. Verifies the exact
    /// building block the D6 dispenser will queue once a faucet wallet is wired.
    #[test]
    fn build_faucet_transfer_makes_valid_signed_1_comme() {
        let faucet = Wallet::generate();
        let recipient = Address([9u8; 32]);
        let tx = build_faucet_transfer(&faucet, recipient, 7);

        assert!(tx.verify(), "faucet tx must carry a valid signature");
        assert_eq!(tx.from, *faucet.address());
        assert_eq!(tx.nonce, 7);
        assert!(
            tx.fee >= commputer_core::transaction::MINIMUM_FEE,
            "Transfer is not fee-exempt; fee must cover MINIMUM_FEE"
        );
        match tx.kind {
            TxKind::Transfer { to, amount } => {
                assert_eq!(to, recipient);
                assert_eq!(
                    amount.raw(),
                    commputer_core::token::UNITS_PER_COMME,
                    "faucet dispenses exactly 1 COMME"
                );
            }
            other => panic!("expected Transfer, got {:?}", other),
        }
    }

    // ── Security: RPC deadlock safety + WS connection cap ──

    /// Reproduces the lock-order-inversion deadlock between two PUBLIC handlers.
    ///
    /// The test holds `status`, then queues `get_stats_page` (acquires status
    /// first) and `get_network_info` behind it. Pre-fix, `get_network_info`
    /// grabbed `balances` BEFORE `status`, so after `status` is released it held
    /// `balances` while waiting for `status` (held by stats), and stats waited for
    /// `balances` — a permanent cycle. With the canonical order (status before
    /// balances) both handlers queue on `status` holding nothing and run to
    /// completion. A deadlock manifests as the timeout firing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rpc_handlers_have_consistent_lock_order_no_deadlock() {
        let (state, _rx) = make_rpc_state();

        // Hold `status` so both handlers must queue on it (tokio Mutex is FIFO).
        let status_guard = state.status.lock().await;

        // Queue get_stats_page first (canonical: status → metrics → balances).
        let sb = state.clone();
        let b = tokio::spawn(async move {
            let _ = get_stats_page(State(sb)).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Then get_network_info. Pre-fix it grabs `balances` first and parks on
        // `status` holding it → the cycle. Post-fix it parks on `status` holding
        // nothing.
        let sa = state.clone();
        let a = tokio::spawn(async move {
            let _ = get_network_info(State(sa)).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Release status; a single-order implementation runs both to completion.
        drop(status_guard);

        let joined = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let _ = a.await;
            let _ = b.await;
        })
        .await;
        assert!(
            joined.is_ok(),
            "RPC handlers deadlocked due to inconsistent lock order"
        );
    }

    /// The public `/ws` route caps concurrent connections: reservations succeed up
    /// to MAX_WS_CONNECTIONS, the next is rejected, and releasing a slot frees one.
    #[test]
    fn ws_connection_slots_are_capped() {
        // No live connections in the test process.
        WS_CONNECTIONS.store(0, Ordering::SeqCst);

        // Reserve exactly the cap.
        let mut guards = Vec::new();
        for _ in 0..MAX_WS_CONNECTIONS {
            let g = try_reserve_ws_slot();
            assert!(g.is_some(), "reservation within the cap must succeed");
            guards.push(g.unwrap());
        }
        // One more must be rejected, and the counter must not leak past the cap.
        assert!(
            try_reserve_ws_slot().is_none(),
            "reservation over the cap must be rejected"
        );
        assert_eq!(WS_CONNECTIONS.load(Ordering::SeqCst), MAX_WS_CONNECTIONS);

        // Free one slot — a new reservation now succeeds.
        guards.pop();
        let g = try_reserve_ws_slot();
        assert!(
            g.is_some(),
            "after releasing a slot a new reservation must succeed"
        );
        assert_eq!(WS_CONNECTIONS.load(Ordering::SeqCst), MAX_WS_CONNECTIONS);

        // Clean up so the global counter is left at zero.
        drop(g);
        guards.clear();
        assert_eq!(WS_CONNECTIONS.load(Ordering::SeqCst), 0);
    }
}
