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
/// A transaction handed to the event loop, optionally carrying a reply channel
/// for the caller to receive the REAL mempool-admission verdict.
///
/// `POST /tx` sets `reply` and awaits it, so a submitter learns whether the tx
/// was actually ACCEPTED (passed `validate_tx_for_mempool`) or REJECTED and
/// why — instead of the old contract, which answered `accepted: true` the
/// instant the tx was queued and dropped every real rejection to a log line the
/// submitter never sees. That gap is the structural root of this project's
/// silent-loss bugs; closing it is what lets the CLI tell users the truth.
///
/// Internal issuers (the faucet, `/submit_job`) leave `reply` None and keep
/// fire-and-forget semantics — they manage their own outcome separately.
pub struct RpcTxRequest {
    pub tx: Transaction,
    pub reply: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

// NOTE: there was a `RpcTxRequest::fire(tx)` constructor here — a fire-and-forget
// submission with `reply: None`. It is deliberately GONE.
//
// Every endpoint that reports an outcome to a caller must use
// `submit_awaiting_verdict`. `fire` existed so an issuer could skip the wait,
// and both endpoints that used it (/faucet and /submit_job) ended up telling
// callers a transaction had succeeded when only the QUEUEING had succeeded —
// the silent-loss class that has cost this project more debugging than any
// other single bug. /tx was fixed first and the other two were simply forgotten,
// because the convenient wrong thing was still one call away.
//
// `reply` stays an `Option` because the event loop tolerates a submitter that
// went away; but nothing in-tree constructs one without a reply channel, and
// anything that wants to should have to write it out and justify itself.

/// How long an endpoint waits for the event loop's admission verdict.
///
/// Generous on purpose: the loop drains the channel every iteration so a verdict
/// normally lands in milliseconds, but one select! arm body runs to completion
/// first, and a heavy body (a sync batch, an orphan cascade) can hold the loop
/// for seconds during catch-up. Timing out early would misreport an admitted tx
/// as failed — the exact dishonesty this machinery exists to remove.
const VERDICT_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// The node's real answer to "was this transaction admitted?".
#[derive(Debug)]
pub(crate) enum TxVerdict {
    /// Passed the mempool gate.
    Admitted,
    /// The gate rejected it, with the reason that used to vanish into a log line.
    Rejected(String),
    /// Never queued — the channel was full.
    QueueFull,
    /// Never queued, or the loop dropped the reply: the node is shutting down.
    NodeStopping,
    /// Queued, but no answer within `VERDICT_WAIT`. **NOT a rejection** — the tx
    /// is sitting in the channel and will most likely be admitted. Callers must
    /// not report this as failure, and must not undo side effects on it.
    Unconfirmed,
}

/// Submit a transaction and wait for the mempool gate's REAL verdict.
///
/// The single place this is decided. It used to be open-coded per endpoint,
/// which is precisely how `/tx` came to tell the truth while `/faucet` and
/// `/submit_job` kept answering on a successful queue.
pub(crate) async fn submit_awaiting_verdict(state: &RpcState, tx: Transaction) -> TxVerdict {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if let Err(e) = state.tx_sender.try_send(RpcTxRequest { tx, reply: Some(reply_tx) }) {
        return match e {
            mpsc::error::TrySendError::Full(_) => TxVerdict::QueueFull,
            mpsc::error::TrySendError::Closed(_) => TxVerdict::NodeStopping,
        };
    }
    match tokio::time::timeout(VERDICT_WAIT, reply_rx).await {
        Ok(Ok(Ok(()))) => TxVerdict::Admitted,
        Ok(Ok(Err(reason))) => TxVerdict::Rejected(reason),
        Ok(Err(_recv_err)) => TxVerdict::NodeStopping,
        Err(_timeout) => TxVerdict::Unconfirmed,
    }
}

/// Shared state for the RPC server.
pub struct RpcState {
    /// Channel to send submitted transactions to the event loop, each with an
    /// optional reply channel for the real admission verdict.
    pub tx_sender: mpsc::Sender<RpcTxRequest>,
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
    /// Feature 142: Compliance dashboard stats. Still initialized in protected
    /// main.rs; readers were removed by the Phase 0 honesty change (get_compliance
    /// now returns a constant honest payload) — removing this field is a Tier 3 edit.
    #[allow(dead_code)]
    pub compliance_stats: Mutex<ComplianceDashboard>,
    /// Feature 150: Anti-scale metrics. Still initialized in protected main.rs;
    /// readers were removed by the Phase 0 honesty change (get_anti_scale now
    /// returns a constant honest payload) — removing this field is a Tier 3 edit.
    #[allow(dead_code)]
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
    /// Alpha reset (D6): the provisioned faucet signing wallet, or `None` when the
    /// faucet is disabled on this node. Threaded from `provision_faucet_from_env`
    /// in main.rs; `None` until `COMMPUTER_FAUCET_SEED` is set on the one
    /// provisioner node at the reset.
    pub faucet_wallet: Option<commputer_core::wallet::Wallet>,
    /// Alpha reset (D6/E3): next nonce for the faucet dispenser. The dispense path
    /// holds this lock across the claim check + build + send so concurrent claims
    /// from one IP cannot bypass the per-epoch limit or desync the nonce.
    pub faucet_next_nonce: Mutex<u64>,
    /// Track-2 (Phase B): shared DA blob store — the `/submit_job` publisher persists
    /// coded chunks here (the inbound serve path reads the same store). `None` = DA off.
    pub da_store: Option<std::sync::Arc<commputer::da_store::DaStore>>,
    /// Track-2 (Phase B): DA backend command sender (into the event loop's da_command
    /// drain arm) — `/submit_job` uses it to Advertise the published chunks. `None` = DA off.
    /// `std::sync::mpsc::Sender<T>` is `Sync` on modern rustc, so no Mutex is needed.
    pub da_command_tx: Option<std::sync::mpsc::Sender<commputer_pouw_onchain::da_transport::DaCommand>>,
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

    // Send WITH a reply channel and await the event loop's real verdict, so
    // `accepted` means the tx passed the mempool gate — not merely that it was
    // queued. Shape-identical response; only its truthfulness changes.
    //
    // The wait itself lives in `submit_awaiting_verdict`, shared with /faucet and
    // /submit_job. It was open-coded here first, and keeping it that way is
    // exactly how those two endpoints were left behind still answering on a
    // successful queue.
    match submit_awaiting_verdict(&state, tx).await {
        TxVerdict::Admitted => (
            StatusCode::OK,
            Json(SubmitTxResponse { accepted: true, tx_hash, error: None }),
        ),
        TxVerdict::Rejected(reason) => (
            // The mempool gate rejected it. This is the verdict that used to be
            // silently dropped to a log line.
            StatusCode::BAD_REQUEST,
            Json(SubmitTxResponse { accepted: false, tx_hash, error: Some(reason) }),
        ),
        TxVerdict::QueueFull => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash,
                error: Some("Transaction queue full, try again later".into()),
            }),
        ),
        TxVerdict::NodeStopping => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash,
                error: Some("Node stopped before confirming the transaction".into()),
            }),
        ),
        TxVerdict::Unconfirmed => (
            // 202 Accepted: queued for processing, outcome not yet known. NOT a
            // rejection — the tx is in the channel and will very likely be
            // admitted; the client can resubmit (idempotent) or poll /balance.
            StatusCode::ACCEPTED,
            Json(SubmitTxResponse {
                accepted: false,
                tx_hash,
                error: Some(
                    "Queued but not yet confirmed (node busy); it may still be admitted — \
                     resubmit or check /balance"
                        .into(),
                ),
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
    /// §10 honesty: false until nerf enforcement is wired to a reward path.
    pub enforcement_active: bool,
    /// Explains what the numbers below do and do not measure.
    pub note: String,
    pub total_validators: u64,
    pub compliant_count: u64,
    pub nerfed_count: u64,
    pub current_nerf_percentage: u32,
    pub suspicious_count: u64,
}

/// Feature 150: Anti-scale metrics response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AntiScaleDashboard {
    /// §10 honesty: false until nerf enforcement is wired to a reward path.
    pub enforcement_active: bool,
    /// Explains what the numbers below do and do not measure.
    pub note: String,
    pub total_warehouse_detections: u64,
    pub total_nerfed_rewards: u64,
    pub nerf_percentage_history: Vec<(u64, u32)>,
    pub largest_detected_clusters: Vec<(usize, String)>,
}

/// §10 honesty (North Star Phase 0): nothing writes compliance stats yet, so
/// serving Default zeros reads as "checked and clean". Until enforcement is
/// wired, these endpoints say so explicitly. The 80 is NerfRate::INITIAL
/// (8000 bps) — the protocol floor constant, currently applied to no one.
pub fn honest_compliance_dashboard() -> ComplianceDashboard {
    ComplianceDashboard {
        enforcement_active: false,
        note: "Nerf enforcement is not yet wired to any reward path. Counts are \
               structural zeros (nothing is checked yet), not measurements. \
               current_nerf_percentage is the protocol floor constant, applied to no one."
            .to_string(),
        total_validators: 0,
        compliant_count: 0,
        nerfed_count: 0,
        current_nerf_percentage: 80,
        suspicious_count: 0,
    }
}

pub fn honest_anti_scale_dashboard() -> AntiScaleDashboard {
    AntiScaleDashboard {
        enforcement_active: false,
        note: "Warehouse detection is not yet wired to any reward path. Totals are \
               structural zeros, not measurements."
            .to_string(),
        total_warehouse_detections: 0,
        total_nerfed_rewards: 0,
        nerf_percentage_history: Vec::new(),
        largest_detected_clusters: Vec::new(),
    }
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

/// GET /compliance — Feature 142. Serves the honest constant payload: the
/// RpcState.compliance_stats mutex has no writer anywhere, so reading it
/// would serve Default zeros dressed up as measurements.
async fn get_compliance(State(_state): State<Arc<RpcState>>) -> Json<ComplianceDashboard> {
    Json(honest_compliance_dashboard())
}

/// GET /storage/metrics — Feature 188: storage metrics.
async fn get_storage_metrics(
    State(state): State<Arc<RpcState>>,
) -> Json<commputer_storage::StorageMetrics> {
    let metrics = state.storage_metrics.lock().await.clone();
    Json(metrics)
}

/// GET /anti-scale — Feature 150. Same honesty rule as /compliance.
async fn get_anti_scale(State(_state): State<Arc<RpcState>>) -> Json<AntiScaleDashboard> {
    Json(honest_anti_scale_dashboard())
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

/// Build the honest "why is this address not counted" response for
/// `get_pending_rewards`'s early-return path (QC-006 rev 3b, I-1 residual).
///
/// The two regimes must NOT share one sentence. While the pin is active, a
/// non-pinned address is a VERIFIED negative — the allowlist is the trust
/// anchor and this snapshot has the whole list, so "earns nothing" is a fact
/// this handler can back. Once the pin retires, `eligible()` returning
/// `false` means only that `bonded = 0` (the hardcoded stand-in, see the
/// comment above `eligible`) failed the real floor — NOT that the address's
/// actual bonded stake failed it. This handler has no per-address bond
/// figure to check (`BalanceInfo` does not carry one), so asserting "earns
/// nothing" here would be a specific factual claim about a number it never
/// looked at. Split into its own function so the two sentences — and the
/// choice between them — are one thing this file can unit-test directly,
/// independent of which regime happens to be compiled in.
///
/// RESIDUAL, not closed by this function: the real fix is to plumb a real
/// per-address bonded-stake figure into `BalanceInfo` (and therefore into
/// `event_loop.rs`'s balance sweep), so `eligible()` can pass a genuine
/// `bonded` instead of the hardcoded `0`. That is out of scope for this lane
/// (it touches a protected file) and is tracked as a set-opening
/// precondition against QC-006 in the QC ledger. Until it lands, the
/// pin-retired branch below is this handler's honest ceiling: "unknown," not
/// "no."
fn ineligibility_response(address: &str, epoch: u64, pin_active: bool) -> serde_json::Value {
    if pin_active {
        serde_json::json!({
            "address": address,
            "estimated_reward": 0,
            "composite_score": 0,
            "epoch": epoch,
            "note": "registered, but not in the consensus set that can actually \
                     produce blocks — this address is never selected to produce \
                     and earns nothing",
            "eligibility_basis": "allowlist",
        })
    } else {
        serde_json::json!({
            "address": address,
            "estimated_reward": 0,
            "composite_score": 0,
            "epoch": epoch,
            "note": "registered, but this handler cannot verify this address's \
                     bonded stake from the balance snapshot it holds — \
                     eligibility is UNKNOWN, not confirmed ineligible, and the \
                     zero reported below is pending verification, not a claim \
                     about what this address will actually earn.",
            "eligibility_basis": "bonded-stake",
        })
    }
}

/// GET /rewards/{address} — an ESTIMATE of future mining income, not an
/// accrued or owed balance. See `estimated_reward_basis` and
/// `eligibility_basis` in the response for what the number assumes.
async fn get_pending_rewards(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Canonical lock order (see RpcState): status → balances → chain_health.
    let status = state.status.lock().await;
    let balances = state.balances.lock().await;
    let chain_health = state.chain_health.lock().await;

    let pin_active = crate::testnet_genesis::pin_is_active();

    // Registered is NOT the same as able to earn. `is_validator` is set by any
    // ValidatorRegister tx, and registration is free and automatic at every
    // boot. Who can actually earn is a CONSENSUS question with two regimes —
    // pinned allowlist vs. bonded stake — and `consensus_set` is the one
    // place that derivation is allowed to live (its module doc: "one
    // derivation, used everywhere"). A hand-rolled version of this rule
    // previously lived here and, in the allowlist-retired regime, returned
    // `true` for every registrant — exactly the inversion `consensus_set.rs`
    // exists to forbid (see its test
    // `the_open_regime_is_not_disabled_by_the_empty_allowlist`). Delegating
    // closes that gap (QC-006 rev 3, I-1).
    //
    // This snapshot (`BalanceInfo`) carries no bonded-stake figure, so the
    // real per-address bond is unknown here and 0 is passed. That is not a
    // guess dressed up as data — it is deliberately conservative:
    //   * while the pin is active, `is_consensus_eligible` never consults
    //     `bonded` at all, so passing 0 changes nothing;
    //   * once the pin retires, 0 against the real `MIN_CONSENSUS_BOND`
    //     (> 0 outside the test harness) FAILS CLOSED — nobody is reported
    //     eligible rather than everybody, the safe direction to be wrong in;
    //   * under `--features formation-test`, `MIN_CONSENSUS_BOND` is itself 0
    //     by design (consensus_set.rs), so `0 >= 0` correctly restores
    //     is_validator-only semantics for that harness, which is exactly the
    //     regime it is meant to exercise.
    let eligible = |addr_hex: &str, is_validator: bool| -> bool {
        let pinned = commputer_core::identity::Address::from_hex(addr_hex)
            .map(|a| crate::testnet_genesis::is_pinned_validator(&a))
            .unwrap_or(false);
        commputer::consensus_set::is_consensus_eligible(
            is_validator,
            0,
            commputer::consensus_set::MIN_CONSENSUS_BOND,
            pin_active,
            pinned,
        )
    };

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
        if !eligible(&address, true) {
            return (
                StatusCode::OK,
                Json(ineligibility_response(&address, status.epoch, pin_active)),
            );
        }
        let validator_count = balances
            .iter()
            .filter(|(addr, b)| eligible(addr, b.is_validator))
            .count()
            .max(1);

        // QC-006: this read `100 COMME/day` from a literal that was never the
        // emission schedule — wrong by ~6,850x at era 0, on a public key-free
        // route.
        //
        // Derived from two MEASURED inputs rather than one literal replacing
        // another. `block_reward` is the chain's own single source of truth for
        // issuance. Blocks-per-day comes from the health monitor's observed
        // `avg_block_time`, NOT from the 2s target: 2s is a floor
        // (`min_block_interval_secs`) and consensus rounds only add to it, so
        // any constant derived from the target overstates issuance — the repo's
        // own 2026-07-30 measurement put real blocks nearer 2.4s, i.e. ~20% fewer
        // per day than the 43,200 a 2s assumption gives.
        // (`EmissionSchedule::per_validator_daily_rate` hardcodes that 43,200 and
        // is therefore off by the same margin wherever it is used — see the QC
        // ledger. Not fixed here; that helper also feeds the startup banner and
        // deserves its own change.)
        //
        // Before the health monitor has two samples it has nothing to average
        // and returns a 2.0s floor (chain_health_monitor.rs) — indistinguishable
        // from a real 2.0s measurement unless the response says which one it
        // is. `block_time_source` is that provenance flag, so a fresh-boot
        // answer can be told apart from a steady-state one instead of quietly
        // overstating by up to ~4.3x in the first minutes (QC-006 rev 3, I-4).
        let (avg_block_time, block_time_source): (f64, &str) = match chain_health
            .get("avg_block_time")
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite() && *v > 0.0)
        {
            Some(v) => (v, "measured"),
            None => (2.0, "default_no_samples"),
        };
        let blocks_per_day = (86_400.0 / avg_block_time) as u64;
        let estimated_reward = commputer_core::token::block_reward(status.height)
            .saturating_mul(blocks_per_day)
            / validator_count as u64;

        // The basis must say WHICH regime produced `validator_count`, not
        // assert "the active consensus set" unconditionally — a fixed string
        // here was a promise the response didn't keep the moment the
        // allowlist retires (QC-006 rev 3, I-2).
        let (eligibility_basis, estimated_reward_basis): (&str, &str) = if pin_active {
            (
                "allowlist",
                "equal split of observed-rate emission among the alpha-allowlisted \
                 consensus set; the chain actually pays 100% to each block's \
                 producer and selects producers by stake weight, so this is an \
                 equal-stake approximation",
            )
        } else {
            (
                "bonded-stake",
                "equal split of observed-rate emission among validators this \
                 snapshot could verify against the bonded-stake floor; per-address \
                 bond is not carried here, so eligibility is evaluated as \
                 zero-bonded and therefore fails closed against the real floor \
                 outside the formation-test harness (whose floor is deliberately \
                 zero) — the chain actually pays 100% to each block's producer \
                 and selects producers by stake weight",
            )
        };

        (StatusCode::OK, Json(serde_json::json!({
            "address": address,
            "estimated_reward": estimated_reward,
            // Say what the number is NOT, too. The chain pays 100% of the block
            // reward to the producer and selects producers by STAKE WEIGHT, so an
            // equal split is only correct while bonds are equal — true of the
            // three founder nodes today and false the moment stakes diverge.
            "estimated_reward_basis": estimated_reward_basis,
            "eligibility_basis": eligibility_basis,
            "block_reward": commputer_core::token::block_reward(status.height),
            "avg_block_time_secs": avg_block_time,
            "block_time_source": block_time_source,
            "blocks_per_day": blocks_per_day,
            "eligible_validators": validator_count,
            "height": status.height,
            // STILL A PLACEHOLDER, deliberately left as one — but note the
            // precise reason, because an earlier draft of this comment claimed
            // composite scoring does not exist and that is FALSE.
            // `EpochProofSummary::composite_score` (core/src/proof.rs) is real,
            // is called from consensus/src/anchor.rs, and is summed into every
            // epoch summary in the event loop. What is missing is only the
            // plumbing: RpcState carries proof_leaderboard and
            // validator_performance, not per-address epoch summaries. So this
            // is wired-up work, not invention — which is exactly why the wrong
            // comment was worth correcting rather than deleting.
            "composite_score": 100,
            "epoch": status.epoch,
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
/// using `nonce` as the tx nonce. Signed with `sign_transaction` (no chain_id —
/// the signature form `tx.verify()` checks), so a faucet tx passes the mempool
/// gate.
///
/// FEE = ACCOUNT_CREATION_FEE, not MINIMUM_FEE. A faucet dispense goes to a
/// wallet the chain has never seen — that is what a faucet IS — and
/// `apply_transaction` requires `fee >= ACCOUNT_CREATION_FEE` for a transfer to
/// a non-existent recipient (storage/src/state.rs, "transfer to new account
/// requires fee"). MINIMUM_FEE is 10x too small, so EVERY dispense was doomed.
///
/// It failed invisibly: the mempool gate only checks `fee >= MINIMUM_FEE`, so
/// the tx was accepted everywhere, gossiped, and selected into a block — then
/// discarded by `select_applicable_txs`, which (until this was found) dropped
/// trial-apply failures with no log at all. The dispenser had already consumed
/// its in-memory nonce, so the faucet then rejected every later claim as
/// "invalid nonce" until the node restarted. No faucet dispense had ever landed
/// on the chain.
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
        fee: commputer_core::transaction::ACCOUNT_CREATION_FEE,
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

// ── Track-2 (Phase A): PoUW job-submission builder (inert, non-protected substrate) ──
//
// These are the money-path builders the PROTECTED Phase B `POST /submit_job` handler
// will call once `RpcState` gains `da_store` / `da_command_tx` fields (added by the
// founder in main.rs's protected RpcState constructor). Exactly like the faucet's
// `build_faucet_transfer`, this is a pure free function: it takes the DA store + the
// submitter wallet, does the DA publish + tx build/sign, and returns the artifacts.
// It is NOT reachable from any route yet, so a node that never enables DA is
// byte-identical on-chain. Verified by `build_and_publish_job_*` tests below.

/// Maximum combined `program.len() + input.len()` a single job may carry.
///
/// The DA publisher wraps the two into ONE envelope `[program_len:u32 LE][program][input]`
/// (`encode_job_blob`), and the frozen da crate refuses an envelope needing more than
/// 128 data chunks (its GF(2^8) rate-1/2 ceiling → 256 coded chunks). The 4-byte length
/// prefix is part of that envelope, so the payload ceiling is `128 * chunk_size - 4`.
/// Reject above this with a precise error rather than surfacing an opaque `DaError::TooLarge`.
#[allow(dead_code)]
pub const MAX_JOB_BLOB_BYTES: usize =
    128 * commputer_da::params::DEFAULT_CHUNK_SIZE as usize - 4; // = 8_388_604 at the 64 KiB default

/// Minimum `comme_budget` (raw units) a job pot may carry: 1 $COMME.
///
/// `comme_budget` is ESCROWED into the per-job pot at submit and, on a Confirmed
/// settlement, split worker 85% / verifiers 10% / burn 5%. A 1-COMME floor keeps every
/// slice well above `MINIMUM_FEE` and out of dust-rounding territory.
#[allow(dead_code)]
pub const MIN_JOB_BUDGET: u64 = commputer_core::token::UNITS_PER_COMME; // 100_000_000

/// Default declared job duration stamped into the built `SubmitJobV2` (the Phase B
/// handler may later thread a caller-supplied value; the substrate uses a fixed default).
const DEFAULT_JOB_MAX_DURATION_SECS: u64 = 3_600;

/// Everything that can go wrong building + publishing a job before it is admitted.
#[allow(dead_code)]
#[derive(Debug)]
pub enum JobSubmitError {
    /// `program.len() + input.len()` exceeds [`MAX_JOB_BLOB_BYTES`] (would overflow the DA
    /// 128-data-chunk ceiling).
    TooLarge { got: usize, max: usize },
    /// `budget` is below [`MIN_JOB_BUDGET`].
    BudgetTooLow { got: u64, min: u64 },
    /// The DA publisher failed to build the attestation or persist a coded chunk.
    Publish(commputer::da_publisher::PublishError),
}

impl std::fmt::Display for JobSubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobSubmitError::TooLarge { got, max } =>
                write!(f, "job blob too large: {got} bytes (max {max})"),
            JobSubmitError::BudgetTooLow { got, min } =>
                write!(f, "job budget too low: {got} raw units (min {min})"),
            JobSubmitError::Publish(e) => write!(f, "da publish failed: {e}"),
        }
    }
}

impl std::error::Error for JobSubmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JobSubmitError::Publish(e) => Some(e),
            _ => None,
        }
    }
}

impl From<commputer::da_publisher::PublishError> for JobSubmitError {
    fn from(e: commputer::da_publisher::PublishError) -> Self {
        JobSubmitError::Publish(e)
    }
}

/// Publish a job's `program‖input` into the node-local DA store and build a signed
/// `SubmitJobV2` transaction anchoring the resulting `da_root`.
///
/// Steps: (1) size-cap `program+input`; (2) budget floor; (3) `publish_job_blob` persists
/// the 2N coded chunks + returns the `DaAttestation`; (4) compute the linchpin
/// `program_hash = sha256(program)` / `input_hash = sha256(input)`; (5) build + sign a
/// `SubmitJobV2 { da_root, program_hash, input_hash, comme_budget, … }` with the submitter
/// wallet. Returns the tx + attestation; the caller (Phase B handler) advertises
/// `live_chunk_hashes(&att)` over the DA backend and submits the tx.
///
/// NONCE (Phase B, PROTECTED): the built tx carries `nonce = 0`. This free function does
/// not read chain state, so it cannot know the submitter's account nonce. The Phase B
/// handler — which owns `ChainState` — MUST set the submitter's real nonce and re-sign (or
/// this builder must gain a `nonce` parameter at wire-in). With `nonce = 0` the signature
/// is valid (self-consistent) and the tx is structurally complete, which is all the inert
/// substrate + its tests require. This is called out in the Track-2 Phase B checklist.
#[allow(dead_code)]
pub fn build_and_publish_job(
    store: &commputer::da_store::DaStore,
    program: &[u8],
    input: &[u8],
    budget: u64,
    nonce: u64,
    submitter: &commputer_core::wallet::Wallet,
) -> Result<(Transaction, commputer_da::params::DaAttestation), JobSubmitError> {
    use sha2::{Digest, Sha256};

    // (1) Size cap: envelope = 4-byte len prefix + program + input must stay under the
    //     128-data-chunk DA ceiling. Reject early with a precise error.
    let payload = program.len().saturating_add(input.len());
    if payload > MAX_JOB_BLOB_BYTES {
        return Err(JobSubmitError::TooLarge { got: payload, max: MAX_JOB_BLOB_BYTES });
    }

    // (2) Budget floor.
    if budget < MIN_JOB_BUDGET {
        return Err(JobSubmitError::BudgetTooLow { got: budget, min: MIN_JOB_BUDGET });
    }

    // (3) Publish program‖input into the DA store (persists 2N coded chunks + the Q15
    //     attestation whose da_root this tx anchors). Deterministic — no clock, no rng.
    let att = commputer::da_publisher::publish_job_blob(store, program, input)?;

    // (4) The linchpin identities the verification game re-binds on fetch/re-exec.
    let program_hash: [u8; 32] = Sha256::digest(program).into();
    let input_hash: [u8; 32] = Sha256::digest(input).into();

    // (5) Build + sign the SubmitJobV2 with the submitter's key at the caller-supplied
    //     nonce. The Phase-B handler passes the submitter's real next nonce
    //     (on-chain + pending); signing binds it, so the caller must not mutate it after.
    let mut tx = Transaction {
        from: *submitter.address(),
        nonce,
        kind: commputer_core::transaction::TxKind::SubmitJobV2 {
            program_hash,
            input_hash,
            da_root: att.da_root,
            // Minimal CPU declaration; the Phase B handler may parameterize from the request.
            resources: commputer_core::compute::ResourceRequirements::cpu_only(1, 0),
            max_duration_secs: DEFAULT_JOB_MAX_DURATION_SECS,
            comme_budget: commputer_core::token::Amount::from_raw(budget),
            l2_id: None,
        },
        fee: commputer_core::transaction::MINIMUM_FEE,
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    };
    commputer_core::signing::sign_transaction(&mut tx, submitter);

    Ok((tx, att))
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

    // D6 dispense path: only when a faucet wallet is provisioned on this node.
    // The whole check-build-send is serialized under the faucet nonce lock (E3)
    // so N concurrent claims from one IP cannot each pass the per-epoch check
    // before any claim is recorded, and the nonce cannot desync. On a send
    // failure the nonce is NOT consumed and the claim is NOT recorded (retryable).
    if let Some(wallet) = state.faucet_wallet.as_ref() {
        // The address was length-validated (32 bytes) above.
        let to = match hex::decode(&req.address)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
        {
            Some(arr) => commputer_core::identity::Address(arr),
            None => {
                return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                    "error": "invalid address format (expected 64 hex characters)",
                })));
            }
        };

        let mut next_nonce = state.faucet_next_nonce.lock().await;
        {
            let claims = state.faucet_claims.lock().await;
            if let Some(&last_epoch) = claims.get(&req.address)
                && last_epoch >= current_epoch
            {
                return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({
                    "error": "faucet already claimed this epoch",
                    "next_available_epoch": current_epoch + 1,
                })));
            }
        }

        let tx = build_faucet_transfer(wallet, to, *next_nonce);
        let tx_hash = hex::encode(tx.hash().0);

        // WAIT FOR THE REAL VERDICT before claiming anything happened.
        //
        // This used to answer `"1 COMME dispensed"` the instant `try_send`
        // succeeded, and consume BOTH the epoch claim slot and the nonce in that
        // same breath. A dispense the gate then rejected was therefore invisible
        // AND unretryable: the caller was told they had been paid, and their one
        // claim for the epoch was already spent. That is strictly worse than a
        // generic silent loss, and it is what the alpha.7 notes promised to fix.
        //
        // The nonce lock is deliberately held across this await, which serializes
        // dispenses. That is the point: apply requires `tx.nonce == account.nonce`
        // EXACTLY (storage/src/state.rs), so a gap or a reuse bricks every later
        // dispense. Serializing keeps at most one faucet tx in flight, which is
        // what makes the rejection path below safe to resync. The faucet is
        // inherently low-rate — one claim per address per epoch — so this costs
        // nothing real.
        match submit_awaiting_verdict(&state, tx).await {
            TxVerdict::Admitted => {
                *next_nonce += 1;
                state.faucet_claims.lock().await.insert(req.address.clone(), current_epoch);
                return (StatusCode::OK, Json(serde_json::json!({
                    "success": true,
                    "detail": "1 COMME dispensed",
                    "address": req.address,
                    "epoch": current_epoch,
                    "tx_hash": tx_hash,
                })));
            }
            TxVerdict::Rejected(reason) => {
                // Nothing was dispensed: consume NEITHER the nonce NOR the claim
                // slot, so the caller can simply try again.
                //
                // RESYNC the nonce to chain truth. Our tx did not enter the
                // mempool, so the account's on-chain nonce is authoritative for
                // the next attempt. This is safe only because the lock above
                // guarantees no other faucet tx is in flight — otherwise this
                // could rewind beneath one and mint a duplicate nonce.
                if let Some(chain_nonce) = state
                    .balances
                    .lock()
                    .await
                    .get(&hex::encode(wallet.address().0))
                    .map(|b| b.nonce)
                {
                    if *next_nonce != chain_nonce {
                        warn!(
                            "faucet nonce resync after rejection: {} -> {} (chain)",
                            *next_nonce, chain_nonce
                        );
                        *next_nonce = chain_nonce;
                    }
                }
                return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                    "error": "faucet dispense rejected",
                    "detail": reason,
                    "address": req.address,
                })));
            }
            TxVerdict::Unconfirmed => {
                // Ambiguous, and the ONLY case where we consume optimistically.
                // The tx is queued and will most likely be admitted; if we did
                // not advance the nonce and it lands, the next dispense would
                // reuse a spent nonce and fail. If it turns out to have been
                // rejected, the resync above repairs the drift on the next
                // attempt. The claim slot is NOT spent, so the caller keeps
                // their epoch claim either way.
                *next_nonce += 1;
                return (StatusCode::ACCEPTED, Json(serde_json::json!({
                    "success": false,
                    "detail": "queued but not yet confirmed (node busy); it may still be \
                               dispensed — check /balance before retrying",
                    "address": req.address,
                    "tx_hash": tx_hash,
                })));
            }
            TxVerdict::QueueFull | TxVerdict::NodeStopping => {
                // Never queued — consume NOTHING (retryable).
                return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
                    "error": "faucet temporarily unavailable",
                    "detail": "node is busy; retry shortly",
                    "address": req.address,
                    "epoch": current_epoch,
                })));
            }
        }
    }

    // Unprovisioned faucet (no wallet on this node): honest 503 (F-6). Do NOT
    // consume the per-epoch claim slot on a request we cannot fulfill.
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

/// Alpha.5 (R1): env var consulted for the admin key when no `--rpc-key` /
/// config key was provided. CLI/config always wins; an empty value counts as
/// unset (an empty-string "key" would be satisfiable by an empty header).
pub const RPC_KEY_ENV: &str = "COMMPUTER_RPC_KEY";

/// Admin-key precedence: the explicitly configured key first, else a NON-EMPTY
/// env-supplied value. Pure (the env value is passed in) so the precedence is
/// testable without process-global env mutation.
fn resolve_admin_key(configured: Option<String>, env_val: Option<String>) -> Option<String> {
    configured.or_else(|| env_val.filter(|v| !v.is_empty()))
}

/// The key the auth gate and bind guard actually enforce: `--rpc-key` else
/// `COMMPUTER_RPC_KEY`. Read per call — an env lookup is negligible next to
/// any handler, and skipping a process-wide cache keeps the value observable
/// under test.
fn effective_api_key(state: &RpcState) -> Option<String> {
    resolve_admin_key(state.api_key.clone(), std::env::var(RPC_KEY_ENV).ok())
}

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
/// Alpha.5 (R1): "configured" means `--rpc-key`/config OR the `COMMPUTER_RPC_KEY`
/// env fallback (`effective_api_key`) — CLI/config wins when both are set.
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
    if let Some(ref expected_key) = effective_api_key(&state) {
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
/// Track-2 (Phase B): POST /submit_job request body.
#[derive(Debug, Deserialize)]
pub struct SubmitJobRequest {
    /// Hex-encoded program (WASM) bytes.
    pub program_hex: String,
    /// Hex-encoded input bytes.
    pub input_hex: String,
    /// Job budget in raw COMME units.
    pub budget: u64,
    /// 24-word BIP39 seed of the submitter wallet. Sensitive — this route is keyed-tier;
    /// the seed transits to this node only (run behind loopback / TLS).
    pub submitter_seed: String,
}

/// POST /submit_job (keyed tier) — publish a job's program‖input blob to DA (persist the coded
/// chunks + the Q15 attestation, advertise them) and submit a `SubmitJobV2` carrying its `da_root`.
/// This is the entry point that makes a job's bytes retrievable so executors/verifiers can run it.
async fn submit_job(
    State(state): State<Arc<RpcState>>,
    Json(mut req): Json<SubmitJobRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    use zeroize::Zeroize;
    fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
        (code, Json(serde_json::json!({ "error": msg })))
    }

    let Some(store) = state.da_store.clone() else {
        req.submitter_seed.zeroize();
        return err(StatusCode::SERVICE_UNAVAILABLE, "DA backend not enabled on this node");
    };
    let program = match hex::decode(req.program_hex.trim()) {
        Ok(b) => b,
        Err(_) => { req.submitter_seed.zeroize(); return err(StatusCode::BAD_REQUEST, "invalid program_hex"); }
    };
    let input = match hex::decode(req.input_hex.trim()) {
        Ok(b) => b,
        Err(_) => { req.submitter_seed.zeroize(); return err(StatusCode::BAD_REQUEST, "invalid input_hex"); }
    };
    let submitter = match commputer_core::wallet::Wallet::from_seed_phrase(req.submitter_seed.trim()) {
        Ok(w) => w,
        Err(_) => { req.submitter_seed.zeroize(); return err(StatusCode::BAD_REQUEST, "invalid submitter_seed"); }
    };
    req.submitter_seed.zeroize();
    let budget = req.budget;

    // Submitter's next nonce (best-effort from the balances snapshot; 0 for an unseen account).
    let addr_hex = hex::encode(submitter.address().0);
    let nonce = state.balances.lock().await.get(&addr_hex).map(|b| b.nonce).unwrap_or(0);

    // Publish (Reed-Solomon coding + disk writes) OFF the async runtime.
    let (tx, att) = match tokio::task::spawn_blocking(move || {
        build_and_publish_job(&store, &program, &input, budget, nonce, &submitter)
    })
    .await
    {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => return err(StatusCode::BAD_REQUEST, &format!("publish failed: {:?}", e)),
        Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "publish task failed"),
    };

    // Advertise every published chunk (+ the Q15 attestation object) so committee members can fetch.
    if let Some(ref da_tx) = state.da_command_tx {
        use commputer_pouw_onchain::da_transport::DaCommand;
        let me = commputer_da::params::ProviderId(att.da_root); // informational; Advertise = local start_providing
        for ch in commputer::da_publisher::live_chunk_hashes(&att) {
            let _ = da_tx.send(DaCommand::Advertise { chunk_hash: ch, me });
        }
    }

    // Wait for the real verdict. This previously answered `accepted: true` on a
    // successful queue, so a job whose transaction the gate then rejected was
    // reported as submitted — after the chunks had already been published to DA
    // and advertised to the network. The submitter had no way to learn their job
    // would never run.
    let tx_hash = hex::encode(tx.hash().0);
    let da_root = hex::encode(att.da_root);
    match submit_awaiting_verdict(&state, tx).await {
        TxVerdict::Admitted => (
            StatusCode::OK,
            Json(serde_json::json!({
                "accepted": true,
                "da_root": da_root,
                "tx_hash": tx_hash,
            })),
        ),
        TxVerdict::Rejected(reason) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "accepted": false,
                "error": "job transaction rejected",
                "detail": reason,
                "da_root": da_root,
                "tx_hash": tx_hash,
            })),
        ),
        TxVerdict::Unconfirmed => (
            // Queued, outcome unknown. NOT a rejection — the DA chunks are
            // already published, so a resubmit is wasteful; poll instead.
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "accepted": false,
                "detail": "queued but not yet confirmed (node busy); the job may still be \
                           admitted — poll the job status before resubmitting",
                "da_root": da_root,
                "tx_hash": tx_hash,
            })),
        ),
        TxVerdict::QueueFull => err(StatusCode::SERVICE_UNAVAILABLE, "node busy; retry"),
        TxVerdict::NodeStopping => {
            err(StatusCode::INTERNAL_SERVER_ERROR, "node stopped before confirming the job")
        }
    }
}

/// cors_middleware is applied as the OUTERMOST layer (over the whole merged app)
/// so OPTIONS preflight is answered before auth/rate-limit.
pub fn build_router(rpc_state: Arc<RpcState>) -> Router {
    // Alpha.5 (R1): surface the unkeyed-admin condition loudly at build time.
    // rpc_bind_guard only inspects the node's OWN bind — behind a reverse proxy
    // a loopback bind passes the guard while the proxy exposes the keyless
    // admin tier (and /submit_job) to the internet.
    if effective_api_key(&rpc_state).is_none() {
        warn!(
            "RPC admin endpoints (including /submit_job) are UNKEYED — anyone who can reach \
             this RPC (including via a reverse proxy) can submit jobs; set --rpc-key or \
             COMMPUTER_RPC_KEY"
        );
    }

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
        .route("/compliance", get(get_compliance))
        .route("/anti-scale", get(get_anti_scale))
        .route_layer(middleware::from_fn_with_state(rpc_state.clone(), rate_limit_middleware));

    // ADMIN tier — rate-limited AND key-gated (auth_middleware).
    let admin = Router::new()
        .route("/metrics", get(get_metrics))
        .route("/metrics/prometheus", get(get_prometheus_metrics))
        .route("/storage/metrics", get(get_storage_metrics))
        .route("/traffic", get(get_traffic))
        .route("/network/quality", get(get_peer_quality))
        .route("/peers/full", get(get_peers_full))
        // Track-2 (Phase B): job submission. KEYED tier — it accepts a submitter seed, so it is
        // NOT on the public tier (the founder may relax to public / loopback-only per §5). 32 MiB
        // body limit for the program+input payload.
        .route("/submit_job", post(submit_job).layer(DefaultBodyLimit::max(32 * 1024 * 1024)))
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
    // being silently exposed to the network. Alpha.5 (R1): the guard consults
    // the EFFECTIVE key — an env-only key (COMMPUTER_RPC_KEY, no --rpc-key)
    // must permit a non-loopback bind exactly as a CLI key does.
    if let Err(reason) = rpc_bind_guard(&rpc_bind, effective_api_key(&rpc_state).is_some()) {
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

    fn make_rpc_state() -> (Arc<RpcState>, mpsc::Receiver<RpcTxRequest>) {
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
            faucet_wallet: None,
            faucet_next_nonce: Mutex::new(0),
            da_store: None,
            da_command_tx: None,
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

        // /tx now AWAITS the event loop's real verdict, so drive the request
        // and a stand-in event loop (drain the channel, reply Ok) concurrently.
        let tx_hash = tx.hash();
        let client = tokio::spawn(async move { app.oneshot(req).await.unwrap() });
        let received = rx.recv().await.expect("tx forwarded to the channel");
        assert_eq!(received.tx.hash(), tx_hash);
        received
            .reply
            .expect("/tx must set a reply channel")
            .send(Ok(()))
            .unwrap();

        let resp = client.await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let result: SubmitTxResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert!(result.accepted, "accepted only after the event loop confirms admission");
        assert!(!result.tx_hash.is_empty());
        assert!(result.error.is_none());
    }

    /// The point of the validate-before-acknowledge change: a rejection is
    /// reported to the submitter (accepted=false + reason), not swallowed. This
    /// used to be impossible — /tx answered accepted=true before the gate ran.
    #[tokio::test]
    async fn submit_tx_reports_the_real_rejection() {
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

        let client = tokio::spawn(async move { app.oneshot(req).await.unwrap() });
        let received = rx.recv().await.expect("tx forwarded to the channel");
        // Stand-in event loop rejects it, as validate_tx_for_mempool would.
        received
            .reply
            .expect("/tx must set a reply channel")
            .send(Err("invalid nonce".to_string()))
            .unwrap();

        let resp = client.await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let result: SubmitTxResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert!(!result.accepted, "a rejected tx must not report accepted");
        assert_eq!(result.error.as_deref(), Some("invalid nonce"));
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

    /// QC-006: /rewards must quote the real halving schedule, not a literal.
    ///
    /// Pinned against the OLD constant explicitly. A test that only checked
    /// "reward > 0" passed just as happily while the endpoint was understating
    /// by ~6,850x, which is exactly how the defect survived on a public route.
    #[tokio::test]
    async fn rewards_are_derived_from_the_emission_schedule_not_a_literal() {
        // Must be an address that can actually EARN. An arbitrary string here
        // now lands in the not-in-the-consensus-set branch — which is how the
        // first version of this test failed, and a fair sign the old fixture
        // was describing a node that never existed.
        let addr: String = crate::testnet_genesis::ALPHA_PINNED_VALIDATORS
            .first()
            .map(|s| (*s).to_string())
            .unwrap_or_else(|| "ab".repeat(32)); // pin retired: any registrant is eligible

        let (state, _rx) = make_rpc_state();
        state.balances.lock().await.insert(
            addr.clone(),
            BalanceInfo {
                address: addr.clone(),
                balance: 0,
                tier: "standard".to_string(),
                nonce: 0,
                is_validator: true,
                total_mined: 0,
            },
        );
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/rewards/{addr}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let got = v["estimated_reward"].as_u64().unwrap();

        // Read the height and validator count back out of the response rather
        // than restating the harness's 42 / 1, so bumping the shared fixture
        // cannot fail this test with a bare numeric mismatch. (QC-006 rev 3,
        // M-3: this used to divide by a dead literal `/ 1` that silently
        // encoded the single-validator assumption instead of naming it.)
        let height = v["height"].as_u64().unwrap();
        let bpd = v["blocks_per_day"].as_u64().unwrap();
        let eligible_validators = v["eligible_validators"].as_u64().unwrap();
        let expected = commputer_core::token::block_reward(height) * bpd / eligible_validators;
        assert_eq!(got, expected, "must be block_reward x observed blocks/day / eligible validators");

        // The real regression guard: never again the literal.
        let old_literal = 100u64 * commputer_core::token::UNITS_PER_COMME;
        assert_ne!(got, old_literal, "regressed to the hardcoded 100 COMME/day");

        // Blocks/day must come from the MEASURED average, not the 2s target.
        // Default fixture has no block samples, so the monitor's own 2.0s
        // fallback applies — assert we consumed it rather than a constant.
        let avg = v["avg_block_time_secs"].as_f64().unwrap();
        assert!(avg > 0.0 && avg.is_finite(), "block time must be usable");
        assert_eq!(bpd, (86_400.0 / avg) as u64, "blocks/day must follow the observed rate");
        assert_eq!(v["eligible_validators"].as_u64().unwrap(), 1);

        // QC-006 rev 3 (I-3b): this fixture never seeds `chain_health`, so it
        // IS the no-samples/boot case — assert the provenance flag says so,
        // rather than letting a silent 2.0s assumption pass as a measurement.
        assert_eq!(avg, 2.0, "no samples were seeded; must be the documented floor");
        assert_eq!(
            v["block_time_source"].as_str().unwrap(),
            "default_no_samples",
            "boot-time fallback must be labelled, not presented as a measurement"
        );
    }

    /// QC-006 rev 3 (I-3a): rev 2's test derived its expectations from the
    /// response's own fields (`bpd == 86_400 / avg`, both read out of the
    /// same payload), so it would still pass if the handler deleted the
    /// `chain_health` read and hardcoded 2.0 — precisely the defect this
    /// epic exists to catch. This test seeds a REAL, non-default average and
    /// checks against literals computed independently, offline, below —
    /// deleting the `chain_health` read fails this test.
    #[tokio::test]
    async fn rewards_follow_a_seeded_block_time_not_the_2s_floor() {
        // Three eligible validators, matching the live chain's founder count.
        // Under the allowlist regime that means the three pinned addresses;
        // once the allowlist retires (formation-test), `MIN_CONSENSUS_BOND`
        // is itself 0 (consensus_set.rs), so any registered validator is
        // eligible — either way, use real, distinct 64-hex addresses (rev-1's
        // "abc" fixture bug must not come back).
        let addrs: Vec<String> = if crate::testnet_genesis::pin_is_active() {
            crate::testnet_genesis::ALPHA_PINNED_VALIDATORS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        } else {
            vec!["aa".repeat(32), "bb".repeat(32), "cc".repeat(32)]
        };
        assert_eq!(addrs.len(), 3, "fixture must exercise all three eligible slots");

        let (state, _rx) = make_rpc_state();
        {
            let mut balances = state.balances.lock().await;
            for addr in &addrs {
                balances.insert(addr.clone(), BalanceInfo {
                    address: addr.clone(),
                    balance: 0,
                    tier: "standard".to_string(),
                    nonce: 0,
                    is_validator: true,
                    total_mined: 0,
                });
            }
        }
        // A real measured average, not the 2.0s no-samples floor.
        *state.chain_health.lock().await = serde_json::json!({"avg_block_time": 2.884});

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/rewards/{}", addrs[0]))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        // Hand-computed offline, independent of the handler under test:
        //   height = 42 (make_rpc_state's fixed fixture) => era 0 =>
        //     block_reward = INITIAL_BLOCK_REWARD = 1_585_489_599 raw
        //   avg_block_time = 2.884s => blocks_per_day = (86_400/2.884) as u64
        //     = 29_958
        //   estimated_reward = 1_585_489_599 * 29_958 / 3 = 15_832_699_135_614 raw
        // This independently reproduces the repo's own 2026-07-30 research
        // figure of ~158,327 COMME/day to four digits (consensus_set.rs:75-76;
        // rev-2 review, Angle 3).
        assert_eq!(v["height"].as_u64().unwrap(), 42);
        assert_eq!(v["avg_block_time_secs"].as_f64().unwrap(), 2.884);
        assert_eq!(v["block_time_source"].as_str().unwrap(), "measured");
        assert_eq!(
            v["blocks_per_day"].as_u64().unwrap(),
            29_958,
            "must follow the seeded rate, not the 2s floor"
        );
        assert_eq!(v["eligible_validators"].as_u64().unwrap(), 3);
        assert_eq!(
            v["estimated_reward"].as_u64().unwrap(),
            15_832_699_135_614,
            "must follow the seeded block time; this literal cannot be produced by \
             the 2.0s no-samples fallback the endpoint uses at boot"
        );

        // QC-006 rev 3b (I-1 re-review): under `--features formation-test`
        // this address is queried in the PIN-RETIRED regime (`pin_active() ==
        // false`), which is the one currently-compilable build that actually
        // serves a regime-B response — proving `eligibility_basis` is real,
        // reachable output here, not dead code, per the re-review's I-2
        // caveat. Under the default (pin-active) build the same field must
        // read "allowlist" instead.
        let expected_basis = if crate::testnet_genesis::pin_is_active() { "allowlist" } else { "bonded-stake" };
        assert_eq!(v["eligibility_basis"].as_str().unwrap(), expected_basis);
        assert!(v["estimated_reward_basis"].as_str().unwrap().contains("stake weight"));
    }

    /// QC-006 follow-up: registration is free and automatic, but while the alpha
    /// allowlist is in force only pinned addresses ever produce. An unpinned
    /// registrant must be told zero — the previous behaviour quoted it a full
    /// validator's income, and correcting the emission figure made that lie
    /// ~6,850x larger on the endpoint operator.html sends newcomers to.
    #[tokio::test]
    async fn an_unpinned_registrant_is_told_it_earns_nothing() {
        if !crate::testnet_genesis::pin_is_active() {
            return; // allowlist retired; eligibility is stake and this case is moot
        }
        let (state, _rx) = make_rpc_state();
        state.balances.lock().await.insert(
            "dead".repeat(16),
            BalanceInfo {
                address: "dead".repeat(16),
                balance: 0,
                tier: "standard".to_string(),
                nonce: 0,
                is_validator: true,
                total_mined: 0,
            },
        );
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/rewards/{}", "dead".repeat(16)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v["estimated_reward"].as_u64().unwrap(),
            0,
            "a registrant outside the consensus set earns nothing and must be told so"
        );
        assert!(v["note"].as_str().unwrap().contains("consensus set"));
        // QC-006 rev 3b: while the pin is active this negative claim IS
        // verifiable (the snapshot has the whole allowlist), so the basis
        // must read "allowlist", not the unverifiable-regime label.
        assert_eq!(v["eligibility_basis"].as_str().unwrap(), "allowlist");
    }

    /// QC-006 rev 3b (I-1 re-review): rev 3's `bonded = 0` fix made the
    /// pin-retired early-return diverge EVERY caller to a note that stated,
    /// as unqualified fact, "this address is never selected to produce and
    /// earns nothing" — false for a genuinely-bonded validator once the set
    /// actually opens, because this handler never looked at the real bond.
    /// `ineligibility_response` is the fix: it is pulled out of the handler
    /// specifically so the two regimes' wording is a single, directly
    /// testable decision, independent of which feature set happens to be
    /// compiled in.
    ///
    /// This is a plain unit test, not gated by `--features formation-test`
    /// or `cfg(test)` regime plumbing, because the branch it covers is NOT
    /// reachable end-to-end through the live handler under ANY build in this
    /// repo today: production's `pin_active() == false` combined with a
    /// non-zero `MIN_CONSENSUS_BOND` doesn't exist yet (that is the future
    /// state I-1 is about), and `--features formation-test` retires the pin
    /// but ALSO zeroes `MIN_CONSENSUS_BOND` in the same commit
    /// (`consensus_set.rs`), so `eligible()` never returns `false` for a
    /// validator there either — see
    /// `rewards_follow_a_seeded_block_time_not_the_2s_floor`'s formation-test
    /// run, which necessarily takes the SUCCESS path in that regime, not
    /// this one. Calling the pure function directly is what makes the
    /// pin-retired wording verifiable at all without touching
    /// `consensus_set.rs` / `testnet_genesis.rs` (out of scope for this
    /// lane) to decouple the two constants.
    ///
    /// RESIDUAL: closing this for real — i.e. making the pin-retired branch
    /// reachable with a genuine bonded-or-not answer instead of "unknown" —
    /// needs real per-address bonded stake plumbed into `BalanceInfo`. That
    /// is a set-opening precondition tracked against QC-006 in the QC
    /// ledger, not something this function can supply on its own.
    #[test]
    fn ineligibility_response_distinguishes_verified_from_unverifiable() {
        let pinned = ineligibility_response("addr", 7, true);
        assert_eq!(pinned["eligibility_basis"], "allowlist");
        assert_eq!(pinned["estimated_reward"], 0);
        assert_eq!(pinned["epoch"], 7);
        let note = pinned["note"].as_str().unwrap();
        assert!(
            note.contains("earns nothing"),
            "pin-active is a VERIFIED negative — the snapshot has the whole \
             allowlist, so this claim is one the handler can back"
        );

        let retired = ineligibility_response("addr", 7, false);
        assert_eq!(retired["eligibility_basis"], "bonded-stake");
        assert_eq!(retired["estimated_reward"], 0);
        let note = retired["note"].as_str().unwrap();
        assert!(
            !note.contains("earns nothing"),
            "pin-retired must NOT assert a specific fact about bonded stake \
             this handler never looked at"
        );
        assert!(
            note.contains("UNKNOWN") || note.contains("cannot verify"),
            "must say eligibility is unverifiable from this snapshot, not silently \
             imply it was checked and failed: got {note:?}"
        );
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
    // clippy::await_holding_lock is CORRECT to warn in production, so it is
    // allowed HERE ONLY rather than workspace-wide. This is a std::sync::Mutex
    // used purely to serialize tests that mutate a PROCESS-GLOBAL env var; the
    // guard must outlive the awaits or a concurrent test can observe the wrong
    // environment. Production async code uses tokio::sync::Mutex, which this
    // lint does not flag.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn loopback_passes_through_when_no_key_set() {
        // Alpha.5 (R1): "no key" now also means "no COMMPUTER_RPC_KEY in env" —
        // hold the env lock so the fallback tests cannot set it concurrently.
        let _env = lock_rpc_key_env();
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

    /// PUBLIC route (Phase 0 honesty): /compliance and /anti-scale must be
    /// reachable with NO key even when an admin key IS configured, and their
    /// bodies must carry the honest `enforcement_active: false` signal. This
    /// pins the Phase 0 change against two regressions at once: moving the
    /// routes back behind auth_middleware, or reverting a handler to read the
    /// (now-unwritten) compliance_stats/anti_scale_metrics mutex.
    #[tokio::test]
    async fn compliance_and_anti_scale_are_public_and_honest() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let compliance_req = Request::builder()
            .method("GET")
            .uri("/compliance")
            .body(Body::empty())
            .unwrap();
        let compliance_resp = app.clone().oneshot(compliance_req).await.unwrap();
        assert_eq!(
            compliance_resp.status(),
            StatusCode::OK,
            "public /compliance MUST stay open without a key even when an admin key is set"
        );
        let body_bytes = axum::body::to_bytes(compliance_resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let compliance: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(compliance["enforcement_active"], false);

        let anti_scale_req = Request::builder()
            .method("GET")
            .uri("/anti-scale")
            .body(Body::empty())
            .unwrap();
        let anti_scale_resp = app.oneshot(anti_scale_req).await.unwrap();
        assert_eq!(
            anti_scale_resp.status(),
            StatusCode::OK,
            "public /anti-scale MUST stay open without a key even when an admin key is set"
        );
        let body_bytes = axum::body::to_bytes(anti_scale_resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let anti_scale: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(anti_scale["enforcement_active"], false);
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

    // ── Alpha.5 (R1): admin-key env fallback (COMMPUTER_RPC_KEY) tests ──

    /// Serializes every test that sets OR depends on the absence of
    /// COMMPUTER_RPC_KEY — process env is global across the parallel test
    /// threads. Poison-tolerant: a failed env test must not cascade.
    static RPC_KEY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_rpc_key_env() -> std::sync::MutexGuard<'static, ()> {
        RPC_KEY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Removes COMMPUTER_RPC_KEY on drop so a failing assertion cannot leak
    /// the var into later tests.
    struct RpcKeyEnvGuard;
    impl Drop for RpcKeyEnvGuard {
        fn drop(&mut self) {
            // SAFETY: process-global env mutation, serialized by RPC_KEY_ENV_LOCK
            // (held by every test that touches or observes this var).
            unsafe { std::env::remove_var(RPC_KEY_ENV) };
        }
    }

    /// Precedence is pure and pinned without env mutation: CLI/config wins,
    /// env fills the gap, an empty env value counts as unset.
    #[test]
    fn resolve_admin_key_precedence() {
        assert_eq!(
            resolve_admin_key(Some("cli-key".into()), Some("env-key".into())),
            Some("cli-key".to_string()),
            "an explicitly configured key MUST win over the env fallback"
        );
        assert_eq!(
            resolve_admin_key(None, Some("env-key".into())),
            Some("env-key".to_string()),
            "with no configured key, a non-empty env value supplies the key"
        );
        assert_eq!(
            resolve_admin_key(None, Some(String::new())),
            None,
            "an empty env value MUST count as unset, not as an empty-string key"
        );
        assert_eq!(resolve_admin_key(None, None), None);
    }

    /// With NO CLI/config key but COMMPUTER_RPC_KEY exported, the admin tier
    /// MUST enforce the env-supplied key: keyless request 401s, the correct
    /// header reaches the handler.
    // clippy::await_holding_lock is CORRECT to warn in production, so it is
    // allowed HERE ONLY rather than workspace-wide. This is a std::sync::Mutex
    // used purely to serialize tests that mutate a PROCESS-GLOBAL env var; the
    // guard must outlive the awaits or a concurrent test can observe the wrong
    // environment. Production async code uses tokio::sync::Mutex, which this
    // lint does not flag.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn env_fallback_supplies_admin_key() {
        let _env = lock_rpc_key_env();
        let _cleanup = RpcKeyEnvGuard;
        // SAFETY: serialized by RPC_KEY_ENV_LOCK; removed by RpcKeyEnvGuard.
        unsafe { std::env::set_var(RPC_KEY_ENV, "env-admin-key") };

        let (state, _rx) = make_rpc_state();
        assert!(state.api_key.is_none(), "helper default must be no-key");
        let app = build_router(state);

        let unauth = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(unauth).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "with COMMPUTER_RPC_KEY set and no CLI key, admin routes MUST require the env key"
        );

        let authed = Request::builder()
            .method("GET")
            .uri("/metrics")
            .header("X-API-Key", "env-admin-key")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(authed).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the env-supplied key MUST be accepted on admin routes"
        );
    }

    /// /submit_job is ADMIN-tier: with a key configured, a keyless POST MUST
    /// 401 before the handler runs; the correct key reaches the handler (503
    /// here — DA is off in the test state — which proves auth passed).
    #[tokio::test]
    async fn submit_job_requires_key_when_configured() {
        let (mut state, _rx) = make_rpc_state();
        Arc::get_mut(&mut state)
            .expect("state Arc must be unique before router build")
            .api_key = Some("s3cret-key".to_string());
        let app = build_router(state);

        let body = serde_json::json!({
            "program_hex": "00",
            "input_hex": "00",
            "budget": 1u64,
            "submitter_seed": "not-a-real-seed",
        })
        .to_string();

        let unauth = Request::builder()
            .method("POST")
            .uri("/submit_job")
            .header("content-type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap();
        let resp = app.clone().oneshot(unauth).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "/submit_job MUST be key-gated — it accepts a submitter seed"
        );

        let authed = Request::builder()
            .method("POST")
            .uri("/submit_job")
            .header("content-type", "application/json")
            .header("X-API-Key", "s3cret-key")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(authed).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "with the correct key the request MUST reach the handler (DA off ⇒ 503)"
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

    // ── /faucet now waits for the REAL verdict ────────────────────────────────
    //
    // The bug these pin: /faucet answered "1 COMME dispensed" the instant the tx
    // was QUEUED, consuming the caller's one-per-epoch claim slot and the faucet
    // nonce in the same breath. A dispense the gate then rejected was invisible
    // AND unretryable. What matters is not just the status code — it is which
    // side effects survive each verdict.

    /// Stand in for the event loop: take one request and answer with `verdict`.
    fn fake_event_loop(
        mut rx: mpsc::Receiver<RpcTxRequest>,
        verdict: Result<(), String>,
    ) -> tokio::task::JoinHandle<Option<Transaction>> {
        tokio::spawn(async move {
            let req = rx.recv().await?;
            if let Some(reply) = req.reply {
                let _ = reply.send(verdict);
            }
            Some(req.tx)
        })
    }

    /// `Wallet` is deliberately not `Clone` (it holds signing material), so the
    /// generated wallet is MOVED into the state and its address returned for
    /// assertions.
    fn state_with_faucet() -> (Arc<RpcState>, mpsc::Receiver<RpcTxRequest>, Address) {
        let (state, rx) = make_rpc_state();
        let wallet = Wallet::generate();
        let faucet_addr = *wallet.address();
        // make_rpc_state builds an Arc; rebuild with the faucet wired in.
        let mut inner = Arc::try_unwrap(state).ok().expect("sole owner of the test state");
        inner.faucet_wallet = Some(wallet);
        (Arc::new(inner), rx, faucet_addr)
    }

    async fn call_faucet(state: &Arc<RpcState>, address: &str) -> (StatusCode, serde_json::Value) {
        let (code, Json(body)) = faucet(
            State(state.clone()),
            Json(FaucetRequest { address: address.to_string() }),
        )
        .await;
        (code, body)
    }

    /// THE FIX. A rejected dispense must leave the caller able to try again:
    /// no claim slot spent, no nonce burned, and the real reason surfaced.
    #[tokio::test]
    async fn faucet_rejection_consumes_neither_claim_slot_nor_nonce() {
        let (state, rx, _w) = state_with_faucet();
        let addr = "11".repeat(32);
        let loop_task = fake_event_loop(rx, Err("insufficient fee for account creation".into()));

        let (code, body) = call_faucet(&state, &addr).await;
        loop_task.await.unwrap();

        assert_eq!(code, StatusCode::BAD_REQUEST, "a rejection must not read as success");
        assert_eq!(body["detail"], "insufficient fee for account creation",
            "the real gate reason must reach the caller, not vanish into a log");
        assert!(
            !state.faucet_claims.lock().await.contains_key(&addr),
            "REGRESSION: the epoch claim slot was spent on a dispense that never happened"
        );
        assert_eq!(
            *state.faucet_next_nonce.lock().await, 0,
            "REGRESSION: a rejected tx burned a nonce; apply requires an exact match, so a \
             gap bricks every later dispense"
        );
    }

    /// Serve a SEQUENCE of verdicts, so one test can make several calls.
    fn fake_event_loop_seq(
        mut rx: mpsc::Receiver<RpcTxRequest>,
        verdicts: Vec<Result<(), String>>,
    ) -> tokio::task::JoinHandle<Vec<Transaction>> {
        tokio::spawn(async move {
            let mut seen = Vec::new();
            for v in verdicts {
                match rx.recv().await {
                    Some(req) => {
                        if let Some(reply) = req.reply {
                            let _ = reply.send(v);
                        }
                        seen.push(req.tx);
                    }
                    None => break,
                }
            }
            seen
        })
    }

    /// The property a user actually cares about: a rejected dispense can simply
    /// be tried again, and the retry succeeds — same address, same epoch, and
    /// crucially the SAME nonce, because the failed attempt burned nothing.
    #[tokio::test]
    async fn faucet_retry_after_rejection_succeeds_and_reuses_the_nonce() {
        let (state, rx, _w) = state_with_faucet();
        let addr = "22".repeat(32);
        let loop_task =
            fake_event_loop_seq(rx, vec![Err("transient gate rejection".into()), Ok(())]);

        let (first, _) = call_faucet(&state, &addr).await;
        assert_eq!(first, StatusCode::BAD_REQUEST);
        assert!(
            !state.faucet_claims.lock().await.contains_key(&addr),
            "the address must still be allowed to claim this epoch"
        );

        let (second, body) = call_faucet(&state, &addr).await;
        assert_eq!(second, StatusCode::OK, "the retry must be allowed and must succeed");
        assert_eq!(body["success"], true);

        let sent = loop_task.await.unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent[0].nonce, sent[1].nonce,
            "the rejected attempt must not have consumed the nonce, so the retry reuses it"
        );
        assert_eq!(*state.faucet_next_nonce.lock().await, 1, "only the success advances it");
    }

    /// A successful dispense DOES consume both — otherwise the per-epoch limit
    /// is meaningless and the nonce desyncs.
    #[tokio::test]
    async fn faucet_success_consumes_claim_slot_and_advances_nonce() {
        let (state, rx, _w) = state_with_faucet();
        let addr = "33".repeat(32);
        let loop_task = fake_event_loop(rx, Ok(()));

        let (code, body) = call_faucet(&state, &addr).await;
        let sent = loop_task.await.unwrap().expect("a tx must have been submitted");

        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["success"], true);
        assert_eq!(sent.nonce, 0, "the first dispense must use the starting nonce");
        assert!(state.faucet_claims.lock().await.contains_key(&addr));
        assert_eq!(*state.faucet_next_nonce.lock().await, 1);
    }

    /// A second claim in the same epoch is still refused after the fix.
    #[tokio::test]
    async fn faucet_still_enforces_one_claim_per_epoch() {
        let (state, rx, _w) = state_with_faucet();
        let addr = "44".repeat(32);
        let loop_task = fake_event_loop(rx, Ok(()));
        let (first, _) = call_faucet(&state, &addr).await;
        loop_task.await.unwrap();
        assert_eq!(first, StatusCode::OK);

        // No event loop this time: if the per-epoch check did not short-circuit,
        // this would hang on the verdict instead of returning immediately.
        let (second, body) = call_faucet(&state, &addr).await;
        assert_eq!(second, StatusCode::TOO_MANY_REQUESTS);
        assert!(body["error"].as_str().unwrap().contains("already claimed"));
    }

    /// The faucet substrate builds a valid, signed 1-COMME Transfer with the
    /// requested nonce, the account-creation fee, and the correct recipient.
    #[test]
    fn build_faucet_transfer_makes_valid_signed_1_comme() {
        let faucet = Wallet::generate();
        let recipient = Address([9u8; 32]);
        let tx = build_faucet_transfer(&faucet, recipient, 7);

        assert!(tx.verify(), "faucet tx must carry a valid signature");
        assert_eq!(tx.from, *faucet.address());
        assert_eq!(tx.nonce, 7);
        // The fee must cover ACCOUNT CREATION, not merely MINIMUM_FEE. A faucet
        // dispense always goes to a wallet the chain has never seen, and
        // apply_transaction rejects a transfer to a non-existent recipient
        // whose fee is below ACCOUNT_CREATION_FEE.
        //
        // This assertion used to read `>= MINIMUM_FEE`, which the broken value
        // satisfied — so the suite stayed green while every dispense on the live
        // chain was rejected at block-apply and silently dropped. Assert the
        // condition the CONSENSUS rule imposes, not the weaker one the mempool
        // gate happens to check.
        assert!(
            tx.fee >= commputer_core::transaction::ACCOUNT_CREATION_FEE,
            "faucet always creates a new account; fee {} must cover ACCOUNT_CREATION_FEE {}",
            tx.fee,
            commputer_core::transaction::ACCOUNT_CREATION_FEE,
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

    // ── Track-2 (Phase A): PoUW submit_job builder tests ──

    /// A unique temp dir per test invocation for the in-process DaStore.
    fn submit_job_tmp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "commputer-submit-job-{tag}-{}-{}",
            std::process::id(),
            nanos
        ))
    }

    /// `build_and_publish_job` produces a valid, signed `SubmitJobV2` whose `da_root`
    /// matches the published attestation and whose linchpin hashes / budget are correct,
    /// AND every coded chunk it published is retrievable from the DA store.
    #[test]
    fn build_and_publish_job_makes_valid_signed_submit_job_v2() {
        use commputer::da_store::DaStore;
        use sha2::{Digest, Sha256};

        let program = b"\x00\x61\x73\x6d\x01\x00\x00\x00program-bytes".to_vec();
        let input = b"da-input-bytes".to_vec();
        let budget = MIN_JOB_BUDGET; // exactly at the floor — accepted

        let dir = submit_job_tmp_dir("valid");
        let store = DaStore::open(&dir).expect("open da store");
        let submitter = Wallet::generate();

        let (tx, att) = build_and_publish_job(&store, &program, &input, budget, 0, &submitter)
            .expect("build_and_publish_job succeeds");

        // (1) Valid, signed, from the submitter.
        assert!(tx.verify(), "submit_job tx must carry a valid signature");
        assert_eq!(tx.from, *submitter.address(), "tx.from must be the submitter");
        assert!(
            tx.fee >= commputer_core::transaction::MINIMUM_FEE,
            "SubmitJobV2 must cover MINIMUM_FEE"
        );

        // (2) A SubmitJobV2 whose da_root matches the attestation + correct linchpins/budget.
        match tx.kind {
            TxKind::SubmitJobV2 {
                program_hash,
                input_hash,
                da_root,
                comme_budget,
                ..
            } => {
                assert_eq!(da_root, att.da_root, "tx da_root must match the published attestation");
                let ph: [u8; 32] = Sha256::digest(&program).into();
                let ih: [u8; 32] = Sha256::digest(&input).into();
                assert_eq!(program_hash, ph, "program_hash must be sha256(program)");
                assert_eq!(input_hash, ih, "input_hash must be sha256(input)");
                assert_eq!(comme_budget.raw(), budget, "comme_budget must equal the escrowed budget");
            }
            other => panic!("expected SubmitJobV2, got {:?}", other),
        }

        // (3) Every attested coded chunk is retrievable from the store (NON-VACUOUS: proves
        //     the publish actually persisted the DA set the da_root commits to).
        let live = commputer::da_publisher::live_chunk_hashes(&att);
        assert!(!live.is_empty(), "a published job has at least one coded chunk");
        for key in &live {
            assert!(store.has(*key), "every published chunk must be retrievable");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A budget below the floor is rejected BEFORE any DA disk write (nothing is published).
    #[test]
    fn build_and_publish_job_rejects_low_budget() {
        use commputer::da_store::DaStore;

        let dir = submit_job_tmp_dir("low-budget");
        let store = DaStore::open(&dir).expect("open da store");
        let submitter = Wallet::generate();

        let err = build_and_publish_job(&store, b"prog", b"in", MIN_JOB_BUDGET - 1, 0, &submitter)
            .expect_err("sub-floor budget must be rejected");
        match err {
            JobSubmitError::BudgetTooLow { got, min } => {
                assert_eq!(got, MIN_JOB_BUDGET - 1);
                assert_eq!(min, MIN_JOB_BUDGET);
            }
            other => panic!("expected BudgetTooLow, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An oversized `program + input` is rejected up front (returns before publishing).
    #[test]
    fn build_and_publish_job_rejects_oversized_blob() {
        use commputer::da_store::DaStore;

        let dir = submit_job_tmp_dir("oversized");
        let store = DaStore::open(&dir).expect("open da store");
        let submitter = Wallet::generate();

        // One byte over the payload ceiling (empty input, program = MAX + 1).
        let program = vec![0u8; MAX_JOB_BLOB_BYTES + 1];
        let err = build_and_publish_job(&store, &program, b"", MIN_JOB_BUDGET, 0, &submitter)
            .expect_err("oversized blob must be rejected");
        match err {
            JobSubmitError::TooLarge { got, max } => {
                assert_eq!(got, MAX_JOB_BLOB_BYTES + 1);
                assert_eq!(max, MAX_JOB_BLOB_BYTES);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
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

        // QC-006 rev 3, M-4: get_pending_rewards is now the widest lock-holder
        // in the file (status → balances → chain_health, three mutexes) but
        // was not exercised here. It takes all three locks regardless of
        // whether the address is found, so no balance needs to be seeded.
        let sc = state.clone();
        let c = tokio::spawn(async move {
            let _ = get_pending_rewards(State(sc), Path("nonexistent".to_string())).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Release status; a single-order implementation runs both to completion.
        drop(status_guard);

        let joined = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let _ = a.await;
            let _ = b.await;
            let _ = c.await;
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

    #[test]
    fn compliance_dashboard_is_honest_until_enforcement_exists() {
        let d = honest_compliance_dashboard();
        assert!(!d.enforcement_active);
        assert_eq!(d.current_nerf_percentage, 80);
        assert!(d.note.contains("not yet wired"));
    }

    #[test]
    fn anti_scale_dashboard_is_honest_until_enforcement_exists() {
        let d = honest_anti_scale_dashboard();
        assert!(!d.enforcement_active);
        assert!(d.note.contains("not yet wired"));
        assert!(d.total_warehouse_detections == 0);
    }
}
