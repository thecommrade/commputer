use std::collections::{HashMap, HashSet};
use commputer_core::transaction::TxHash;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;
use tracing::{info, warn, debug, error, trace};
use futures::{StreamExt, FutureExt};

use commputer_core::block::{Block, BlockHeader, BlockHash};
use commputer_core::identity::{Address, HardwareFingerprint};
use commputer_core::transaction::Transaction;
use commputer_core::token::UNITS_PER_COMME;
use commputer_core::wallet::Wallet;

use commputer_consensus::emission::EmissionSchedule;
use commputer_consensus::epoch::EpochState;

use commputer_storage::state::ChainState;
use commputer_storage::job_pool::{JobPool, PoolJob, JobId as PoolJobId};

use commputer_network::transport::{CommpNetwork, CommpBehaviourEvent};
use commputer_network::topics;

use commputer_validator::lifecycle::{ValidatorState, ValidatorStatus};
use commputer_validator::compliance_check::ComplianceChecker;

use commputer::chain_health_monitor::{ChainHealthMonitor, FinalizeMethod};
use crate::consensus_manager::{ConsensusManager, ConsensusMessage};
use crate::proof_manager::{ProofManager, ProofMessage};

// ---------------------------------------------------------------------------
// Peer exchange types (replaces PeerResponse in NetworkMessage)
// ---------------------------------------------------------------------------

/// Maximum number of peer addresses to include in a single exchange message.
const MAX_PEERS_PER_EXCHANGE: usize = 20;

/// A peer exchange message — includes our address and addresses of known peers.
/// Serialized as JSON and published on the TOPIC_PEER_ADDRS topic.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PeerExchangeMessage {
    /// Per-peer addresses: key = peer_id.to_string() or "us", value = multiaddr strings.
    peers: HashMap<String, Vec<String>>,
    /// Sender's own listen addresses.
    our_addresses: Vec<String>,
}

/// Feature 172: Minimum peers before we consider the network partitioned.
/// Item 2: Lowered to 1 — a node with 1 peer should still produce blocks.
const MINIMUM_PEERS: usize = 1;

/// Feature 174: Block height at which protocol v2 rules activate.
/// Set to u64::MAX — not yet activated.
pub const PROTOCOL_V2_ACTIVATION_HEIGHT: u64 = u64::MAX;

/// Connection quality metrics per peer (latency, message counts).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerQuality {
    pub avg_latency_ms: u64,
    pub messages_received: u64,
    pub messages_dropped: u64,
    pub connected_since: u64,
}

impl Default for PeerQuality {
    fn default() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            avg_latency_ms: 0,
            messages_received: 0,
            messages_dropped: 0,
            connected_since: now,
        }
    }
}

/// Snapshot of an epoch finalization, produced off-task by spawn_blocking
/// verdict computation and consumed by a dedicated `tokio::select!` arm.
///
/// Why: the previous design ran the verifier loop inline inside the
/// `epoch_interval` arm body, which blocks all other arms of the same
/// `tokio::select!` (block_interval, swarm, etc.) until it returns. Even
/// `tokio::task::block_in_place` can't help — block_in_place migrates the
/// calling tokio task to another worker, but a select! arm body is part of
/// that task; the select itself can't fire other arms until the body
/// completes. Empirical evidence: stress runs showed block production
/// stalled for ~110s during epoch transitions.
///
/// New design: `handle_epoch_tick` does only the cheap setup work (early-gate
/// + transition logging), then dispatches the heavy verifier work to
/// `spawn_blocking`. The blocking task computes verdicts via rayon, packages
/// the result into an `EpochFinalizeData`, and sends it back via mpsc. A
/// dedicated select arm receives the message and runs `handle_epoch_tick_post`
/// — applying verdicts, EpochState reset, account scans, etc.
pub struct EpochFinalizeData {
    pub verdicts: std::collections::HashMap<[u8; 32], commputer_core::proof::ProofVerdict>,
    pub multipliers: HashMap<commputer_core::proof::ResourceChannel, f64>,
    /// The epoch number this finalization is for (sanity check at apply time).
    pub epoch_being_finalized: u64,
    /// Validator count at the time the epoch tick fired.
    pub validator_count: u64,
}

#[allow(dead_code)]
/// Main event loop for a Commputer node. Coordinates network, consensus, proofs, and chain state.
pub struct EventLoop {
    pub state: ChainState,
    pub wallet: Wallet,
    pub network: CommpNetwork,
    pub emission: EmissionSchedule,
    pub epoch_state: EpochState,
    pub validator: ValidatorState,
    pub compliance: ComplianceChecker,
    pub pending_txs: Vec<Transaction>,
    pub consensus: ConsensusManager,
    pub proof_manager: ProofManager,
    /// Sender for proof responses produced by spawn_blocking solver workers.
    /// Cloned into each spawned worker; the worker sends back a finished `ProofResponse`.
    pub solver_response_tx: tokio::sync::mpsc::UnboundedSender<commputer_core::proof::ProofResponse>,
    /// Receiver for proof responses; polled in the main `tokio::select!` loop and
    /// turned into a published `ProofMessage::Response` on the event-loop task.
    pub solver_response_rx: tokio::sync::mpsc::UnboundedReceiver<commputer_core::proof::ProofResponse>,
    /// Sender for completed epoch finalization data (verdicts + meta) from the
    /// spawn_blocking verdict-computation worker. The receive arm of the main
    /// select! applies the verdicts and runs the rest of the epoch transition.
    pub epoch_finalize_tx: tokio::sync::mpsc::UnboundedSender<EpochFinalizeData>,
    /// Receiver for completed epoch finalization data.
    pub epoch_finalize_rx: tokio::sync::mpsc::UnboundedReceiver<EpochFinalizeData>,
    /// Detected hardware fingerprint for this node.
    pub hardware: HardwareFingerprint,
    /// Maps libp2p PeerIds to their observed IP addresses.
    pub peer_ips: HashMap<libp2p::PeerId, String>,
    /// Maps libp2p PeerIds to their Commputer validator addresses (learned from registration txs).
    pub peer_validators: HashMap<libp2p::PeerId, Address>,
    /// Peers banned for sending invalid data (bad blocks, bad signatures).
    pub banned_peers: HashSet<libp2p::PeerId>,
    /// Transaction hashes already seen (in finalized blocks or mempool) for dedup.
    pub seen_tx_hashes: HashSet<TxHash>,
    /// Per-peer message rate tracking: (peer_id -> (count, window_start)).
    pub peer_msg_rates: HashMap<libp2p::PeerId, (u32, std::time::Instant)>,
    /// Peer reputation scores: higher is better. Starts at 100.
    pub peer_scores: HashMap<libp2p::PeerId, i32>,
    /// Receiver for transactions submitted via the RPC server.
    pub rpc_rx: Option<mpsc::Receiver<crate::rpc::RpcTxRequest>>,
    /// Shared RPC state (for updating the status snapshot).
    pub rpc_state: Option<Arc<crate::rpc::RpcState>>,
    /// Feature 127: Orphan block pool — blocks whose parents we don't have yet.
    pub orphan_pool: HashMap<BlockHash, Vec<Block>>,
    /// Feature 128: Block propagation timing — when we first saw each block.
    pub block_seen_times: HashMap<BlockHash, u64>,
    /// Feature 128: Block propagation delays (in milliseconds) for percentile tracking.
    pub propagation_delays: Vec<u64>,
    /// Feature 131: Track (producer, height) -> BlockHash for duplicate block detection.
    pub producer_blocks: HashMap<(Address, u64), BlockHash>,
    /// Feature 130: Track when we last saw a block at the expected height.
    pub last_block_seen_time: Option<std::time::Instant>,
    /// Feature 167: Observed external address from identify protocol (for NAT detection).
    pub observed_external_addr: Option<String>,
    /// Feature 170: Track peer /16 subnets for geographic diversity.
    pub peer_subnets: HashMap<libp2p::PeerId, String>,
    /// Feature 171: RTT measurements per peer (PeerId -> last RTT in ms).
    pub peer_rtts: HashMap<libp2p::PeerId, u64>,
    /// Feature 171: Ping timestamps: PeerId -> when we last sent a ping.
    pub ping_timestamps: HashMap<libp2p::PeerId, std::time::Instant>,
    /// Feature 172: Whether a network partition has been detected.
    pub partition_detected: bool,
    /// QC-009 attestation: PeerId -> validator Address, bound by a signed
    /// challenge/response at connect (`/commputer/attest/1`, see
    /// `commputer_core::attest`). Vote intake counts a peer only if it is bound
    /// here to a validator ELIGIBLE at USE time (eligibility is never cached).
    /// Per-connection, in-memory only; removed on ConnectionClosed. Replaces the
    /// forgeable, write-only `verified_peer_validators` (which keyed on the gossip
    /// relayer, not the vote originator).
    pub attested_peers: HashMap<libp2p::PeerId, Address>,
    /// Outstanding attest challenges: PeerId -> the nonce we issued. Cleared on a
    /// verified Proof or on ConnectionClosed.
    pub pending_attest: HashMap<libp2p::PeerId, [u8; 32]>,
    /// Liveness floor: the last active tick at which we held >=1 bound eligible
    /// peer. The unbound-vote fallback engages only after GRACE_T with zero bound
    /// eligible peers, so a genuinely isolated node degrades to clamp semantics
    /// rather than halting, while any honest peer keeps the clock fresh and locks
    /// an attacker out.
    pub last_bound_at: Option<std::time::Instant>,
    /// Kill lever (formation-test builds only): suppresses challenge/answer so the
    /// liveness-floor fallback can be exercised deliberately. Always false in a
    /// production build — there is no runtime off-switch for the gate.
    pub attest_disabled: bool,
    /// Feature 177: Connection quality metrics per peer.
    pub peer_quality: HashMap<libp2p::PeerId, PeerQuality>,
    /// Feature 178: Custom seed multiaddrs for periodic reconnection.
    pub custom_seeds: Vec<String>,
    /// Feature 8: Data directory path for mempool persistence.
    pub data_dir: Option<std::path::PathBuf>,
    /// Whether this node connected to seeds (non-seed nodes should wait before producing blocks).
    pub is_seed_connector: bool,
    /// Whether this node has ever successfully connected to a peer.
    pub has_ever_connected: bool,
    /// Feature 20: Transaction signature verification cache.
    /// Capped at SIG_CACHE_MAX entries (LRU-style: clear when full).
    pub sig_cache: HashSet<TxHash>,
    /// Item 18: Recent gossipsub message IDs for duplicate suppression.
    /// Gossipsub has built-in dedup, but this catches application-level duplicates.
    pub seen_message_ids: HashSet<[u8; 32]>,
    /// Item 51: Mempool transaction expiry tracking — maps tx hash to when it was added.
    pub mempool_added_at: HashMap<TxHash, std::time::Instant>,
    /// Feature 9: Pending epoch summary to include in the next block.
    pub pending_epoch_summary: Option<commputer_core::block::EpochSummary>,
    /// Item 18: In-memory job pool for compute job lifecycle management.
    pub job_pool: JobPool,
    /// Connection count per peer (TCP+QUIC can create multiple connections).
    pub peer_connection_count: HashMap<libp2p::PeerId, usize>,
    /// When the event loop started (for first-node timeout detection).
    pub event_loop_start: std::time::Instant,
    /// Snowball query round counter (nonce to prevent gossipsub dedup).
    pub snowball_round: u64,
    /// Whether initial sync with the network is complete.
    pub sync_complete: bool,
    /// Highest block height heard from any peer.
    pub network_height: u64,
    /// Node operating state: Syncing, Active, or Stale.
    pub node_state: commputer::node_state::NodeStateMachine,
    /// Sync state machine: controlled batch downloading with backpressure.
    pub sync_machine: commputer::sync_machine::SyncMachine,
    /// Consensus rate limiter -- prevents vote spam and duplicate votes.
    pub consensus_rate_limiter: commputer_network::consensus_rate_limiter::ConsensusRateLimiter,
    /// Sync-protocol rate limiter -- per-peer token buckets for GetBlock/GetBlocks serving.
    pub sync_rate_limiter: commputer_network::sync_rate_limiter::SyncRateLimiter,
    /// Peers who have voted for the current consensus height (avoids retry spam).
    pub voted_peers: HashSet<libp2p::PeerId>,
    /// Fork detection circuit breaker.
    pub fork_detector: commputer::fork_detector::ForkDetector,
    /// Timestamp of the first consensus stall signal. None if no stall.
    pub stall_start: Option<std::time::Instant>,
    /// Height → when we last asked a peer for it, so `request_block` does not
    /// re-ask every tick. Pruned in place; bounded by the 30s retention.
    pub block_request_at: HashMap<u64, std::time::Instant>,
    /// Remaining requeue-validation budget for THIS event-loop turn.
    /// A per-CALL cap is not enough: one turn can apply a 10-block sync batch,
    /// walk a 200-deep orphan cascade, or drain a run of finalized heights, and
    /// each of those calls would otherwise get a fresh cap. Reset at the top of
    /// the loop; see `requeue_lost_txs`.
    requeue_budget: usize,
    /// Last epoch for which the shadow schedule was computed and logged.
    /// `build_schedule` materialises the whole cycle, so it must run once per
    /// epoch, never per height.
    shadow_schedule_epoch: Option<u64>,
    /// The stake-weighted proposer schedule for the current epoch — now LIVE.
    /// Cached because `build_schedule` materialises the whole cycle (up to
    /// MAX_CYCLE_LEN entries); it must be rebuilt per epoch, never per height.
    schedule_cache: Option<commputer::schedule_epoch::EpochSchedule>,
    /// Local height at the last stall-triggered NON-destructive sync re-engage.
    /// One re-engage attempt per height: a second stall at the same height means
    /// sync gained nothing (fork shape) and recovery escalates to the
    /// destructive resync. u64::MAX = no attempt yet.
    pub stall_reengage_height: u64,
    /// Cooldown: when the last chain resync completed. Prevents resync loops from DoS.
    pub last_resync: Option<std::time::Instant>,
    /// Last time any message was received from each peer (for online/staleness checks).
    pub peer_last_seen: HashMap<libp2p::PeerId, std::time::Instant>,
    /// Chain health monitor: block freshness, timeout rate, avg block time, active voters.
    pub health_monitor: ChainHealthMonitor,
    // ── Track-2 (Phase B): all None/parked unless main.rs attaches them → byte-identical on-chain when off ──
    /// DA backend command receiver (from the std→tokio da-cmd-pump). None = DA off.
    pub da_command_rx: Option<tokio::sync::mpsc::UnboundedReceiver<commputer_pouw_onchain::da_transport::DaCommand>>,
    /// Node-local coded-chunk blob store, shared (Arc) with the publish/submit path + inbound serve.
    pub da_store: Option<Arc<commputer::da_store::DaStore>>,
    /// In-flight Kademlia get_providers queries → the loop's reply Sender.
    pub pending_find: HashMap<libp2p::kad::QueryId, std::sync::mpsc::Sender<Vec<commputer_da::params::ProviderId>>>,
    /// In-flight DA GetChunk requests → the loop's reply Sender.
    pub pending_fetch: HashMap<libp2p::request_response::OutboundRequestId,
        std::sync::mpsc::Sender<Option<(Vec<u8>, commputer_da::transport::MerklePath)>>>,
    /// Reversible ProviderId(tag) → dialable PeerId, populated as providers are discovered.
    pub da_provider_ids: HashMap<[u8; 32], libp2p::PeerId>,
    /// Per-block executor snapshot sender (to the off-thread executor loop). None = executor off.
    pub executor_snapshot_tx: Option<std::sync::mpsc::Sender<commputer::executor_loop::ExecutorChainView>>,
    /// Per-block verifier snapshot sender (to the off-thread verifier loop). None = verifier off.
    pub verifier_snapshot_tx: Option<std::sync::mpsc::Sender<commputer::verifier_loop::VerifierTick>>,
    /// P4: the SINGLE actor-tx receiver — both loops emit nonce-free TxKind here; the event loop is the sole nonce owner.
    pub actor_tx_rx: Option<tokio::sync::mpsc::UnboundedReceiver<commputer_core::transaction::TxKind>>,
}

impl EventLoop {
    pub fn new(
        state: ChainState,
        wallet: Wallet,
        network: CommpNetwork,
        hardware: HardwareFingerprint,
    ) -> Self {
        let epoch_state = EpochState::new(0, 0);
        let our_address = *wallet.address();
        let (solver_response_tx, solver_response_rx) = tokio::sync::mpsc::unbounded_channel();
        let (epoch_finalize_tx, epoch_finalize_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            state,
            wallet,
            network,
            emission: EmissionSchedule::new(),
            epoch_state,
            validator: ValidatorState::new(),
            compliance: ComplianceChecker::new(),
            pending_txs: Vec::new(),
            consensus: ConsensusManager::new(),
            proof_manager: ProofManager::new(our_address),
            solver_response_tx,
            solver_response_rx,
            epoch_finalize_tx,
            epoch_finalize_rx,
            hardware,
            peer_ips: HashMap::new(),
            peer_validators: HashMap::new(),
            banned_peers: HashSet::new(),
            seen_tx_hashes: HashSet::new(),
            peer_msg_rates: HashMap::new(),
            peer_scores: HashMap::new(),
            rpc_rx: None,
            rpc_state: None,
            orphan_pool: HashMap::new(),
            block_seen_times: HashMap::new(),
            propagation_delays: Vec::new(),
            producer_blocks: HashMap::new(),
            // Some(now), not None: seconds_waiting computes as 0 while this is
            // None (unwrap_or(0)), which freezes leader view-change rotation on
            // any node that boots into a chain that is not currently producing
            // — only the primary may ever produce, and if the primary is the
            // stuck one the network stays frozen (alpha.6 panel, F4 candidate).
            last_block_seen_time: Some(std::time::Instant::now()),
            observed_external_addr: None,
            peer_subnets: HashMap::new(),
            peer_rtts: HashMap::new(),
            ping_timestamps: HashMap::new(),
            partition_detected: true, // Start paused — unpause when peers connect
            attested_peers: HashMap::new(),
            pending_attest: HashMap::new(),
            last_bound_at: None,
            attest_disabled: {
                #[cfg(feature = "formation-test")]
                {
                    std::env::var("COMMPUTER_ATTEST_DISABLE").is_ok()
                }
                #[cfg(not(feature = "formation-test"))]
                {
                    false
                }
            },
            peer_quality: HashMap::new(),
            custom_seeds: Vec::new(),
            data_dir: None,
            is_seed_connector: false,
            has_ever_connected: false,
            sig_cache: HashSet::new(),
            seen_message_ids: HashSet::new(),
            pending_epoch_summary: None,
            job_pool: JobPool::new(),
            mempool_added_at: HashMap::new(),
            peer_connection_count: HashMap::new(),
            event_loop_start: std::time::Instant::now(),
            snowball_round: 0,
            sync_complete: false,
            node_state: commputer::node_state::NodeStateMachine::new(),
            sync_machine: commputer::sync_machine::SyncMachine::new(),
            consensus_rate_limiter: commputer_network::consensus_rate_limiter::ConsensusRateLimiter::new(),
            sync_rate_limiter: commputer_network::sync_rate_limiter::SyncRateLimiter::new(),
            voted_peers: HashSet::new(),
            network_height: 0,
            fork_detector: commputer::fork_detector::ForkDetector::new(),
            stall_start: None,
            block_request_at: HashMap::new(),
            requeue_budget: Self::REQUEUE_BUDGET_PER_TURN,
            shadow_schedule_epoch: None,
            schedule_cache: None,
            stall_reengage_height: u64::MAX,
            last_resync: None,
            peer_last_seen: HashMap::new(),
            health_monitor: ChainHealthMonitor::new(),
            // Track-2 (Phase B): parked until main.rs attaches them.
            da_command_rx: None,
            da_store: None,
            pending_find: HashMap::new(),
            pending_fetch: HashMap::new(),
            da_provider_ids: HashMap::new(),
            executor_snapshot_tx: None,
            verifier_snapshot_tx: None,
            actor_tx_rx: None,
        }
    }

    /// Attach the RPC channel and shared state for the RPC server.
    pub fn attach_rpc(
        &mut self,
        rx: mpsc::Receiver<crate::rpc::RpcTxRequest>,
        state: Arc<crate::rpc::RpcState>,
    ) {
        self.rpc_rx = Some(rx);
        self.rpc_state = Some(state);
        self.update_rpc_status();
    }

    /// Track-2 (Phase B): attach the DA backend — the command receiver (from the
    /// std→tokio da-cmd-pump) + the shared blob store. Idempotent-off until called.
    pub fn attach_da(
        &mut self,
        da_command_rx: tokio::sync::mpsc::UnboundedReceiver<commputer_pouw_onchain::da_transport::DaCommand>,
        da_store: Arc<commputer::da_store::DaStore>,
    ) {
        self.da_command_rx = Some(da_command_rx);
        self.da_store = Some(da_store);
    }

    /// Track-2 (Phase B, P4): attach the ONE shared actor-tx receiver — both loops
    /// emit nonce-free TxKind here and the event loop is the sole wallet-nonce owner.
    pub fn attach_actor_tx(&mut self, rx: tokio::sync::mpsc::UnboundedReceiver<commputer_core::transaction::TxKind>) {
        self.actor_tx_rx = Some(rx);
    }

    /// Track-2 (Phase B): attach the executor loop's per-block snapshot sender.
    pub fn attach_executor(&mut self, snapshot_tx: std::sync::mpsc::Sender<commputer::executor_loop::ExecutorChainView>) {
        self.executor_snapshot_tx = Some(snapshot_tx);
    }

    /// P4/P10 (Phase B): the SINGLE actor-tx sink. Both PoUW loops emit nonce-free
    /// TxKind here; this — the sole wallet-nonce owner — assigns the nonce, signs, and
    /// admits via the SAME mempool path as an RPC tx (F-3 quota + C7 ingress + dedup).
    fn emit_actor_tx(&mut self, kind: commputer_core::transaction::TxKind) {
        use commputer_core::transaction::{Transaction, TxKind, MINIMUM_FEE};
        let me = *self.wallet.address();
        // P2: capture a static tag from &kind BEFORE it moves into the literal (TxKind is not Copy).
        let tag: &'static str = match &kind {
            TxKind::ClaimJob { .. } => "ClaimJob",
            TxKind::CompleteJob { .. } => "CompleteJob",
            TxKind::Commit { .. } => "Commit",
            TxKind::Reveal { .. } => "Reveal",
            _ => "actor-tx",
        };
        let base = self.state.accounts.get(&me).map(|a| a.nonce).unwrap_or(0);
        let pending = self.pending_txs.iter().filter(|t| t.from == me).count() as u64;
        let nonce = base.saturating_add(pending);
        let mut tx = Transaction {
            from: me, nonce, kind, fee: MINIMUM_FEE,
            signature: vec![], public_key: vec![], memo: None, timelock: None,
        };
        commputer_core::signing::sign_transaction(&mut tx, &self.wallet);
        if let Err(reason) = self.validate_tx_for_mempool(&tx) {
            debug!("actor tx {} rejected pre-mempool: {}", tag, reason);
            return;
        }
        let tx_hash = tx.hash();
        self.seen_tx_hashes.insert(tx_hash);
        if let Ok(data) = serde_json::to_vec(&tx) {
            let _ = self.network.swarm.behaviour_mut().gossipsub
                .publish(topics::tx_topic(), commputer_network::compress(&data));
        }
        self.mempool_added_at.insert(tx_hash, std::time::Instant::now());
        self.pending_txs.push(tx);
        self.enforce_mempool_limit();
        debug!("Emitted actor {} (nonce {})", tag, nonce);
    }

    /// R2 (Phase B): push an executor snapshot post-apply. Runtime P9 gate — act ONLY
    /// as a bonded, eligible validator (the "auto-enable when bonded" gate). No-op off.
    fn push_executor_snapshot(&self) {
        let Some(ref tx) = self.executor_snapshot_tx else { return };
        let me = *self.wallet.address();
        let is_validator = self.state.accounts.get(&me).map(|a| a.is_validator).unwrap_or(false);
        if !is_validator || !self.state.is_eligible(&me) { return; }
        let my_balance = self.state.accounts.get(&me).map(|a| a.balance.raw()).unwrap_or(0);
        let view = commputer::executor_loop::build_chain_view(
            self.state.blocks.height(), self.state.current_epoch, me, my_balance,
            &self.state.pending_jobs, &self.state.job_lifecycles,
        );
        let _ = tx.send(view);
    }

    /// R3 (Phase B): push a verifier snapshot post-apply. Same runtime bonded gate.
    fn push_verifier_snapshot(&self) {
        let Some(ref tx) = self.verifier_snapshot_tx else { return };
        let me = *self.wallet.address();
        let is_validator = self.state.accounts.get(&me).map(|a| a.is_validator).unwrap_or(false);
        if !is_validator || !self.state.is_eligible(&me) { return; }
        let my_balance = self.state.accounts.get(&me).map(|a| a.balance.raw()).unwrap_or(0);
        let tick = commputer::verifier_loop::build_verifier_views(
            self.state.blocks.height(), me, my_balance,
            &self.state.job_lifecycles, &self.state.escalation_rounds,
        );
        let _ = tx.send(tick);
    }

    /// R1 (Phase B): stable reversible tag PeerId → ProviderId([u8;32]) = sha256(peer.to_bytes()).
    fn da_provider_tag(peer: &libp2p::PeerId) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(peer.to_bytes()).into()
    }

    /// R1 (Phase B): service one DaCommand from the off-thread loops. The swarm is
    /// single-owner (this task), so the loops inject commands + correlate async replies
    /// via pending_find/pending_fetch. A dropped reply Sender → the frozen bridge
    /// degrades to Abstain (never a hang).
    fn handle_da_command(&mut self, cmd: commputer_pouw_onchain::da_transport::DaCommand) {
        use commputer_pouw_onchain::da_transport::DaCommand;
        match cmd {
            DaCommand::Advertise { chunk_hash, .. } => {
                let key = libp2p::kad::RecordKey::new(&chunk_hash);
                let _ = self.network.swarm.behaviour_mut().kademlia.start_providing(key);
            }
            DaCommand::HasChunk { chunk_hash, reply } => {
                let has = self.da_store.as_ref().map(|s| s.has(chunk_hash)).unwrap_or(false);
                let _ = reply.send(has);
            }
            DaCommand::FindProviders { chunk_hash: _, reply } => {
                // v1 discovery (Q3 decision): DHT provider records don't propagate in a small/fresh
                // net, so treat every connected peer as a candidate. A GetChunk to a peer that lacks
                // the chunk returns None (harmless); verify_available's Merkle+sha256 rebind rejects
                // any wrong bytes, so over-asking is safe.
                let peers: Vec<libp2p::PeerId> = self.peer_ips.keys().copied().collect();
                let out: Vec<commputer_da::params::ProviderId> = peers
                    .into_iter()
                    .map(|peer| {
                        let tag = Self::da_provider_tag(&peer);
                        self.da_provider_ids.insert(tag, peer);
                        commputer_da::params::ProviderId(tag)
                    })
                    .collect();
                let _ = reply.send(out);
            }
            DaCommand::FetchChunk { chunk_hash, from, reply } => {
                // Local-first: the peer path below only dials OTHERS, so a publisher
                // drawn onto the committee could never fetch its own blob (Abstain →
                // lost verifier → flaky quorum in small nets). A node that holds the
                // chunk serves itself; wrong/corrupt local bytes are harmless —
                // verify_available's Merkle+sha256 rebind rejects them.
                let local = self
                    .da_store
                    .as_ref()
                    .and_then(|s| s.get(chunk_hash).ok().flatten())
                    .and_then(|c| {
                        commputer::da_publisher::deserialize_merkle_path(&c.merkle_path)
                            .map(|path| (c.bytes, path))
                    });
                if local.is_some() {
                    let _ = reply.send(local);
                } else if let Some(peer) = self.da_provider_ids.get(&from.0).copied() {
                    let req = commputer_network::da_protocol::DaRequest::GetChunk { chunk_hash };
                    let rid = self.network.swarm.behaviour_mut().da.send_request(&peer, req);
                    self.pending_fetch.insert(rid, reply);
                }
                // else: unknown provider tag → drop reply → facade Abstains (honest).
            }
        }
    }

    /// Wipe chain state and re-enter sync mode.
    /// Called when fork detector or stall timer triggers.
    /// Minimum seconds between resyncs to prevent DoS-triggered resync loops.
    const RESYNC_COOLDOWN_SECS: u64 = 300; // 5 minutes

    fn initiate_chain_resync(&mut self, reason: &str) {
        // Cooldown: don't resync again within 5 minutes of the last resync.
        if let Some(last) = self.last_resync {
            if last.elapsed().as_secs() < Self::RESYNC_COOLDOWN_SECS {
                warn!(
                    "Resync requested but cooldown active ({}s remaining): {}",
                    Self::RESYNC_COOLDOWN_SECS - last.elapsed().as_secs(),
                    reason
                );
                return;
            }
        }

        warn!("Initiating chain resync: {}", reason);

        // 1. Force node state to Syncing.
        self.node_state.force_syncing();

        // 2. Wipe chain state.
        if let Err(e) = self.state.reset_to_genesis() {
            tracing::error!("Failed to reset chain state: {}", e);
            return;
        }

        // 3. Clear consensus state.
        self.consensus.clear();

        // 4. Clear mempool and message dedup.
        self.pending_txs.clear();
        self.seen_tx_hashes.clear();
        self.mempool_added_at.clear();
        self.seen_message_ids.clear();

        // 5. Reset sync flag and network height so SyncMachine re-engages properly.
        self.sync_complete = false;
        self.network_height = 0;
        self.sync_machine.reset();

        // 6. Reset solo-node timeout so it doesn't fire immediately after resync.
        self.event_loop_start = std::time::Instant::now();

        // 7. Reset fork detector and stall timer.
        self.fork_detector.reset();
        self.stall_start = None;

        // 8. Reset voted peers tracking.
        self.voted_peers.clear();

        // 9. Re-queue ValidatorRegister transaction (cleared from mempool).
        // After resync the chain has no validators, so re-registration is needed.
        if self.validator.status() == ValidatorStatus::Active {
            let nonce = 0; // Chain was wiped, nonce resets.
            let mut tx = commputer_core::transaction::Transaction {
                from: *self.wallet.address(),
                nonce,
                kind: commputer_core::transaction::TxKind::ValidatorRegister {
                    hardware_fingerprint_hash: {
                        use sha2::{Sha256, Digest};
                        let hw_bytes = borsh::to_vec(&self.hardware).unwrap_or_default();
                        let hash = Sha256::digest(&hw_bytes);
                        let mut out = [0u8; 32];
                        out.copy_from_slice(&hash);
                        out
                    },
                    contribution_percent: self.validator.contribution_percent(),
                },
                fee: 0,
                signature: vec![],
                public_key: vec![],
                memo: None,
                timelock: None,
            };
            commputer_core::signing::sign_transaction(&mut tx, &self.wallet);
            // Broadcast to network so other nodes include it in their blocks.
            if let Ok(data) = serde_json::to_vec(&tx) {
                let compressed = commputer_network::compress(&data);
                let topic = topics::tx_topic();
                let _ = self.network.swarm.behaviour_mut().gossipsub.publish(topic, compressed);
            }
            self.pending_txs.push(tx);
            info!("Re-queued and broadcast ValidatorRegister transaction after resync");
        }

        // Record resync time for cooldown.
        self.last_resync = Some(std::time::Instant::now());

        info!("Chain resync initiated. Waiting for sync from peers.");
    }

    /// Feature 241: Broadcast a JSON event to all connected WebSocket clients.
    fn broadcast_ws_event(&self, event: &serde_json::Value) {
        if let Some(ref rpc) = self.rpc_state {
            let msg = serde_json::to_string(event).unwrap_or_default();
            let _ = rpc.ws_broadcast.send(msg);
        }
    }

    /// Push a fresh status snapshot to the RPC shared state.
    fn update_rpc_status(&self) {
        if let Some(ref rpc) = self.rpc_state {
            let snapshot = crate::rpc::ChainStatus {
                height: self.state.blocks.height(),
                total_supply: commputer_core::token::TOTAL_SUPPLY,
                emitted: self.state.total_emitted,
                burned: self.state.total_burned,
                circulating: self.state.circulating_supply(),
                remaining: self.state.remaining_supply(),
                accounts: self.state.accounts.len(),
                epoch: self.state.current_epoch,
                pending_txs: self.pending_txs.len(),
            };
            if let Ok(mut guard) = rpc.status.try_lock() {
                *guard = snapshot;
            }

            // Update peer info.
            if let Ok(mut peers_guard) = rpc.peers.try_lock() {
                let mut peers = Vec::new();
                for (peer_id, ip) in &self.peer_ips {
                    let validator_address = self.peer_validators.get(peer_id)
                        .map(|a| hex::encode(a.0));
                    let compliance_status = validator_address.as_ref().and_then(|_| {
                        self.peer_validators.get(peer_id).map(|addr| {
                            format!("{:?}", self.compliance.check(addr))
                        })
                    });
                    let last_seen = self.peer_last_seen.get(peer_id).copied();
                    peers.push(crate::rpc::PeerInfo {
                        peer_id: peer_id.to_string(),
                        ip: Some(ip.clone()),
                        validator_address,
                        compliance_status,
                        last_seen,
                    });
                }
                *peers_guard = peers;
            }

            // Update recent blocks for explorer endpoint.
            if let Ok(mut blk_guard) = rpc.blocks.try_lock() {
                let height = self.state.blocks.height();
                // Add any new blocks not yet tracked.
                let start = height.saturating_sub(100);
                for h in start..=height {
                    if !blk_guard.contains_key(&h)
                        && let Some(block) = self.state.blocks.get_by_height(h)
                            && let Ok(json) = serde_json::to_value(block) {
                                blk_guard.insert(h, json);
                            }
                }
                // Prune blocks older than 100 from the RPC cache.
                if height > 100 {
                    blk_guard.retain(|&h, _| h >= height - 100);
                }
            }

            // Update mempool snapshot.
            if let Ok(mut mp_guard) = rpc.mempool.try_lock() {
                *mp_guard = self.pending_txs.iter().map(|tx| {
                    crate::rpc::MempoolTxInfo {
                        tx_hash: hex::encode(tx.hash().0),
                        from: hex::encode(tx.from.0),
                        nonce: tx.nonce,
                        fee: tx.fee,
                        kind: format!("{:?}", tx.kind).chars().take(50).collect(),
                    }
                }).collect();
            }

            // Update balance info for all accounts.
            if let Ok(mut bal_guard) = rpc.balances.try_lock() {
                bal_guard.clear();
                for account in self.state.accounts.iter() {
                    let addr_hex = hex::encode(account.address.0);
                    bal_guard.insert(addr_hex.clone(), crate::rpc::BalanceInfo {
                        address: addr_hex,
                        balance: account.balance.raw(),
                        tier: format!("{:?}", account.tier()),
                        nonce: account.nonce,
                        is_validator: account.is_validator,
                        total_mined: account.total_mined.raw(),
                    });
                }
            }

            // Update receipts (only recent — last 1000).
            // Note: we only add new receipts, don't re-sync everything each tick.
            // Full receipt sync happens incrementally as blocks are finalized.

            // Update node metrics.
            if let Ok(mut met_guard) = rpc.metrics.try_lock() {
                met_guard.height = self.state.blocks.height();
                met_guard.epoch = self.state.current_epoch;
                met_guard.peers_connected = self.peer_ips.len();
                met_guard.peers_banned = self.banned_peers.len();
                met_guard.pending_txs = self.pending_txs.len();
                met_guard.seen_tx_count = self.seen_tx_hashes.len();
            }

            // Feature 180: Update network health dashboard.
            if let Ok(mut health_guard) = rpc.network_health.try_lock() {
                health_guard.peer_count = self.peer_ips.len();
                health_guard.unique_subnets = self.unique_subnet_count();
                health_guard.avg_latency_ms = self.average_peer_latency();
                health_guard.partition_risk = self.partition_risk().to_string();
            }

            // Feature 177: Update peer quality metrics.
            if let Ok(mut quality_guard) = rpc.peer_quality.try_lock() {
                quality_guard.clear();
                for (peer_id, quality) in &self.peer_quality {
                    if let Ok(json) = serde_json::to_value(quality) {
                        quality_guard.insert(peer_id.to_string(), json);
                    }
                }
            }

            // Feature 10: Update validator performance metrics.
            if let Ok(mut perf_guard) = rpc.validator_performance.try_lock() {
                perf_guard.clear();
                for (addr, perf) in &self.state.validator_performance {
                    let addr_hex = hex::encode(addr.0);
                    if let Ok(json) = serde_json::to_value(perf) {
                        perf_guard.insert(addr_hex, json);
                    }
                }
            }

            // Chain health monitor snapshot.
            if let Ok(mut ch_guard) = rpc.chain_health.try_lock() {
                let h = self.health_monitor.health();
                *ch_guard = serde_json::json!({
                    "is_healthy": h.is_healthy,
                    "stuck_seconds": h.stuck_seconds,
                    "timeout_rate": h.timeout_rate,
                    "avg_block_time": h.avg_block_time,
                    "active_voters": h.active_voters,
                    "total_voters": h.total_voters,
                    "height": h.height,
                    "issues": h.issues,
                });
            }
        }
    }

    /// Adjust a peer's reputation score. Negative values penalize, positive reward.
    fn adjust_peer_score(&mut self, peer_id: libp2p::PeerId, delta: i32) {
        let score = self.peer_scores.entry(peer_id).or_insert(100);
        *score = (*score + delta).clamp(-100, 200);
        if *score <= -50 {
            self.ban_peer(peer_id, "reputation score dropped below threshold");
        }
    }

    /// Ban a peer and disconnect them.
    fn ban_peer(&mut self, peer_id: libp2p::PeerId, reason: &str) {
        if self.banned_peers.insert(peer_id) {
            self.peer_scores.insert(peer_id, -100);
            warn!("Banning peer {} — {}", peer_id, reason);
            // Disconnect the peer.
            let _ = self.network.swarm.disconnect_peer_id(peer_id);
            // Clean up tracking.
            self.peer_ips.remove(&peer_id);
            // P3 (§2.2b): drop this peer's decay sample so a departed peer's stale
            // height can't keep influencing recompute_network_height.
            self.node_state.forget_peer_height(commputer::peer_hash::peer_bucket(&peer_id));
            if let Some(validator_addr) = self.peer_validators.remove(&peer_id) {
                self.compliance.deregister_node(&validator_addr);
            }
        }
    }

    pub async fn run(&mut self) {
        // Feature 8: Load persisted mempool on startup.
        if let Some(ref dir) = self.data_dir {
            let mempool_path = dir.join("mempool.json");
            if mempool_path.exists() {
                match std::fs::read_to_string(&mempool_path) {
                    Ok(json) => {
                        match serde_json::from_str::<Vec<Transaction>>(&json) {
                            Ok(txs) => {
                                info!("Loaded {} pending transactions from mempool.json", txs.len());
                                self.pending_txs.extend(txs);
                            }
                            Err(e) => warn!("Failed to parse mempool.json: {}", e),
                        }
                        // Remove the file after loading.
                        let _ = std::fs::remove_file(&mempool_path);
                    }
                    Err(e) => warn!("Failed to read mempool.json: {}", e),
                }
            }
        }

        // Item 21: Load persisted job pool on startup.
        if let Some(ref dir) = self.data_dir {
            match JobPool::load_from_dir(dir) {
                Ok(pool) => {
                    let total = pool.total_count();
                    self.job_pool = pool;
                    info!("Loaded job pool from disk: {} jobs", total);
                }
                Err(e) => {
                    debug!("No persisted job pool ({}), starting fresh", e);
                }
            }
        }

        // Item 6: Load config on startup to pick up seed nodes etc.
        self.reload_config();

        // Item 54: Chain verification on startup — verify last 10 blocks chain properly.
        {
            let height = self.state.blocks.height();
            let check_start = height.saturating_sub(9);
            let mut chain_ok = true;
            for h in (check_start + 1)..=height {
                if let (Some(block), Some(parent_block)) = (
                    self.state.blocks.get_by_height(h),
                    self.state.blocks.get_by_height(h - 1),
                ) {
                    if block.header.parent_hash != parent_block.hash() {
                        warn!(
                            "Chain integrity warning: block {} parent hash does not match block {} hash",
                            h, h - 1
                        );
                        chain_ok = false;
                    }
                }
            }
            if chain_ok {
                info!("Chain integrity check passed (blocks {} to {})", check_start, height);
            } else {
                warn!("Chain integrity check found inconsistencies! Consider running verify-chain.");
            }
        }

        let mut epoch_interval = time::interval(Duration::from_secs(3600));
        let mut block_interval = time::interval(Duration::from_secs(2));
        let mut consensus_interval = time::interval(Duration::from_millis(500));
        let mut proof_interval = time::interval(Duration::from_secs(300));

        // Feature 169: Peer exchange every 60s.
        let mut peer_exchange_interval = time::interval(Duration::from_secs(60));
        // Feature 171: Bandwidth/latency measurement every 30s.
        let mut ping_interval = time::interval(Duration::from_secs(30));
        // Feature 172: Partition check every 10s.
        let mut partition_check_interval = time::interval(Duration::from_secs(10));
        // Feature 178: Seed reconnection every 30 seconds when disconnected.
        let mut seed_reconnect_interval = time::interval(Duration::from_secs(30));
        // Feature 11: Automatic peer rotation every 5 minutes.
        let mut peer_rotation_interval = time::interval(Duration::from_secs(300));
        // Item 22: Job timeout enforcement every 30s.
        let mut job_timeout_interval = time::interval(Duration::from_secs(30));
        // Feature 12: SIGHUP handler for config hot reload.
        // N2 (C9b): `tokio::signal::unix` is Unix-only. Gate the registration and give non-Unix a no-op
        // that parks forever, so the `select!` arms below compile on Windows with ZERO Unix change.
        #[cfg(not(unix))]
        struct NoopSignal;
        #[cfg(not(unix))]
        impl NoopSignal {
            async fn recv(&mut self) -> Option<()> {
                std::future::pending::<Option<()>>().await
            }
        }
        #[cfg(unix)]
        let mut sighup = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::hangup(),
        ).ok();
        #[cfg(not(unix))]
        let mut sighup: Option<NoopSignal> = None;

        // Feature 11: Connection encryption verification.
        info!("P2P encryption: Noise protocol active");
        info!("Event loop started at height {}. Listening for peers...", self.state.blocks.height());

        // Sync timer: periodically check sync status and request missing blocks.
        let mut sync_timer = time::interval(Duration::from_secs(5));

        // Item 73: Periodic status line every 60 seconds.
        let mut status_line_interval = time::interval(Duration::from_secs(60));

        // Set up graceful shutdown signal handler. N2 (C9b): Unix-only; see the SIGHUP gate above.
        #[cfg(unix)]
        let mut sigterm = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ) {
            Ok(sig) => Some(sig),
            Err(e) => {
                warn!("Failed to register SIGTERM handler: {}", e);
                None
            }
        };
        #[cfg(not(unix))]
        let mut sigterm: Option<NoopSignal> = None;

        loop {
            // One requeue-validation budget per turn, shared by every apply
            // this turn performs (sync batch, orphan cascade, finalized run).
            self.requeue_budget = Self::REQUEUE_BUDGET_PER_TURN;
            // Take the RPC receiver out to satisfy the borrow checker in select!
            let rpc_recv = async {
                if let Some(ref mut rx) = self.rpc_rx {
                    rx.recv().await
                } else {
                    // No RPC channel — park forever.
                    std::future::pending::<Option<crate::rpc::RpcTxRequest>>().await
                }
            };
            // Track-2 (Phase B): DA backend commands + the single actor-tx sink.
            // Both park forever when their channel is unattached → zero cost when off.
            let da_recv = async {
                if let Some(ref mut rx) = self.da_command_rx {
                    rx.recv().await
                } else {
                    std::future::pending::<Option<commputer_pouw_onchain::da_transport::DaCommand>>().await
                }
            };
            let actor_recv = async {
                if let Some(ref mut rx) = self.actor_tx_rx {
                    rx.recv().await
                } else {
                    std::future::pending::<Option<commputer_core::transaction::TxKind>>().await
                }
            };

            tokio::select! {
                swarm_result = std::panic::AssertUnwindSafe(self.network.swarm.select_next_some()).catch_unwind() => {
                    match swarm_result {
                        Ok(event) => self.handle_swarm_event(event),
                        Err(panic_info) => {
                            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                                s.to_string()
                            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                                s.clone()
                            } else {
                                "unknown panic".to_string()
                            };
                            tracing::error!("Caught panic in libp2p swarm: {} — continuing", msg);
                        }
                    }
                }
                Some(req) = rpc_recv => {
                    info!("Received transaction from RPC: {}", hex::encode(req.tx.hash().0));
                    self.handle_rpc_transaction(req);
                }
                Some(kind) = actor_recv => {
                    // P4: the single wallet-nonce owner signs + admits the loop's tx.
                    self.emit_actor_tx(kind);
                }
                Some(cmd) = da_recv => {
                    // R1: service one DA backend command against the swarm.
                    self.handle_da_command(cmd);
                }
                _ = epoch_interval.tick() => {
                    self.handle_epoch_tick();
                    self.update_rpc_status();
                }
                _ = block_interval.tick() => {
                    self.handle_block_tick();
                    self.update_rpc_status();
                }
                _ = consensus_interval.tick() => {
                    self.handle_consensus_tick();
                }
                _ = proof_interval.tick() => {
                    self.handle_proof_tick();
                }
                Some(response) = self.solver_response_rx.recv() => {
                    // Proof solved off-runtime in spawn_blocking — record + publish on main task.
                    self.proof_manager.record_response(response.clone());
                    let resp_msg = ProofMessage::Response(response);
                    self.publish_proof_message(&resp_msg);
                }
                Some(epoch_data) = self.epoch_finalize_rx.recv() => {
                    // Verdicts arrived from the spawn_blocking verifier worker.
                    // Apply them and run the rest of the epoch transition. This
                    // arm body still runs on the event-loop task, but each call
                    // is the cheap apply phase (HashMap inserts + state mutation),
                    // not the heavy verify phase.
                    self.handle_epoch_tick_post(epoch_data);
                }
                _ = peer_exchange_interval.tick() => {
                    self.handle_peer_exchange_tick();
                }
                _ = ping_interval.tick() => {
                    self.handle_ping_tick();
                }
                _ = partition_check_interval.tick() => {
                    self.check_network_partition();
                }
                _ = seed_reconnect_interval.tick() => {
                    self.reconnect_seeds();
                }
                _ = sync_timer.tick() => {
                    // Feed the node state machine.
                    self.node_state.set_our_height(self.state.blocks.height());
                    // P3 (§2.2a): recompute the sync target from the DECAY tracker
                    // (median of authenticated per-peer samples, floored at our tip)
                    // instead of re-feeding the monotonic self.network_height. This is
                    // the switch that lets network_height DECREASE back to reality, so
                    // a single stale/orphan/poison reading can no longer pin the node
                    // in Syncing forever. Runs unconditionally every tick (not gated on
                    // is_active), so a wedged node can always climb back to Active.
                    self.node_state.recompute_network_height();

                    // Catch-up is a permanent capability, not an initial-sync
                    // one-shot: when the network's tip has moved sustainably
                    // above ours, clear the sync_complete latch and re-engage.
                    // Rate-limiting/backoff live in should_reengage. Without
                    // this, a node that completes a round to a stale target
                    // goes dormant below a moving tip (live: nodes sat 9-335
                    // blocks behind for hours; far-behind nodes never even
                    // stall because no candidates form at their next height).
                    if self.sync_complete
                        && self.sync_machine.should_reengage(
                            self.state.blocks.height(),
                            self.node_state.network_height(),
                        )
                    {
                        info!(
                            "Sync re-engage: local {} vs network {} — resuming catch-up",
                            self.state.blocks.height(),
                            self.node_state.network_height()
                        );
                        self.sync_complete = false;
                        self.sync_machine.reset();
                    }

                    if !self.sync_complete {
                        let our_height = self.state.blocks.height();

                        // Solo node timeout: if no peers have blocks after 30s, start producing.
                        // Requires actually being ALONE. Without the peer check this
                        // fires on a node that has peers but has not yet learned a
                        // network height, marking it sync_complete and resetting the
                        // sync machine — and because every re-engage is undone the same
                        // way, a freshly-wiped node can never join a running chain. Live
                        // 2026-07-25: a rejoining validator sat at height 0 for ten
                        // minutes, connected, reporting "synced", while its peers were
                        // at 1265. The log line already claims this condition.
                        if self.event_loop_start.elapsed().as_secs() >= 30
                            && self.network_height == 0
                            && self.peer_ips.is_empty() {
                            info!("No network blocks found after 30s — starting block production");
                            self.sync_complete = true;
                            self.sync_machine.reset();
                            self.node_state.force_active();
                        }
                        // Drive the sync state machine.
                        else if !self.peer_ips.is_empty() {
                            use commputer::sync_machine::SyncState;
                            let peers: Vec<libp2p::PeerId> = self.peer_ips.keys().copied().collect();

                            match self.sync_machine.state().clone() {
                                SyncState::Idle => {
                                    // Start syncing.
                                    self.sync_machine.start();
                                    // Send GetHeight to up to 3 peers.
                                    for peer in peers.iter().take(3) {
                                        let req = commputer_network::sync_protocol::SyncRequest::GetHeight;
                                        self.network.swarm.behaviour_mut().sync.send_request(peer, req);
                                    }
                                }
                                SyncState::QueryHeight => {
                                    if self.sync_machine.should_start_downloading(our_height) {
                                        let target = self.sync_machine.begin_downloading(our_height);
                                        if target == 0 && self.event_loop_start.elapsed().as_secs() >= 30 {
                                            // No peers have blocks after 30s — we're the first node.
                                            info!("No network blocks found after 30s — starting block production");
                                            self.sync_complete = true;
                                            self.sync_machine.reset();
                                            self.node_state.force_active();
                                        } else if target > 0 && our_height >= target {
                                            // AT target — complete. The old "+2 close enough"
                                            // tolerance deadlocked with leader election: a
                                            // 2-block-behind node whose turn it is to produce
                                            // can neither sync (gap swallowed here) nor
                                            // produce (not at tip) — and re-engage fires at
                                            // exactly gap>=2, looping forever (live
                                            // 2026-07-25: solar at 30, tip 32, leader for 33).
                                            info!("Initial sync complete at height {} (network at {})", our_height, target);
                                            self.sync_complete = true;
                                            self.sync_machine.reset();
                                            self.node_state.force_active();
                                        }
                                        // If target == 0 and < 30s: stay in Downloading, re-check next tick.
                                    }
                                }
                                SyncState::Downloading => {
                                    // If target is 0, we're stuck — reset and re-query.
                                    if self.sync_machine.target_height() == 0 {
                                        if self.event_loop_start.elapsed().as_secs() >= 30 {
                                            info!("No network blocks found after 30s — starting block production");
                                            self.sync_complete = true;
                                            self.sync_machine.reset();
                                            self.node_state.force_active();
                                        } else {
                                            // Re-query heights.
                                            self.sync_machine.reset();
                                            self.sync_machine.start();
                                            for peer in peers.iter().take(3) {
                                                let req = commputer_network::sync_protocol::SyncRequest::GetHeight;
                                                self.network.swarm.behaviour_mut().sync.send_request(peer, req);
                                            }
                                        }
                                    } else {
                                        // Normal batch download.
                                        if self.sync_machine.batch_timed_out() {
                                            if let Some(peer) = self.sync_machine.select_peer(&peers) {
                                                self.sync_machine.record_batch_failure(peer);
                                            }
                                        }
                                        if let Some((start, end)) = self.sync_machine.next_batch(our_height) {
                                            if let Some(peer) = self.sync_machine.select_peer(&peers) {
                                                let req = commputer_network::sync_protocol::SyncRequest::GetBlocks { start, end };
                                                self.network.swarm.behaviour_mut().sync.send_request(&peer, req);
                                                debug!("Sync: requested batch {}-{} from {}", start, end, peer);
                                            }
                                        }
                                    }
                                }
                                SyncState::Verifying => {
                                    // Re-query heights to verify we're caught up.
                                    if self.sync_machine.verification_ready() {
                                        if self.sync_machine.complete_verification(our_height) {
                                            info!("Initial sync complete at height {}", our_height);
                                            self.sync_complete = true;
                                            self.sync_machine.reset();
                                            self.node_state.force_active();
                                        }
                                        // else: back to Downloading, next tick will request.
                                    } else {
                                        // Send GetHeight to re-check.
                                        for peer in peers.iter().take(3) {
                                            let req = commputer_network::sync_protocol::SyncRequest::GetHeight;
                                            self.network.swarm.behaviour_mut().sync.send_request(peer, req);
                                        }
                                    }
                                }
                                SyncState::Complete => {
                                    info!("Initial sync complete at height {}", our_height);
                                    self.sync_complete = true;
                                    self.sync_machine.reset();
                                    self.node_state.force_active();
                                }
                            }
                        }
                    }
                }
                _ = peer_rotation_interval.tick() => {
                    self.handle_peer_rotation();
                }
                _ = job_timeout_interval.tick() => {
                    // Item 22 / B6 (C9a): OBSERVE-ONLY wall-clock tick. It touches ONLY the node-local
                    // V1 `job_pool` and emits warnings — it must NEVER settle a PoUW lifecycle or mutate
                    // ChainState. PoUW timeout settlement is 100% in-apply (`settle_due_jobs`, anchored to
                    // applied height inside the rollback envelope), so it reproduces identically on every
                    // node and on reorg replay; a wall-clock-driven settlement here would fork.
                    let height = self.state.blocks.height();
                    let penalties = self.job_pool.enforce_timeouts(height, 2);
                    for (job_id, executor) in &penalties {
                        warn!(
                            "Job {} timed out — executor {} penalized",
                            hex::encode(&job_id.0[..8]),
                            hex::encode(&executor.0[..8]),
                        );
                    }
                }
                _ = status_line_interval.tick() => {
                    // Item 73: Periodic one-line status summary.
                    let height = self.state.blocks.height();
                    let peers = self.peer_ips.len();
                    let epoch = self.state.current_epoch;
                    let balance = self.state.accounts.get(self.wallet.address())
                        .map(|a| a.balance.raw() / UNITS_PER_COMME)
                        .unwrap_or(0);
                    info!(
                        "Height: {} | Peers: {} | Balance: {} COMME | Epoch: {}",
                        height, peers, balance, epoch
                    );
                }
                _ = async {
                    if let Some(ref mut sig) = sighup {
                        sig.recv().await
                    } else {
                        std::future::pending::<Option<()>>().await
                    }
                } => {
                    info!("Received SIGHUP — reloading configuration");
                    self.reload_config();
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("Received SIGINT — shutting down gracefully");
                    self.shutdown();
                    return;
                }
                _ = async {
                    if let Some(ref mut sig) = sigterm {
                        sig.recv().await
                    } else {
                        std::future::pending::<Option<()>>().await
                    }
                } => {
                    info!("Received SIGTERM — shutting down gracefully");
                    self.shutdown();
                    return;
                }
            }
        }
    }

    /// Flush state to disk and clean up before exit.
    /// Feature 133: Save consensus state (pending blocks) for recovery on restart.
    fn shutdown(&mut self) {
        // Item 50: Publish a "goodbye" message on the blocks topic before closing.
        let goodbye = serde_json::json!({
            "type": "goodbye",
            "peer_id": self.network.local_peer_id.to_string(),
            "height": self.state.blocks.height(),
        });
        if let Ok(data) = serde_json::to_vec(&goodbye) {
            let topic = topics::block_topic();
            if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, data) {
                debug!("Failed to publish goodbye message: {}", e);
            } else {
                info!("Published goodbye message to peers");
            }
        }

        info!("Flushing chain state to disk...");
        if let Err(e) = self.state.flush() {
            warn!("Failed to flush state on shutdown: {}", e);
        } else {
            info!("Chain state flushed successfully. Height: {}", self.state.blocks.height());
        }

        // Item 15: Mark clean shutdown so next startup can detect crashes.
        self.state.mark_clean_shutdown();

        // Feature 133: Persist pending consensus state info.
        // Active heights and pending blocks are logged for debugging;
        // on restart, the node will re-sync from peers via the sync protocol.
        let active_heights = self.consensus.active_heights();
        let finalized_heights = self.consensus.finalized_heights();
        if !active_heights.is_empty() || !finalized_heights.is_empty() {
            info!(
                "Consensus state at shutdown: {} active heights, {} finalized pending",
                active_heights.len(),
                finalized_heights.len(),
            );
        }
        info!("Orphan pool: {} parent hashes with pending blocks", self.orphan_pool.len());

        // Item 21: Persist job pool on shutdown.
        if let Some(ref dir) = self.data_dir {
            if let Err(e) = self.job_pool.save_to_dir(dir) {
                warn!("Failed to persist job pool: {}", e);
            } else {
                info!("Persisted job pool: {} jobs", self.job_pool.total_count());
            }
        }

        // Feature 8: Persist pending transactions to mempool.json.
        if !self.pending_txs.is_empty()
            && let Some(ref dir) = self.data_dir {
                let mempool_path = dir.join("mempool.json");
                match serde_json::to_string_pretty(&self.pending_txs) {
                    Ok(json) => {
                        if let Err(e) = std::fs::write(&mempool_path, json) {
                            warn!("Failed to persist mempool: {}", e);
                        } else {
                            info!("Persisted {} pending transactions to {}", self.pending_txs.len(), mempool_path.display());
                        }
                    }
                    Err(e) => warn!("Failed to serialize mempool: {}", e),
                }
            }

        // Item 69: Clean shutdown message.
        println!();
        println!("  Shutting down... state saved. Goodbye.");
        println!();
    }

    /// Feature 20: Maximum signature cache size.
    #[allow(dead_code)]
    const SIG_CACHE_MAX: usize = 10_000;

    /// Feature 20: Verify a transaction signature with caching.
    /// Returns true if the signature is valid (or was previously verified).
    #[allow(dead_code)]
    pub fn verify_tx_cached(&mut self, tx: &Transaction) -> bool {
        let tx_hash = tx.hash();
        // Check cache first.
        if self.sig_cache.contains(&tx_hash) {
            return true;
        }
        // Perform full verification.
        if tx.verify() {
            // Add to cache; clear if at capacity (simple LRU approximation).
            if self.sig_cache.len() >= Self::SIG_CACHE_MAX {
                self.sig_cache.clear();
            }
            self.sig_cache.insert(tx_hash);
            true
        } else {
            false
        }
    }

    /// Feature 11: Automatic peer rotation — disconnect lowest-reputation peer
    /// and try to discover a new one from the DHT.
    fn handle_peer_rotation(&mut self) {
        // Find the peer with the lowest reputation score.
        if let Some((&worst_peer, &worst_score)) = self.peer_scores.iter()
            .min_by_key(|(_, score)| **score)
            && worst_score < 50 {
                info!(
                    "Peer rotation: disconnecting {} (score {})",
                    worst_peer, worst_score
                );
                let _ = self.network.swarm.disconnect_peer_id(worst_peer);
                self.peer_ips.remove(&worst_peer);
                // P3 (§2.2b): drop the rotated-out peer's decay sample.
                self.node_state.forget_peer_height(commputer::peer_hash::peer_bucket(&worst_peer));
                self.peer_validators.remove(&worst_peer);
                self.peer_scores.remove(&worst_peer);
                self.peer_quality.remove(&worst_peer);
                self.peer_subnets.remove(&worst_peer);
                self.peer_rtts.remove(&worst_peer);
            }

        // Eclipse attack detection: check peer subnet diversity.
        {
            use commputer_network::eclipse_attack_detector::{EclipseDetector, PeerSubnet};
            let subnets: Vec<PeerSubnet> = self.peer_ips.values()
                .map(|ip| PeerSubnet::new(ip))
                .collect();
            if subnets.len() >= 3 {
                let detector = EclipseDetector::new();
                let alerts = detector.check(&subnets);
                for alert in &alerts {
                    warn!("Eclipse detection: {}", alert.description());
                }
            }
        }

        // Try to discover a new peer from the Kademlia DHT.
        let random_key = libp2p::kad::RecordKey::new(&rand::random::<[u8; 32]>());
        self.network.swarm.behaviour_mut().kademlia.get_closest_peers(random_key.to_vec());
        debug!("Peer rotation: initiated Kademlia peer discovery");
    }

    /// Feature 12: Config hot reload — re-read config.toml and update settings.
    /// Item 6: Also reads seed nodes from config file.
    fn reload_config(&mut self) {
        // Look for commputer.toml in the data directory or current dir.
        let config_path = self.data_dir
            .as_ref()
            .map(|d| d.join("commputer.toml"))
            .unwrap_or_else(|| std::path::PathBuf::from("commputer.toml"));

        let contents = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Config reload: could not read {:?}: {}", config_path, e);
                return;
            }
        };

        // Simple key=value parser (no toml crate).
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                match key {
                    "log_level" => {
                        info!("Config reload: log_level = {}", value);
                        // Update the tracing filter if possible.
                        // This requires a reload handle which we don't have here,
                        // so we just log the intended change.
                    }
                    "contribution_percent" => {
                        if let Ok(pct) = value.parse::<u8>()
                            && (1..=100).contains(&pct) {
                                info!("Config reload: contribution_percent = {}", pct);
                            }
                    }
                    "max_peer_count" => {
                        if let Ok(_count) = value.parse::<usize>() {
                            info!("Config reload: max_peer_count = {}", value);
                        }
                    }
                    // Item 6: Read seed nodes from config file.
                    "seeds" => {
                        let seeds: Vec<String> = value.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if !seeds.is_empty() {
                            info!("Config reload: {} seed nodes from config", seeds.len());
                            self.custom_seeds = seeds;
                        }
                    }
                    _ => {
                        debug!("Config reload: ignoring unknown key '{}'", key);
                    }
                }
            }
        }
        info!("Config reload completed");
    }

    fn handle_swarm_event(
        &mut self,
        event: libp2p::swarm::SwarmEvent<CommpBehaviourEvent>,
    ) {
        use libp2p::swarm::SwarmEvent;
        use libp2p::gossipsub;

        match event {
            SwarmEvent::Behaviour(CommpBehaviourEvent::Gossipsub(
                gossipsub::Event::Message { propagation_source, message, .. }
            )) => {
                // Drop messages from banned peers.
                if self.banned_peers.contains(&propagation_source) {
                    debug!("Ignoring message from banned peer {}", propagation_source);
                    return;
                }

                // Rate limiting: max 50 messages per peer per second.
                const MAX_MSGS_PER_SEC: u32 = 50;
                let now = std::time::Instant::now();
                let entry = self.peer_msg_rates.entry(propagation_source)
                    .or_insert((0, now));
                if now.duration_since(entry.1).as_secs() >= 1 {
                    // Reset window.
                    *entry = (1, now);
                } else {
                    entry.0 += 1;
                    if entry.0 > MAX_MSGS_PER_SEC {
                        // Log but don't ban — high volume is normal during sync.
                        // The dedicated sync protocol handles bulk transfers.
                        debug!("Peer {} exceeded gossipsub rate limit ({}/s), dropping message",
                            propagation_source, entry.0);
                        return;
                    }
                }

                // Feature 177: Track messages received per peer.
                self.peer_quality.entry(propagation_source)
                    .or_default()
                    .messages_received += 1;

                // Refresh last-seen timestamp on every gossip message.
                self.peer_last_seen.insert(propagation_source, std::time::Instant::now());

                // Item 18: Application-level duplicate message suppression.
                {
                    use sha2::{Sha256, Digest};
                    let msg_hash: [u8; 32] = Sha256::digest(&message.data).into();
                    if !self.seen_message_ids.insert(msg_hash) {
                        debug!("Suppressing duplicate message from {}", propagation_source);
                        return;
                    }
                    // Prune seen set periodically to avoid unbounded growth.
                    if self.seen_message_ids.len() > 10_000 {
                        self.seen_message_ids.clear();
                    }
                }

                // Feature 173: Decompress message data before deserialization.
                let data = commputer_network::decompress(&message.data);

                let topic = message.topic.as_str();
                debug!("Gossipsub message on topic: {} from {}", topic, propagation_source);

                if topic == topics::TOPIC_BLOCKS {
                    // Feature 7: Try to parse as BlockAnnounce first, then fall back to full block.
                    if let Ok(announce) = serde_json::from_slice::<commputer_core::block::BlockAnnounce>(&data) {
                        // Check if we already have this block.
                        if !self.state.blocks.contains(&announce.hash) {
                            // Request the full block via block request protocol.
                            debug!("BlockAnnounce: need block {} at height {}", announce.hash, announce.height);
                            self.request_block(announce.height);
                        }
                    } else if let Ok(block) = serde_json::from_slice::<Block>(&data) {
                        self.handle_received_block(block, propagation_source);
                    }
                } else if topic == topics::TOPIC_TRANSACTIONS {
                    if let Ok(tx) = serde_json::from_slice::<Transaction>(&data) {
                        self.handle_new_transaction(tx, propagation_source);
                    }
                } else if topic == topics::TOPIC_CONSENSUS {
                    if let Ok(msg) = serde_json::from_slice::<ConsensusMessage>(&data) {
                        self.handle_consensus_message(msg, propagation_source);
                    }
                } else if topic == topics::TOPIC_PROOFS {
                    if let Ok(msg) = serde_json::from_slice::<ProofMessage>(&data) {
                        self.handle_proof_message(msg);
                    }
                } else if topic == topics::TOPIC_PEER_ADDRS {
                    // Feature 6: Handle peer address gossip.
                    // Updated: deserialize as PeerExchangeMessage (replaces NetworkMessage/PeerResponse).
                    // The new format includes addresses of ALL known peers, not just the sender.
                    if let Ok(msg) = serde_json::from_slice::<PeerExchangeMessage>(&data) {
                        // SECURITY(finding [16]): bound inbound work to the same cap the
                        // send side uses (MAX_PEERS_PER_EXCHANGE). Without it, one message
                        // forces thousands of base58/multihash/multiaddr parses +
                        // kademlia.add_address calls (CPU amp + routing-table pollution).
                        for (peer_str, addrs) in msg.peers.iter().take(MAX_PEERS_PER_EXCHANGE) {
                            // Skip our own entry.
                            if peer_str == "us" {
                                continue;
                            }
                            // Parse the peer ID.
                            let peer_id = match peer_str.parse::<libp2p::PeerId>() {
                                Ok(id) => id,
                                Err(_) => continue,
                            };
                            // Skip already-connected and banned peers.
                            if self.peer_ips.contains_key(&peer_id)
                                || self.banned_peers.contains(&peer_id)
                            {
                                continue;
                            }
                            // Add each address to Kademlia so libp2p can dial them.
                            for addr_str in addrs.iter().take(2) {
                                if let Ok(addr) = addr_str.parse::<libp2p::Multiaddr>() {
                                    self.network.swarm.behaviour_mut().kademlia.add_address(
                                        &peer_id, addr,
                                    );
                                }
                            }
                        }
                        debug!(
                            "[peer_exchange] processed exchange from {}: {} peer entries",
                            propagation_source,
                            msg.peers.len()
                        );
                    }
                }
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(
                    "Listening on {}/p2p/{}",
                    address, self.network.local_peer_id
                );
            }
            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                // Teach the seed keepalive this peer's identity if the remote
                // address is one of our seed targets (first statement so it
                // runs before the banned/duplicate early-returns).
                let remote_addr = endpoint.get_remote_address().clone();
                self.network.note_seed_connection(&peer_id, &remote_addr, &self.custom_seeds);
                // Mark that we've connected to at least one peer.
                if !self.has_ever_connected {
                    info!("First peer connection established — node is now eligible to produce blocks");
                    self.has_ever_connected = true;
                    self.partition_detected = false;

                    // Broadcast any pending ValidatorRegister transactions so the
                    // seed node learns our identity immediately.
                    for tx in &self.pending_txs {
                        if matches!(tx.kind, commputer_core::transaction::TxKind::ValidatorRegister { .. }) {
                            if let Ok(data) = serde_json::to_vec(tx) {
                                let compressed = commputer_network::compress(&data);
                                let topic = topics::tx_topic();
                                if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, compressed) {
                                    debug!("Failed to broadcast ValidatorRegister tx on connect: {}", e);
                                } else {
                                    info!("Broadcast ValidatorRegister tx to network");
                                }
                            }
                        }
                    }
                }
                // Reject connections from banned peers (before connection counting).
                if self.banned_peers.contains(&peer_id) {
                    info!("Rejecting connection from banned peer {}", peer_id);
                    let _ = self.network.swarm.disconnect_peer_id(peer_id);
                    return;
                }
                // Deduplicate TCP/QUIC dual-stack connections per peer.
                let conn_count = self.peer_connection_count.entry(peer_id).or_insert(0);
                *conn_count += 1;
                if *conn_count > 1 {
                    debug!("Additional connection to peer {} (now {} connections)", peer_id, conn_count);
                    return; // Already tracked as connected
                }
                // Extract the IP address from the multiaddr.
                let addr_str = endpoint.get_remote_address().to_string();
                if let Some(ip) = extract_ip_from_multiaddr(&addr_str) {
                    // Feature 170: Track peer /16 subnet for geographic diversity.
                    let subnet = extract_slash16_subnet(&ip);
                    self.peer_subnets.insert(peer_id, subnet.clone());

                    self.peer_ips.insert(peer_id, ip.clone());
                    // If we know this peer's validator address, register with compliance.
                    if let Some(validator_addr) = self.peer_validators.get(&peer_id) {
                        self.compliance.register_node(*validator_addr, ip);
                    }
                }

                // Feature 177: Initialize peer quality metrics.
                self.peer_quality.entry(peer_id).or_default();

                // Track connection as a peer activity timestamp.
                self.peer_last_seen.insert(peer_id, std::time::Instant::now());

                // Enforce connection limit: max 50 peers.
                // Feature 170: Geographic diversity — if new peer has unique /16,
                // allow even if at limit by disconnecting a duplicate-subnet peer.
                const MAX_PEERS: usize = 50;
                if self.peer_ips.len() >= MAX_PEERS {
                    let new_subnet = self.peer_subnets.get(&peer_id).cloned().unwrap_or_default();
                    let subnet_counts = self.count_subnets();
                    let new_is_unique = !subnet_counts.contains_key(&new_subnet);

                    if new_is_unique {
                        // Find a peer from a duplicate subnet to disconnect.
                        if let Some(victim) = self.find_duplicate_subnet_peer(&peer_id) {
                            info!("Geographic diversity: disconnecting {} (duplicate subnet) to keep unique-subnet peer {}", victim, peer_id);
                            let _ = self.network.swarm.disconnect_peer_id(victim);
                        } else {
                            info!("Connection limit reached ({}) — disconnecting new peer {}", MAX_PEERS, peer_id);
                            let _ = self.network.swarm.disconnect_peer_id(peer_id);
                            return;
                        }
                    } else {
                        info!("Connection limit reached ({}) — disconnecting new peer {}", MAX_PEERS, peer_id);
                        let _ = self.network.swarm.disconnect_peer_id(peer_id);
                        return;
                    }
                }

                info!("Connected to peer: {} at {}", peer_id, addr_str);
                // Initialize peer reputation score.
                self.peer_scores.entry(peer_id).or_insert(100);

                // QC-009 attestation: challenge this peer to prove it controls an
                // eligible validator key. Runs once per peer (past the dual-stack
                // dedup return and the MAX_PEERS/diversity gate, so only for a peer
                // we are keeping). The nonce binds this exact (challenger,
                // responder) pair; a copied/relayed answer is worthless. The kill
                // lever (formation-test only) skips the send, leaving the peer
                // unbound so the liveness floor degrades to unbound counting.
                if !self.attest_disabled {
                    let nonce: [u8; 32] = rand::random();
                    self.pending_attest.insert(peer_id, nonce);
                    let req = commputer_network::attest_protocol::AttestRequest::Challenge {
                        chain_id: commputer_core::genesis::TESTNET_CHAIN_ID.to_string(),
                        challenger_peer: self.network.local_peer_id.to_bytes(),
                        responder_peer: peer_id.to_bytes(),
                        nonce,
                    };
                    self.network
                        .swarm
                        .behaviour_mut()
                        .attest
                        .send_request(&peer_id, req);
                }
            }
            SwarmEvent::Behaviour(CommpBehaviourEvent::Identify(
                libp2p::identify::Event::Received { peer_id, info, .. }
            )) => {
                // Check protocol compatibility.
                if !info.protocol_version.starts_with("/commputer/") {
                    warn!(
                        "Peer {} runs incompatible protocol '{}' — disconnecting",
                        peer_id, info.protocol_version
                    );
                    let _ = self.network.swarm.disconnect_peer_id(peer_id);
                } else {
                    debug!("Peer {} identified: protocol={}, agent={}",
                        peer_id, info.protocol_version, info.agent_version);

                    // Item 20: Genesis hash verification — check agent_version contains our genesis hash.
                    // Agent version format: "commputer/<version>/<genesis_hash_hex_prefix>"
                    if let Some(genesis_block) = self.state.blocks.get_by_height(0) {
                        let our_genesis_hex = hex::encode(&genesis_block.hash().0[..8]);
                        let agent_has_genesis = info.agent_version.contains(&our_genesis_hex);
                        if info.agent_version.contains("commputer/") && !agent_has_genesis
                            && !info.agent_version.contains("unknown")
                        {
                            warn!(
                                "Peer {} has different genesis hash (agent: {}) — disconnecting",
                                peer_id, info.agent_version
                            );
                            let _ = self.network.swarm.disconnect_peer_id(peer_id);
                            return;
                        }
                    }

                    // Feature 169: Add the peer's listen addresses to Kademlia for discovery.
                    for addr in &info.listen_addrs {
                        self.network.swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                    }

                    // Feature 167: NAT detection via observed_addr.
                    {
                        let observed_str = info.observed_addr.to_string();
                        let observed_ip = extract_ip_from_multiaddr(&observed_str);

                        // Check if our observed IP differs from our listening addresses.
                        if let Some(ref obs_ip) = observed_ip
                            && self.observed_external_addr.is_none() {
                                self.observed_external_addr = Some(obs_ip.clone());
                                // Check if it looks like NAT (private vs public IP mismatch).
                                let is_private = is_private_ip(obs_ip);
                                if is_private {
                                    warn!("NAT detected: observed address {} appears to be behind NAT", obs_ip);
                                    warn!("Peers may not be able to connect to you directly");
                                    warn!("Consider using --relay flag or configuring port forwarding");
                                } else {
                                    info!("External address observed: {}", obs_ip);
                                }
                            }
                    }
                }
            }
            // Feature 166: Handle Kademlia events for peer discovery.
            SwarmEvent::Behaviour(CommpBehaviourEvent::Kademlia(kad_event)) => {
                use libp2p::kad;
                match kad_event {
                    kad::Event::OutboundQueryProgressed { id, result, .. } => {
                        match result {
                            // R1 (Phase B): DA provider discovery. Tag each PeerId to a reversible
                            // ProviderId, remember the PeerId so a follow-up FetchChunk can dial it,
                            // and fulfil the loop's stashed FindProviders reply.
                            kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders { providers, .. })) => {
                                if let Some(reply) = self.pending_find.remove(&id) {
                                    let mut out = Vec::with_capacity(providers.len());
                                    for peer in providers {
                                        let tag = Self::da_provider_tag(&peer);
                                        self.da_provider_ids.insert(tag, peer);
                                        out.push(commputer_da::params::ProviderId(tag));
                                    }
                                    let _ = reply.send(out);
                                }
                            }
                            kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. })) => {
                                if let Some(reply) = self.pending_find.remove(&id) {
                                    let _ = reply.send(Vec::new());
                                }
                            }
                            kad::QueryResult::GetProviders(Err(_)) => {
                                self.pending_find.remove(&id); // → the loop's bridge Abstains
                            }
                            kad::QueryResult::Bootstrap(Ok(result)) => {
                                debug!("Kademlia bootstrap progress: {} remaining peers", result.num_remaining);
                            }
                            kad::QueryResult::Bootstrap(Err(e)) => {
                                debug!("Kademlia bootstrap error: {:?}", e);
                            }
                            kad::QueryResult::GetClosestPeers(Ok(result)) => {
                                debug!("Kademlia found {} closest peers", result.peers.len());
                                for peer_info in &result.peers {
                                    for addr in &peer_info.addrs {
                                        debug!("Discovered peer via Kademlia: {} at {}", peer_info.peer_id, addr);
                                        // Try to connect to discovered peer.
                                        if !self.peer_ips.contains_key(&peer_info.peer_id)
                                            && !self.banned_peers.contains(&peer_info.peer_id)
                                            && peer_info.peer_id != self.network.local_peer_id
                                        {
                                            let _ = self.network.swarm.dial(addr.clone());
                                        }
                                    }
                                }
                            }
                            _ => {
                                debug!("Kademlia query result: {:?}", result);
                            }
                        }
                    }
                    kad::Event::RoutingUpdated { peer, addresses, .. } => {
                        debug!("Kademlia routing updated for peer {} ({} addresses)", peer, addresses.len());
                    }
                    _ => {}
                }
            }
            // Relay client events — log relay connections
            SwarmEvent::Behaviour(CommpBehaviourEvent::RelayClient(event)) => {
                debug!("Relay client event: {:?}", event);
            }
            // DCUtR hole-punching events
            SwarmEvent::Behaviour(CommpBehaviourEvent::Dcutr(event)) => {
                info!("DCUtR hole-punch event: {:?}", event);
            }
            // UPnP port mapping events
            SwarmEvent::Behaviour(CommpBehaviourEvent::Upnp(event)) => {
                match event {
                    libp2p::upnp::Event::NewExternalAddr(addr) => {
                        info!("UPnP: mapped external address {}", addr);
                    }
                    libp2p::upnp::Event::GatewayNotFound => {
                        debug!("UPnP: no gateway found (normal behind VPN)");
                    }
                    libp2p::upnp::Event::NonRoutableGateway => {
                        debug!("UPnP: gateway is not routable");
                    }
                    libp2p::upnp::Event::ExpiredExternalAddr(addr) => {
                        debug!("UPnP: external address expired: {}", addr);
                    }
                }
            }
            // Sync protocol — direct peer-to-peer block download.
            SwarmEvent::Behaviour(CommpBehaviourEvent::Sync(event)) => {
                use libp2p::request_response::{Event as RrEvent, Message as RrMessage};
                use commputer_network::sync_protocol::{SyncRequest, SyncResponse};
                match event {
                    RrEvent::Message { peer, message } => {
                        match message {
                            RrMessage::Request { request, channel, .. } => {
                                // Peer is requesting blocks from us.
                                // [27]/E6/E9: gate serving with the per-peer sync rate
                                // limiter. E9 (P7): key on peer_bucket_tagged over the
                                // FULL PeerId (the old bytes[..8] fold exposed ~2 key
                                // bytes = grindable). E6: GetBlock (tag 0) and GetBlocks
                                // (tag 1) use SEPARATE buckets so batch sync is never
                                // starved by GetBlock noise. Over-limit → cheap empty
                                // response, never a ban (a syncing peer is not hostile).
                                match request {
                                    SyncRequest::GetBlock { height } => {
                                        let resp = if self.sync_rate_limiter.check(commputer::peer_hash::peer_bucket_tagged(&peer, 0)) {
                                            let block_bytes = self.state.blocks.get_by_height(height)
                                                .and_then(|b| serde_json::to_vec(b).ok());
                                            SyncResponse::Block(block_bytes)
                                        } else {
                                            SyncResponse::Block(None)
                                        };
                                        let _ = self.network.swarm.behaviour_mut().sync
                                            .send_response(channel, resp);
                                    }
                                    SyncRequest::GetBlocks { start, end } => {
                                        let resp = if self.sync_rate_limiter.check(commputer::peer_hash::peer_bucket_tagged(&peer, 1)) {
                                            let mut blocks = Vec::new();
                                            // [28]: saturating_add — `start` is attacker-
                                            // controlled; `start + 100` overflow-panics in debug.
                                            for h in start..=end.min(start.saturating_add(100)) {
                                                if let Some(b) = self.state.blocks.get_by_height(h) {
                                                    if let Ok(data) = serde_json::to_vec(b) {
                                                        blocks.push(data);
                                                    }
                                                }
                                            }
                                            SyncResponse::Blocks(blocks)
                                        } else {
                                            SyncResponse::Blocks(Vec::new())
                                        };
                                        let _ = self.network.swarm.behaviour_mut().sync
                                            .send_response(channel, resp);
                                    }
                                    SyncRequest::GetHeight => {
                                        // Ungated: single scalar, used by the sync height-probe handshake.
                                        let resp = SyncResponse::Height(self.state.blocks.height());
                                        let _ = self.network.swarm.behaviour_mut().sync
                                            .send_response(channel, resp);
                                    }
                                }
                            }
                            RrMessage::Response { response, .. } => {
                                // We received blocks from a peer.
                                match response {
                                    SyncResponse::Block(Some(data)) => {
                                        if let Ok(block) = serde_json::from_slice::<commputer_core::block::Block>(&data) {
                                            // SECURITY(net-height §0): do NOT raise network_height from an
                                            // unvalidated synced-block height field. apply_synced_block
                                            // validates; the sync target advances only from GetHeight
                                            // replies (below) and validated consensus blocks.
                                            self.apply_synced_block(block, peer);
                                        }
                                    }
                                    SyncResponse::Blocks(blocks) => {
                                        for data in blocks {
                                            if let Ok(block) = serde_json::from_slice::<commputer_core::block::Block>(&data) {
                                                self.apply_synced_block(block, peer);
                                            }
                                        }
                                    }
                                    SyncResponse::Height(h) => {
                                        // net-height §0: a Height reply to our own GetHeight probe is a
                                        // trusted channel but the value is still self-reported — clamp to
                                        // tip + MAX_SYNC_WINDOW so one peer cannot pin an unreachable target.
                                        self.advance_network_height(h);
                                        // P3: feed the decay tracker with this authenticated sample.
                                        self.node_state.record_peer_height(commputer::peer_hash::peer_bucket(&peer), h);
                                        // Feed into sync state machine for height collection.
                                        self.sync_machine.record_height(h);
                                    }
                                    SyncResponse::Block(None) => {}
                                }
                            }
                        }
                    }
                    RrEvent::OutboundFailure { peer, error, .. } => {
                        debug!("Sync request to {} failed: {}", peer, error);
                    }
                    RrEvent::InboundFailure { peer, error, .. } => {
                        debug!("Sync response to {} failed: {}", peer, error);
                    }
                    RrEvent::ResponseSent { .. } => {}
                }
            }
            // === QC-009 peer->validator attestation (/commputer/attest/1) ===
            SwarmEvent::Behaviour(CommpBehaviourEvent::Attest(event)) => {
                use libp2p::request_response::{Event as RrEvent, Message as RrMessage};
                use commputer_network::attest_protocol::{AttestRequest, AttestResponse};
                match event {
                    RrEvent::Message { peer, message } => match message {
                        RrMessage::Request { request, channel, .. } => {
                            let AttestRequest::Challenge {
                                chain_id,
                                challenger_peer,
                                responder_peer,
                                nonce,
                            } = request;
                            // SERVE side. Mutual gate (constraint 3): answer only a
                            // peer we ourselves connected to and challenged (in
                            // pending_attest or already bound), never an
                            // unconnected/unchallenged stranger. Anti-relay: the
                            // challenge must name US as responder and the requesting
                            // peer as challenger (a relayed challenge naming a
                            // different challenger is refused), and the chain id must
                            // match. Only then do we sign with our validator key.
                            let we_challenged = self.pending_attest.contains_key(&peer)
                                || self.attested_peers.contains_key(&peer);
                            let names_us = responder_peer == self.network.local_peer_id.to_bytes();
                            let names_caller = challenger_peer == peer.to_bytes();
                            let chain_ok = chain_id == commputer_core::genesis::TESTNET_CHAIN_ID;
                            let response = if we_challenged && names_us && names_caller && chain_ok {
                                let sig = self
                                    .wallet
                                    .sign(&commputer_core::attest::build_attest_bytes(
                                        &chain_id,
                                        &responder_peer,
                                        &challenger_peer,
                                        &nonce,
                                    ))
                                    .to_bytes()
                                    .to_vec();
                                AttestResponse::Proof {
                                    pubkey: self.wallet.public_key().to_bytes().to_vec(),
                                    sig,
                                }
                            } else {
                                AttestResponse::Decline
                            };
                            let _ = self
                                .network
                                .swarm
                                .behaviour_mut()
                                .attest
                                .send_response(channel, response);
                        }
                        RrMessage::Response { response, .. } => {
                            // CHALLENGER side. Verify the proof against the nonce we
                            // issued THIS peer, binding PeerId->Address. Eligibility
                            // is NOT checked here (constraint 5) — only at vote intake
                            // — so a peer that attests before its ValidatorRegister
                            // applies flips ineligible->eligible with no re-handshake.
                            if let AttestResponse::Proof { pubkey, sig } = response {
                                if let Some(nonce) = self.pending_attest.get(&peer).copied() {
                                    if let Some(addr) = commputer_core::attest::verify_attestation(
                                        &pubkey,
                                        commputer_core::genesis::TESTNET_CHAIN_ID,
                                        &peer.to_bytes(),
                                        &self.network.local_peer_id.to_bytes(),
                                        &nonce,
                                        &sig,
                                    ) {
                                        debug!("Attestation OK: peer {} -> validator {}", peer, addr);
                                        self.attested_peers.insert(peer, addr);
                                    } else {
                                        debug!("Attestation from {} failed verification", peer);
                                    }
                                    self.pending_attest.remove(&peer);
                                }
                            }
                        }
                    },
                    RrEvent::OutboundFailure { peer, error, .. } => {
                        // A peer on an older binary (no /commputer/attest/1) yields
                        // this; it simply never binds and the liveness floor carries
                        // it during a mixed-version rollout.
                        debug!("Attest challenge to {} failed: {}", peer, error);
                        self.pending_attest.remove(&peer);
                    }
                    RrEvent::InboundFailure { peer, error, .. } => {
                        debug!("Attest response to {} failed: {}", peer, error);
                    }
                    RrEvent::ResponseSent { .. } => {}
                }
            }
            // === Track-2 DA request-response protocol (/commputer/da/1) ===
            SwarmEvent::Behaviour(CommpBehaviourEvent::Da(event)) => {
                use libp2p::request_response::{Event as RrEvent, Message as RrMessage};
                use commputer_network::da_protocol::{DaRequest, DaResponse};
                match event {
                    RrEvent::Message { peer, message } => match message {
                        RrMessage::Request { request, channel, .. } => {
                            let DaRequest::GetChunk { chunk_hash } = request;
                            // P8: gate the inbound serve on the sync limiter (distinct bucket tag 2)
                            // so a GetChunk flood can't pin the swarm thread / starve block sync.
                            let chunk = if self
                                .sync_rate_limiter
                                .check(commputer::peer_hash::peer_bucket_tagged(&peer, 2))
                            {
                                self.da_store.as_ref().and_then(|s| s.get(chunk_hash).ok().flatten())
                            } else {
                                None
                            };
                            let _ = self
                                .network
                                .swarm
                                .behaviour_mut()
                                .da
                                .send_response(channel, DaResponse::Chunk(chunk));
                        }
                        RrMessage::Response { request_id, response } => {
                            if let Some(reply) = self.pending_fetch.remove(&request_id) {
                                let DaResponse::Chunk(opt) = response;
                                let out = opt.and_then(|c| {
                                    commputer::da_publisher::deserialize_merkle_path(&c.merkle_path)
                                        .map(|path| (c.bytes, path))
                                });
                                let _ = reply.send(out);
                            }
                        }
                    },
                    RrEvent::OutboundFailure { request_id, .. } => {
                        self.pending_fetch.remove(&request_id); // → the loop's bridge Abstains
                    }
                    RrEvent::InboundFailure { .. } | RrEvent::ResponseSent { .. } => {}
                }
            }
            // === Consensus request-response protocol ===
            // Direct block proposals and votes — replaces gossipsub for consensus.
            SwarmEvent::Behaviour(CommpBehaviourEvent::Consensus(event)) => {
                use libp2p::request_response::{Event as RrEvent, Message as RrMessage};
                use commputer_network::consensus_protocol::{ConsensusRequest, ConsensusResponse};
                match event {
                    RrEvent::Message { peer, message } => {
                        match message {
                            RrMessage::Request { request, channel, .. } => {
                                match request {
                                    ConsensusRequest::BlockProposal { block_bytes, height } => {
                                        info!("Received BlockProposal at height {} from {}", height, peer);
                                        // Rate limit: reject if this peer is spamming.
                                        // SECURITY(E9/[20]): full-PeerId bucket key (was ~16-bit grindable).
                                        if !self.consensus_rate_limiter.check(commputer::peer_hash::peer_bucket(&peer), height) {
                                            debug!("Rate limited consensus request from {} at height {}", peer, height);
                                            let _ = self.network.swarm.behaviour_mut().consensus.send_response(
                                                channel, ConsensusResponse::NotReady { height, tip: self.state.blocks.height() });
                                            return;
                                        }
                                        if let Ok(block) = serde_json::from_slice::<commputer_core::block::Block>(&block_bytes) {
                                            let hash = block.hash();
                                            // SECURITY(net-height §0): removed the pre-validation raise AND
                                            // the direct node_state.set_network_height — advance ONLY after
                                            // full block validation, clamped to tip + MAX_SYNC_WINDOW.
                                            if !self.state.blocks.contains(&hash) && self.validate_block_from_peer(&block, peer) {
                                                self.advance_network_height(height);
                                                self.node_state.record_peer_height(commputer::peer_hash::peer_bucket(&peer), height);
                                                self.consensus.add_candidate(block);
                                                self.consensus.try_finalize_round(height, self.peer_ips.len(), self.tip_hash());
                                                self.try_apply_finalized(height);
                                            }
                                            // VOTE-HEIGHT DISCIPLINE (alpha.6).
                                            // (1) height already applied: echo the applied hash — this is
                                            //     the catch-up path that helps a lagging proposer converge.
                                            // (2) exactly our tip+1, and we are Active: endorse ONLY a
                                            //     candidate that builds on the chain we hold.
                                            // (3) anything above tip+1: we cannot have validated it, so
                                            //     NotReady + a bounded fetch of the gap. Voting here was
                                            //     the rubber-stamp that let a producer finalize alone.
                                            let our_tip = self.state.blocks.height();
                                            let response = if let Some(applied) = self.state.blocks.get_by_height(height) {
                                                ConsensusResponse::Vote {
                                                    height,
                                                    preference: applied.hash().0,
                                                    accept: true,
                                                }
                                            } else if height == our_tip + 1 && self.node_state.is_active() {
                                                let tip_hash = self.state.blocks.latest()
                                                    .map(|b| b.hash())
                                                    .unwrap_or(BlockHash::GENESIS);
                                                // Pass the SCHEDULE CYCLE so the vote ranks
                                                // candidates by their producer's view offset
                                                // before the (grindable) block hash.
                                                let vote_validators = self.consensus_cycle();
                                                match self.consensus.query_votable_preference(height, tip_hash, &vote_validators) {
                                                    Some(pref) => ConsensusResponse::Vote {
                                                        height,
                                                        preference: pref.0,
                                                        accept: true,
                                                    },
                                                    None => {
                                                        debug!(
                                                            "VOTE-REFUSE h={} our_tip={} tip={} pref={:?} candidates={:?}",
                                                            height, our_tip, tip_hash,
                                                            self.consensus.query_preference(height),
                                                            self.consensus.candidate_parents(height)
                                                        );
                                                        ConsensusResponse::NotReady { height, tip: our_tip }
                                                    }
                                                }
                                            } else {
                                                // Pull the immediate next block ONLY when we are barely
                                                // behind (missing a single block), which is the case this
                                                // fetch exists for: it lets a node one block short become
                                                // votable again immediately.
                                                //
                                                // Deliberately NOT fired when far behind. This runs on
                                                // every proposal and every 500ms re-proposal, so a node
                                                // with a large gap generates several requests per second
                                                // that compete with the sync machine's bulk batches for
                                                // the same per-peer rate-limit budget — and starve them.
                                                // Live 2026-07-25: a rejoining node at height 0 with the
                                                // tip near 1800 produced 1417 rate-limit rejections in
                                                // four minutes and never synced a single block. Bulk
                                                // catch-up belongs to the sync machine alone.
                                                if height == our_tip + 2 {
                                                    self.request_block(our_tip + 1);
                                                }
                                                ConsensusResponse::NotReady { height, tip: our_tip }
                                            };
                                            let _ = self.network.swarm.behaviour_mut().consensus.send_response(channel, response);
                                        }
                                    }
                                    ConsensusRequest::VoteRequest { height, block_hash: _ } => {
                                        // SECURITY(E9/[20]): full-PeerId bucket key (was ~16-bit grindable).
                                        if !self.consensus_rate_limiter.check(commputer::peer_hash::peer_bucket(&peer), height) {
                                            let _ = self.network.swarm.behaviour_mut().consensus.send_response(
                                                channel, ConsensusResponse::NotReady { height, tip: self.state.blocks.height() });
                                            return;
                                        }
                                        // Same discipline as the BlockProposal path: echo an applied
                                        // height, endorse only a tip+1 candidate that extends our
                                        // chain, otherwise NotReady with our tip.
                                        let our_tip = self.state.blocks.height();
                                        let response = if let Some(applied) = self.state.blocks.get_by_height(height) {
                                            ConsensusResponse::Vote {
                                                height,
                                                preference: applied.hash().0,
                                                accept: true,
                                            }
                                        } else if let Some(pref) = self.consensus.query_preference(height) {
                                            // DISCIPLINE DISABLED — see the BlockProposal arm.
                                            ConsensusResponse::Vote {
                                                height,
                                                preference: pref.0,
                                                accept: true,
                                            }
                                        } else {
                                            ConsensusResponse::NotReady { height, tip: our_tip }
                                        };
                                        let _ = self.network.swarm.behaviour_mut().consensus.send_response(channel, response);
                                    }
                                }
                            }
                            RrMessage::Response { response, .. } => {
                                // We received a vote back from a peer we sent a proposal to.
                                match response {
                                    ConsensusResponse::Vote { height, preference, accept } => {
                                        info!("Received Vote from {} at height {} (accept={})", peer, height, accept);
                                        if accept {
                                            // QC-009 vote-path gate: count a remote vote only from a peer
                                            // that PROVED control of a validator eligible RIGHT NOW
                                            // (attestation binding + use-time eligibility), or — only when
                                            // nothing is bound anywhere past the grace window — from the
                                            // unbound fallback so a genuinely isolated node degrades to clamp
                                            // semantics instead of halting. An attacker's sockets cannot
                                            // attest and cannot unbind our honest peers, so their votes never
                                            // enter the k=3 sample. Bookkeeping (voted_peers / health) moves
                                            // INSIDE the gate so an attacker cannot steer proposal
                                            // re-targeting or partition health either.
                                            let eligible = self.consensus_validators();
                                            let bound_eligible = self
                                                .attested_peers
                                                .get(&peer)
                                                .is_some_and(|addr| eligible.contains(addr));
                                            if bound_eligible || self.unbound_fallback_active(&eligible) {
                                                self.consensus.record_peer_response(height, BlockHash(preference), peer);
                                                self.voted_peers.insert(peer);
                                                self.health_monitor.record_vote(commputer::peer_hash::peer_bucket(&peer));
                                            } else {
                                                debug!("Dropped unattested vote from {} at height {}", peer, height);
                                            }
                                        }
                                    }
                                    ConsensusResponse::NotReady { height, tip } => {
                                        // Damp the stall timer only for a peer that is
                                        // genuinely BEHIND us and therefore will catch
                                        // up. A peer level with or ahead of us is
                                        // refusing or forked, and waiting on it forever
                                        // is how the whole net froze at height 11 in
                                        // mutual NotReady politeness. tip == 0 means an
                                        // older peer that sends no tip: keep the
                                        // budgeted behaviour for those.
                                        let behind = tip > 0 && tip < self.state.blocks.height();
                                        let unknown_tip = tip == 0;
                                        if self.stall_start.is_some()
                                            && (behind || unknown_tip)
                                            && self.consensus.allow_notready_stall_reset(peer)
                                        {
                                            self.stall_start = None;
                                        }
                                        // Self-reported, same trust level as the GetHeight
                                        // replies already recorded; clamped downstream.
                                        if tip > 0 {
                                            self.node_state.record_peer_height(
                                                commputer::peer_hash::peer_bucket(&peer),
                                                tip,
                                            );
                                        }
                                        debug!("Received NotReady from {} at height {} (their tip {})", peer, height, tip);
                                    }
                                }
                            }
                        }
                    }
                    RrEvent::OutboundFailure { peer, error, .. } => {
                        debug!("Consensus request to {} failed: {}", peer, error);
                    }
                    RrEvent::InboundFailure { peer, error, .. } => {
                        debug!("Consensus response to {} failed: {}", peer, error);
                    }
                    RrEvent::ResponseSent { .. } => {}
                }
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                // Deduplicate: only clean up when all connections to this peer are gone.
                if let Some(count) = self.peer_connection_count.get_mut(&peer_id) {
                    *count = count.saturating_sub(1);
                    if *count > 0 {
                        debug!("Connection closed to peer {} ({} remaining)", peer_id, count);
                        return;
                    }
                    self.peer_connection_count.remove(&peer_id);
                }
                // All connections to this peer are gone. Clean up peer tracking.
                self.peer_ips.remove(&peer_id);
                // P3 (§2.2b): drop the disconnected peer's decay sample.
                self.node_state.forget_peer_height(commputer::peer_hash::peer_bucket(&peer_id));
                self.peer_subnets.remove(&peer_id);
                self.peer_rtts.remove(&peer_id);
                self.ping_timestamps.remove(&peer_id);
                self.peer_quality.remove(&peer_id);
                // Save validator addr before removing (for grace drain below).
                let validator_addr = self.peer_validators.remove(&peer_id);
                if let Some(ref addr) = validator_addr {
                    self.compliance.deregister_node(addr);
                }
                // QC-009 attestation is strictly per-connection: drop the binding
                // and any outstanding challenge. A reconnecting honest peer
                // re-challenges within one RTT; the grace clock (kept fresh by any
                // other bound peer) means the fallback window never opens while any
                // honest peer is reachable.
                self.attested_peers.remove(&peer_id);
                self.pending_attest.remove(&peer_id);
                self.peer_scores.remove(&peer_id);
                // Drain grace period for disconnected validators.
                if let Some(ref validator_addr) = validator_addr
                    && let Some(account) = self.state.accounts.get_mut(validator_addr) {
                        // Drain 1 epoch's worth of grace (3600s) on disconnect.
                        account.drain_grace(3600);
                        debug!("Drained grace for disconnected validator {}", validator_addr);
                    }
                info!("Disconnected from peer: {}", peer_id);
            }
            _ => {}
        }
    }

    /// Handle a consensus protocol message from a peer.
    #[allow(unreachable_patterns)]
    fn handle_consensus_message(&mut self, msg: ConsensusMessage, source: libp2p::PeerId) {
        match msg {
            ConsensusMessage::BlockCandidate { block } => {
                let hash = block.hash();
                let height = block.height();

                if self.state.blocks.contains(&hash) {
                    return; // Already finalized this block.
                }

                // Validate block before accepting as candidate.
                if !self.validate_block_from_peer(&block, source) {
                    return;
                }

                // SECURITY(net-height §0): advance the sync target ONLY after the
                // block passes full validation, clamped to tip + MAX_SYNC_WINDOW.
                self.advance_network_height(height);
                // P3: feed the decay tracker with this authenticated sample.
                self.node_state.record_peer_height(commputer::peer_hash::peer_bucket(&source), height);

                debug!("Received block candidate {} at height {}", hash, height);
                self.consensus.add_candidate(block);

                self.consensus.try_finalize_round(height, self.peer_ips.len(), self.tip_hash());
                self.try_apply_finalized(height);

                // Legacy compat: also respond with vote.
                if let Some(pref) = self.consensus.query_preference(height) {
                    let response = ConsensusMessage::VoteResponse {
                        height, preference: pref, round: 0,
                    };
                    self.publish_consensus_message(&response);
                }
            }
            ConsensusMessage::SnowballQuery { height, querier_preference: _, round, .. } => {
                // Legacy: respond or request block if we don't have it.
                // SECURITY(net-height §0): unauthenticated gossip query carrying NO
                // block — do NOT raise network_height from it at all (this is the
                // cheapest u64::MAX poison vector, one tiny TOPIC_CONSENSUS frame).
                if let Some(pref) = self.consensus.query_preference(height) {
                    let response = ConsensusMessage::VoteResponse {
                        height, preference: pref, round,
                    };
                    self.publish_consensus_message(&response);
                } else {
                    self.request_block(height);
                }
            }
            // E2 (finding [4]): legacy gossipsub vote arm DELETED at the alpha reset.
            // Signed+Strict gossip only proves the embedded key signed the frame, not
            // that the source is a connected/authenticated peer — one connection can
            // mint unlimited keypairs and fabricate a quorum. Request-response
            // (ConsensusResponse::Vote) is the sole authenticated vote path. Kept as an
            // inert no-op to preserve match exhaustiveness.
            ConsensusMessage::SnowballResponse { .. } => {}
            ConsensusMessage::BlockRequest { height } => {
                // Serve block from our chain if we have it.
                let block = self.state.blocks.get_by_height(height).cloned();
                let response = ConsensusMessage::BlockResponse {
                    block,
                    requested_height: height,
                };
                self.publish_consensus_message(&response);
            }
            ConsensusMessage::BlockResponse { block: Some(block), requested_height } => {
                debug!("Received block response for height {}", requested_height);
                let height = block.height();

                if height == requested_height && !self.state.blocks.contains(&block.hash()) {
                    // Slice 1 Hunk 1.6: this arm previously added an UNVALIDATED block
                    // to the candidate set (a producer-sig bypass) — validate first.
                    if !self.validate_block_from_peer(&block, source) {
                        return;
                    }
                    // SECURITY(net-height §0): advance only from the validated block, clamped.
                    self.advance_network_height(height);
                    self.node_state.record_peer_height(commputer::peer_hash::peer_bucket(&source), height);
                    self.consensus.add_candidate(block);
                    self.consensus.try_finalize_round(height, self.peer_ips.len(), self.tip_hash());
                    self.try_apply_finalized(height);
                }
            }
            ConsensusMessage::BlockResponse { block: None, requested_height } => {
                debug!("Peer doesn't have block at height {}", requested_height);
            }
            ConsensusMessage::LightClientRequest { tx_hash, block_height } => {
                // Serve merkle proof for the requested transaction.
                debug!("Light client request for tx {:?} in block {}", &tx_hash[..4], block_height);
            }
            ConsensusMessage::LightClientResponse { .. } => {
                // Handle light client response (future use).
            }
            ConsensusMessage::CheckpointCommitment { height, state_root, validator } => {
                // Feature 133: Record checkpoint vote from a validator.
                debug!("Checkpoint commitment from {} at height {}", validator, height);
                self.consensus.record_checkpoint_vote(height, validator, state_root);
            }
            ConsensusMessage::BlockProposal { block, round } => {
                let hash = block.hash();
                let height = block.height();

                if self.state.blocks.contains(&hash) {
                    return;
                }

                if !self.validate_block_from_peer(&block, source) {
                    return;
                }

                // SECURITY(net-height §0): advance only after full validation, clamped.
                self.advance_network_height(height);
                self.node_state.record_peer_height(commputer::peer_hash::peer_bucket(&source), height);

                debug!("Received block proposal {} at height {}", hash, height);
                self.consensus.add_candidate(block);
                self.consensus.try_finalize_round(height, self.peer_ips.len(), self.tip_hash());
                self.try_apply_finalized(height);

                // Immediately respond with our vote.
                if let Some(pref) = self.consensus.query_preference(height) {
                    let response = ConsensusMessage::VoteResponse {
                        height,
                        preference: pref,
                        round,
                    };
                    self.publish_consensus_message(&response);
                }
            }
            ConsensusMessage::BlockQuery { height, preference: _, round } => {
                // SECURITY(net-height §0): unauthenticated gossip query carrying NO
                // block — do NOT raise network_height from it.
                if let Some(pref) = self.consensus.query_preference(height) {
                    let response = ConsensusMessage::VoteResponse {
                        height,
                        preference: pref,
                        round,
                    };
                    self.publish_consensus_message(&response);
                } else {
                    // We don't have this block. Request it so we can vote next round.
                    self.request_block(height);
                }
            }
            // E2 (finding [4]): legacy gossipsub vote arm DELETED at the alpha reset
            // (see SnowballResponse above). Request-response is the sole authenticated
            // vote path. Inert no-op to preserve match exhaustiveness.
            ConsensusMessage::VoteResponse { .. } => {}
        }
    }

    /// SECURITY(net-height §0, findings [0]/[2]/[7]): raise the observed sync
    /// target ONLY from validated evidence, clamped to our tip + MAX_SYNC_WINDOW.
    /// This defangs the network_height-poisoning chain-halt: no single message can
    /// jump the monotonic target to an unreachable value, and honest far-behind
    /// nodes still converge as the ceiling rises with each applied batch. NEVER
    /// call this with an unvalidated attacker-supplied height field.
    fn advance_network_height(&mut self, candidate: u64) {
        let ceiling = self.state.blocks.height().saturating_add(commputer::peer_hash::MAX_SYNC_WINDOW);
        let clamped = candidate.min(ceiling);
        if clamped > self.network_height {
            self.network_height = clamped;
        }
    }

    /// Feature 132: Validate a block received from a peer in stages:
    /// Stage 1: Header checks (protocol version, height, timestamp, size)
    /// Stage 2: Merkle root verification
    /// Stage 3: Transaction signature verification
    fn validate_block_from_peer(&mut self, block: &Block, source: libp2p::PeerId) -> bool {
        // === Stage 1: Header checks ===

        // CHAIN ID. Apply rejects a foreign chain_id (storage/src/state.rs
        // apply_block), but this gate did not check it — so a foreign-chain
        // block could be voted on and FINALIZED, and only then fail to apply.
        // The round is consumed by then, so that height can never be
        // re-finalized: one such block from any peer stalls every node that
        // votes for it. Reject before it can ever win a round.
        if !block.header.chain_id.is_empty()
            && block.header.chain_id != commputer_core::genesis::TESTNET_CHAIN_ID
        {
            warn!(
                "Rejected block from {}: foreign chain_id {:?} (expected {:?})",
                source, block.header.chain_id, commputer_core::genesis::TESTNET_CHAIN_ID
            );
            self.adjust_peer_score(source, -20);
            return false;
        }

        // Feature 123: Protocol version check.
        // Item 1: Don't ban for version mismatch — peer may be running older software.
        if block.header.protocol_version != commputer_core::block::CURRENT_PROTOCOL_VERSION {
            warn!("Rejected block from {}: incompatible protocol version {} (expected {})",
                source, block.header.protocol_version,
                commputer_core::block::CURRENT_PROTOCOL_VERSION);
            self.adjust_peer_score(source, -5);
            return false;
        }

        // Check block size limits.
        // Item 1: Don't ban for oversized blocks — could be config mismatch.
        if !block.within_size_limits() {
            warn!("Rejected oversized block from {}", source);
            self.adjust_peer_score(source, -20);
            return false;
        }

        // Timestamp validation: reject blocks >30s in the future.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if block.header.timestamp > now + 30 {
            warn!("Rejected block from {}: timestamp too far in future ({} vs now {})",
                source, block.header.timestamp, now);
            return false; // Don't ban — could be clock skew.
        }

        // Timestamp must be close to parent block timestamp (no going far backward).
        // Allow 5 seconds of clock skew — desktop nodes won't have perfect NTP.
        // Compare against the block's actual parent (by parent_hash), not our local
        // chain tip. During sync or fork choice, the incoming block's parent may be
        // at a different height than our tip.
        if let Some(parent) = self.state.blocks.get(&block.header.parent_hash)
            && block.header.timestamp + 5 < parent.header.timestamp {
                warn!("Rejected block from {}: timestamp too far before parent ({} < {} - 5s tolerance)",
                    source, block.header.timestamp, parent.header.timestamp);
                return false;
            }

        // === Stage 1b: Leader election validation ===
        // Warn but don't reject for now. The validator set takes time to
        // converge across nodes, and strict rejection causes banning during
        // bootstrap. Enable strict rejection once the network is stable.
        // TODO: re-enable rejection after mainnet stabilizes
        // Same derivation as the production side — both sides of the leader
        // check must produce the identical schedule (see consensus_cycle).
        //
        // ⚠ EPOCH CAVEAT, harmless only while this check is NON-ENFORCING.
        // `consensus_cycle()` returns the schedule for the CURRENT TIP's epoch,
        // not for `block.height()`'s epoch. Validating a block from an older
        // epoch (during sync, or a deep reorg) therefore judges it against the
        // wrong schedule. Today that can only mis-word a debug! line, because
        // the branch below logs and accepts regardless. BEFORE re-enabling
        // rejection here (see the TODO above), this MUST switch to the schedule
        // for `epoch_of(block.height())` or a syncing node will reject valid
        // history. The pre-flip code had the same flaw — it used the live
        // mutable set for every height — so this is not a regression, but it is
        // now a documented precondition rather than a latent surprise.
        let validators: Vec<Address> = self.consensus_cycle();
        if commputer::leader::distinct_validator_count(&validators) >= 2 {
            let seconds_since_parent = if let Some(parent) = self.state.blocks.get(&block.header.parent_hash) {
                block.header.timestamp.saturating_sub(parent.header.timestamp)
            } else {
                0
            };
            if !commputer::leader::cycle_is_valid_leader(
                &validators,
                block.height(),
                &block.header.producer,
                seconds_since_parent,
            ) {
                debug!("Leader mismatch: producer {} not expected leader for height {} ({}s since parent) — accepting anyway",
                    block.header.producer, block.height(), seconds_since_parent);
            }
        }

        // === Stage 1c: Producer-signature enforcement ===
        // E4 (finding [4] hardening): reject any peer-received block at height 0
        // outright — a node has its own genesis before it has peers, so no honest
        // peer sends one; without this, the strict-verifier genesis carve-out would
        // leave height 0 as the one unsigned-block injection point post-flip.
        if block.height() == 0 {
            self.adjust_peer_score(source, -20);
            return false;
        }
        // Post-reset every non-genesis block must carry a valid ed25519 signature
        // whose embedded public key hashes to the declared producer address.
        // (ENFORCE_PRODUCER_SIGNATURES flips true in core/block.rs at the reset.)
        if !block.verify_producer_signature() {
            self.ban_peer(source, "sent block with missing/invalid producer signature");
            return false;
        }

        // === Stage 2: Merkle root verification ===

        // Check merkle roots.
        if !block.verify_roots() {
            self.ban_peer(source, "sent block with invalid merkle roots");
            return false;
        }

        // === Stage 3: Transaction signature verification ===

        // Verify all transaction signatures in the block.
        // Protocol-issued transactions (MiningReward, MilestoneBurn) come from the zero
        // address and have no signature — skip verification for those.
        for tx in &block.transactions {
            if tx.from.is_zero() {
                continue; // Protocol-issued tx, no signature expected
            }
            if !tx.verify() {
                self.ban_peer(
                    source,
                    &format!(
                        "sent block containing transaction with invalid signature at height {}",
                        block.height()
                    ),
                );
                return false;
            }
        }

        // HALT-VECTOR GUARD: never vote for a block we cannot APPLY.
        if !self.block_is_votable_on_tip(block) {
            self.adjust_peer_score(source, -20);
            return false;
        }

        // Block passed all checks — reward the peer.
        self.adjust_peer_score(source, 1);
        true
    }

    /// The stake-weighted proposer schedule for the current epoch — LIVE as of
    /// the flip; previously shadow-only.
    ///
    /// Derived from the SNAPSHOT taken two epochs back, so every node computes an
    /// identical schedule without having to agree on the live, mutable validator
    /// set. Rebuilt only when the epoch changes: `build_schedule` materialises the
    /// whole cycle (up to MAX_CYCLE_LEN entries) and must not run per height.
    ///
    /// Still logs the digest on every epoch change. That log is what proved the
    /// flip safe before it was armed (all three nodes printed identical digests
    /// for 30 consecutive epochs) and it remains the cheapest possible detector:
    /// two nodes printing different digests for one epoch WOULD disagree about who
    /// may propose, and we see it in a log diff instead of as a fork.
    fn refresh_schedule(&mut self) {
        let height = self.state.blocks.height();
        let epoch = commputer::schedule_epoch::epoch_of(height);
        if self.shadow_schedule_epoch == Some(epoch) && self.schedule_cache.is_some() {
            return;
        }

        let snapshot_height = commputer::schedule_epoch::snapshot_height_for(epoch);
        let snapshot_epoch = snapshot_height / commputer::schedule_epoch::EPOCH_BLOCKS;
        let pool = self.state.epoch_validator_pool(snapshot_epoch);
        let fallback_used = pool.is_none();

        // No snapshot for that epoch (too early in the chain's life, pruned, or
        // memory-only): fall back to the live set so shadow mode still reports
        // something, and RECORD that it did — the digest commits to this flag,
        // so a node using the fallback can never compare equal to one using a
        // real snapshot. Silently substituting would make the instrument lie.
        let eligible: Vec<(Address, u64)> = match pool {
            Some(p) => p
                .into_iter()
                .filter(|(addr, bonded)| {
                    commputer::consensus_set::is_consensus_eligible(
                        true, // every entry in the pool was is_validator when recorded
                        *bonded,
                        commputer::consensus_set::MIN_CONSENSUS_BOND,
                        crate::testnet_genesis::pin_is_active(),
                        crate::testnet_genesis::is_pinned_validator(addr),
                    )
                })
                .collect(),
            None => {
                let mut live: Vec<(Address, u64)> = self
                    .consensus_validators()
                    .into_iter()
                    .map(|a| {
                        let bonded = self.state.bonded_of(&a);
                        (a, bonded)
                    })
                    .collect();
                live.sort_unstable_by_key(|(a, _)| a.0);
                live
            }
        };

        let sched = commputer::schedule_epoch::build_schedule(
            epoch,
            snapshot_height,
            fallback_used,
            &eligible,
        );
        info!(
            "shadow schedule: epoch={} snapshot_height={} validators={} cycle_len={} \
             fallback={} digest={}",
            sched.epoch,
            sched.snapshot_height,
            sched.validators.len(),
            sched.cycle.len(),
            sched.fallback_used,
            commputer::schedule_epoch::digest_hex(&sched.digest),
        );
        self.shadow_schedule_epoch = Some(epoch);
        self.schedule_cache = Some(sched);
    }

    /// THE proposer cycle every consensus decision must use — production,
    /// validation, the vote we answer peers with, and our own ballot.
    ///
    /// A cycle is a list in which each validator appears once per unit of weight,
    /// so a plain sorted set IS a cycle in which everyone has weight 1. That is
    /// why the degraded path below is safe rather than merely convenient: if no
    /// schedule can be derived (too early in the chain's life, or a pruned
    /// snapshot) this returns the live sorted consensus set, which the
    /// cycle-aware leader functions interpret as exactly the round-robin the
    /// chain used before the flip. There is no third behaviour to get wrong.
    ///
    /// ⚠ ALL FOUR CONSENSUS DECISION SITES MUST CALL THIS. Moving three of the
    /// four leaves a node disagreeing with itself about who may produce — it
    /// would validate a block it would never have proposed, or refuse to vote for
    /// one it just produced.
    fn consensus_cycle(&mut self) -> Vec<Address> {
        self.refresh_schedule();
        let cycle = self
            .schedule_cache
            .as_ref()
            .map(|s| s.cycle.clone())
            .unwrap_or_default();

        // NEVER-DEGENERATE FLOOR. `consensus_validators()` is deliberately TOTAL
        // and never empty, because a set with no legal producer is an
        // unrecoverable halt: no address may build the very block that would fix
        // it. A schedule derived from a snapshot has no such guarantee — a
        // snapshot that is short, stale, or filtered down to a single eligible
        // address would yield a cycle with fewer than 2 DISTINCT validators.
        //
        // That is not merely degraded, it is a deadlock: the production gate
        // needs >= 2 distinct validators, so every non-seed node stops
        // producing; the tip therefore stops advancing; `refresh_schedule` keys
        // the epoch off the tip height, so the epoch can never roll and the bad
        // schedule can never be replaced. It cannot self-heal.
        //
        // So the floor is inherited rather than assumed: if the schedule cannot
        // name at least two distinct validators but the live set can, use the
        // live set. At equal stakes both derivations are the same list anyway,
        // so this changes nothing on a healthy chain.
        if commputer::leader::distinct_validator_count(&cycle) >= 2 {
            return cycle;
        }
        let live = self.consensus_validators();
        if commputer::leader::distinct_validator_count(&live)
            > commputer::leader::distinct_validator_count(&cycle)
        {
            warn!(
                "schedule names {} distinct validator(s); falling back to the live set of {} \
                 to keep leader election alive",
                commputer::leader::distinct_validator_count(&cycle),
                commputer::leader::distinct_validator_count(&live),
            );
            live
        } else {
            cycle
        }
    }

    /// The addresses eligible to take part in consensus, derived from committed
    /// chain state. ONE derivation, used by BOTH the validation side and the
    /// production side — if the two sides disagree about the set they disagree
    /// about who the valid leader is, so this must never be duplicated.
    ///
    /// Registration is free and automatic, so it cannot gate participation on
    /// its own: an attacker mints identities at zero cost, floods candidates,
    /// and grinds headers until one wins every round. The real gate is BONDED
    /// STAKE (`MIN_CONSENSUS_BOND`), which cannot be minted — N Sybil
    /// identities cost N × 1 COMME of genuinely bonded, slashable balance. The
    /// alpha allowlist still applies while it is in force; once it is retired,
    /// stake alone decides, which is what makes opening the set safe.
    ///
    /// Sorted for determinism: every node must derive the SAME ORDER or the
    /// round-robin leader schedule differs between them. `accounts.iter()` is
    /// map iteration and its order is not guaranteed.
    fn consensus_validators(&self) -> Vec<Address> {
        let mut set: Vec<Address> = self
            .state
            .accounts
            .iter()
            .filter(|a| {
                commputer::consensus_set::is_consensus_eligible(
                    a.is_validator,
                    self.state.bonded_of(&a.address),
                    commputer::consensus_set::MIN_CONSENSUS_BOND,
                    crate::testnet_genesis::pin_is_active(),
                    crate::testnet_genesis::is_pinned_validator(&a.address),
                )
            })
            .map(|a| a.address)
            .collect();
        set.sort_unstable_by_key(|a| a.0);

        // TOTAL, never empty. An empty eligible set is an unrecoverable halt:
        // no address is a legal producer, so the chain cannot make the very
        // block that would fix it. This is reachable with no attacker at all —
        // a mass unbond, a slash cascade, or a bond-accounting bug. Cosmos:
        // "a chain cannot produce a block without a validator set."
        //
        // Fall back to every registered validator (ignoring the stake gate) and
        // say so loudly. A degraded set that still produces blocks is strictly
        // better than a halt that needs human hands, and the operator gets an
        // alarm rather than silence.
        if set.len() < commputer::consensus_set::MIN_CONSENSUS_SET {
            let mut fallback: Vec<Address> = self
                .state
                .accounts
                .iter()
                .filter(|a| a.is_validator)
                .map(|a| a.address)
                .collect();
            fallback.sort_unstable_by_key(|a| a.0);
            warn!(
                "consensus set below minimum ({} < {}) — falling back to all {} registered \
                 validators to avoid a halt; check bonding/slashing state",
                set.len(),
                commputer::consensus_set::MIN_CONSENSUS_SET,
                fallback.len()
            );
            return fallback;
        }
        set
    }

    /// The halt-vector guard, shared by EVERY path that turns a peer block into
    /// a votable candidate. Returns false only when the block builds on our own
    /// tip yet would NOT apply — the one case we must never vote for.
    ///
    /// Every structural check (signatures, roots, chain_id) answers "is this
    /// block well-formed?", never "would it apply on our chain?" Without the
    /// latter, a peer proposes a block that passes structure but fails apply (a
    /// dust transfer, a bad nonce, an over-spend), grinds its header until its
    /// hash is the lowest candidate so the deterministic vote picks it, and
    /// every honest node finalizes it. Finalizing CONSUMES the round, so apply
    /// then fails with only a warning and that height is stuck forever: one
    /// crafted block halts the whole network. This is the safety prerequisite
    /// for opening consensus beyond the founder set.
    ///
    /// It MUST guard both entry points, or the unguarded one is a full bypass:
    /// `validate_block_from_peer` (direct gossip) AND `process_orphans` (a
    /// block buffered while its parent was missing — validated then, when its
    /// parent was NOT our tip so this check was skipped, and later promoted to
    /// a candidate once the parent applies). The orphan path is exactly how the
    /// first version of this fix was bypassed.
    ///
    /// Only fires for a block on OUR tip carrying txs: a block on a parent we
    /// don't hold can't be trial-applied on our tip (vote-height discipline
    /// already refuses to endorse it, and sync fetches the real chain), and an
    /// empty block carries no failing tx — its only apply-failure path is the
    /// settlement pot invariants, which are content-independent and so cannot
    /// be an attacker-controlled grinding target. `would_txs_apply` is proven
    /// non-mutating (`would_txs_apply_is_non_mutating`), so this cannot corrupt
    /// state.
    fn block_is_votable_on_tip(&mut self, block: &Block) -> bool {
        let our_tip = self
            .state
            .blocks
            .latest()
            .map(|b| b.hash())
            .unwrap_or(commputer_core::block::BlockHash::GENESIS);
        if block.header.parent_hash != our_tip || block.transactions.is_empty() {
            return true;
        }
        if let Err(e) = self.state.would_txs_apply(block) {
            warn!(
                "Halt-vector guard: block {} at height {} builds on our tip but would not apply ({:?}) — refusing to vote",
                block.hash(), block.height(), e
            );
            return false;
        }
        true
    }

    /// A block received on the blocks topic (legacy path). Enter it as a candidate
    /// instead of applying directly.
    /// Features 127 (orphan pool), 128 (propagation metrics), 131 (duplicate detection).
    fn handle_received_block(&mut self, block: Block, source: libp2p::PeerId) {
        let hash = block.hash();
        let height = block.height();
        let producer = block.header.producer;

        // Refresh last-seen on block receipt.
        self.peer_last_seen.insert(source, std::time::Instant::now());

        if self.state.blocks.contains(&hash) {
            return; // Already have this block.
        }

        // SECURITY[24] + Slice-1 Hunk 1.7 (deconflicted): validate BEFORE any
        // bookkeeping insert or orphan-buffering. This (a) stops pre-validation
        // blocks from ever growing block_seen_times / producer_blocks / orphan_pool,
        // (b) means process_orphans no longer re-injects un-validated blocks
        // (candidate-entry bypass #2 closed), and (c) post-flip rejects
        // unsigned/forged blocks up front — incl. the E4 height-0 reject inside
        // validate_block_from_peer.
        if !self.validate_block_from_peer(&block, source) {
            return;
        }
        let applied_tip = self.state.blocks.height();

        // Feature 128: Record block propagation timing (validated blocks only).
        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let std::collections::hash_map::Entry::Vacant(e) = self.block_seen_times.entry(hash) {
            e.insert(now_ts);
            let propagation_delay_ms = now_ts.saturating_sub(block.header.timestamp) * 1000;
            if propagation_delay_ms > 0 {
                self.propagation_delays.push(propagation_delay_ms);
                debug!("Block {} propagation delay: {}ms", hash, propagation_delay_ms);
                // Log percentiles every 100 blocks.
                if self.propagation_delays.len().is_multiple_of(100) {
                    self.log_propagation_percentiles();
                }
            }
        }
        // SECURITY[24]: bound block_seen_times (see block_maps.rs).
        commputer::block_maps::prune_block_seen_times(&mut self.block_seen_times, applied_tip);

        // Feature 131: Duplicate block detection (equivocation).
        let producer_key = (producer, height);
        if let Some(existing_hash) = self.producer_blocks.get(&producer_key) {
            if *existing_hash != hash {
                warn!(
                    "DUPLICATE BLOCK: producer {} produced two blocks at height {} ({} and {})",
                    producer, height, existing_hash, hash
                );
                // Still process it — consensus will handle which one wins.
            }
        } else {
            self.producer_blocks.insert(producer_key, hash);
        }
        // SECURITY[24]: drop producer_blocks at/below applied tip, cap the rest.
        commputer::block_maps::prune_producer_blocks(&mut self.producer_blocks, applied_tip);

        // Feature 127: Check if parent exists. If not, add to orphan pool.
        if height > 0 && !self.state.blocks.contains(&block.header.parent_hash)
            && self.state.blocks.height() + 1 != height
        {
            debug!("Block {} at height {} is orphaned — parent {} not found", hash, height, block.header.parent_hash);
            // SECURITY[13]: per-parent (<=20) AND total (<=200) orphan caps (was
            // distinct-parent count only). Block already passed validation above.
            commputer::block_maps::bounded_orphan_insert(
                &mut self.orphan_pool,
                block.header.parent_hash,
                block,
            );
            return;
        }

        // Update last block seen time for view change (feature 130).
        self.last_block_seen_time = Some(std::time::Instant::now());

        debug!("Received block {} at height {} — entering as candidate", hash, height);
        self.consensus.add_candidate(block);

        // Attempt finalization (handles single-candidate fast-path).
        self.consensus.try_finalize_round(height, self.peer_ips.len(), self.tip_hash());
        self.try_apply_finalized(height);
    }

    /// Feature 128: Log p50/p90/p99 propagation delay percentiles.
    fn log_propagation_percentiles(&self) {
        if self.propagation_delays.is_empty() {
            return;
        }
        let mut sorted = self.propagation_delays.clone();
        sorted.sort();
        let len = sorted.len();
        let p50 = sorted[len / 2];
        let p90 = sorted[(len as f64 * 0.9) as usize];
        let p99 = sorted[(len as f64 * 0.99).min(len as f64 - 1.0) as usize];
        info!("Block propagation delay (n={}): p50={}ms, p90={}ms, p99={}ms", len, p50, p90, p99);
    }

    /// Feature 127: Check if any orphaned blocks can now be processed after
    /// a block has been applied at the given hash.
    fn process_orphans(&mut self, parent_hash: BlockHash) {
        if let Some(orphans) = self.orphan_pool.remove(&parent_hash) {
            for orphan in orphans {
                let hash = orphan.hash();
                let height = orphan.height();
                debug!("Processing orphan block {} at height {} (parent now available)", hash, height);
                // Re-run the halt-vector guard at PROMOTION. The orphan was
                // validated when buffered, but its parent was not our tip then,
                // so the trial-apply was skipped; now the parent has applied and
                // this orphan builds on our tip, so it must be trial-applied
                // before it becomes a votable candidate. Skipping this here was
                // a full bypass of the fix.
                if !self.block_is_votable_on_tip(&orphan) {
                    continue;
                }
                self.consensus.add_candidate(orphan);
                self.consensus.try_finalize_round(height, self.peer_ips.len(), self.tip_hash());
                self.try_apply_finalized(height);
            }
        }
    }

    /// Handle a transaction submitted via the RPC server: validate, add to mempool, broadcast.
    fn handle_rpc_transaction(&mut self, req: crate::rpc::RpcTxRequest) {
        let crate::rpc::RpcTxRequest { tx, reply } = req;

        // The verdict this returns is what /tx now reports to the submitter,
        // instead of the old "accepted the instant it was queued". On rejection
        // the caller gets the real reason; a fire-and-forget sender (reply None)
        // keeps the old log-only behavior.

        // IDEMPOTENT RESUBMIT: a tx ALREADY IN OUR MEMPOOL is not a failure —
        // it is already admitted. Report success so a client's retry after a
        // busy-node 202 does not read its queued tx as an error.
        //
        // Test the LIVE mempool, not the coarse seen-hash set: a tx admitted
        // then EVICTED (enforce_mempool_limit) or EXPIRED (1h sweep) is still
        // "seen" while present in neither the pool nor the chain, and must NOT
        // be reported accepted — that would drop it silently, the exact class
        // this change ends. The cheap O(1) seen-hash lookup GATES the O(pending)
        // scan: every pooled tx is also seen, so `seen` is a necessary
        // precondition, and a flood of fresh (unseen) txs never pays for the
        // scan — it fails fast here and meets the normal validation path.
        //
        // A seen-but-not-pooled tx falls through to validate_tx_for_mempool,
        // which rejects it as a duplicate with an explicit error (an honest 400,
        // not a false accept). It is NOT re-admitted until the epoch seen-clear;
        // making evict/expire prune seen so re-admission works is an alpha.7
        // follow-up, and a pre-existing behavior this change does not worsen.
        let tx_hash = tx.hash();
        if self.seen_tx_hashes.contains(&tx_hash)
            && self.pending_txs.iter().any(|t| t.hash() == tx_hash)
        {
            if let Some(reply) = reply {
                let _ = reply.send(Ok(()));
            }
            return;
        }

        if let Err(reason) = self.validate_tx_for_mempool(&tx) {
            if let Some(reply) = reply {
                let _ = reply.send(Err(reason.to_string()));
            } else {
                warn!("RPC transaction rejected: {}", reason);
            }
            return;
        }

        // ASK CONSENSUS, don't re-implement it. The checks above are a
        // hand-written mirror of apply's rules, and a hand-written mirror is
        // exactly what caused this project's worst bug class — it covered
        // Transfer and silently missed nine other TxKinds, each of which was
        // admitted here, gossiped, packed, and then discarded during block
        // production with no error to the sender. Trial-applying the tx closes
        // the whole class by construction: the gate cannot drift from apply,
        // because for this final check it IS apply.
        //
        // Enforces `gate_accepts(tx, S) => apply(tx, S).is_ok()`. The mempool
        // may be STRICTER than consensus, never looser — doubly so here,
        // because one bad tx fails the WHOLE block at apply.
        //
        // `would_tx_apply` is non-mutating (capture + rollback, same primitive
        // the halt-vector guard uses).
        if let Err(e) = self.state.would_tx_apply(&tx) {
            let reason = format!("would not apply: {e:?}");
            if let Some(reply) = reply {
                let _ = reply.send(Err(reason));
            } else {
                warn!("RPC transaction rejected: {}", reason);
            }
            return;
        }

        // tx_hash already computed above for the mempool-membership check.
        self.seen_tx_hashes.insert(tx_hash);

        // Broadcast on the transactions gossipsub topic.
        // Feature 173: Compress before publishing.
        if let Ok(data) = serde_json::to_vec(&tx) {
            let compressed = commputer_network::compress(&data);
            let topic = topics::tx_topic();
            if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, compressed) {
                warn!("Failed to broadcast RPC transaction: {}", e);
            }
        }

        info!("Broadcast transaction {} from RPC", hex::encode(tx_hash.0));

        // Feature 241: Broadcast tx event to WebSocket clients.
        self.broadcast_ws_event(&serde_json::json!({
            "type": "new_transaction",
            "tx_hash": hex::encode(tx_hash.0),
            "from": hex::encode(tx.from.0),
            "kind": format!("{:?}", tx.kind).chars().take(50).collect::<String>(),
        }));

        // Item 18-20: Wire compute job transactions into job pool.
        self.process_job_tx(&tx);

        // Item 51: Track when transaction was added for expiry.
        self.mempool_added_at.insert(tx_hash, std::time::Instant::now());
        self.pending_txs.push(tx);
        // Finding [12]: the RPC ingress path must honour the 5000-tx global cap too.
        // The gossip path already enforces it; without this, /tx admissions grow
        // pending_txs unboundedly (the per-account quota + fee floor bound single-key
        // floods, but not many-key RPC spam). Run before update_rpc_status so the
        // status reflects the post-eviction size.
        self.enforce_mempool_limit();
        self.update_rpc_status();

        // Admitted — tell the submitter it was genuinely accepted.
        if let Some(reply) = reply {
            let _ = reply.send(Ok(()));
        }
    }

    /// Validate a transaction for mempool admission: signature, nonce, dedup.
    fn validate_tx_for_mempool(&self, tx: &Transaction) -> Result<(), &'static str> {
        // Reject duplicates (already in mempool or finalized) before paying
        // for signature verification. Kept OUT of validate_tx_content: a tx
        // being re-admitted by requeue_lost_txs is legitimately in seen.
        if self.seen_tx_hashes.contains(&tx.hash()) {
            return Err("duplicate transaction");
        }
        self.validate_tx_content(tx)
    }

    /// Every mempool ingress check EXCEPT the seen-hash dedup. Shared by
    /// fresh ingress (RPC/gossip, via validate_tx_for_mempool) and by
    /// requeue_lost_txs, which re-admits txs that are already seen but must
    /// still pass every content gate — a losing candidate is attacker-
    /// suppliable (any peer's proposal is accepted as a candidate, and
    /// zero-from txs skip signature checks at candidate ingest), so requeue
    /// without these gates would launder unsigned/unfunded garbage into the
    /// pool.
    fn validate_tx_content(&self, tx: &Transaction) -> Result<(), &'static str> {
        self.validate_tx_content_inner(tx, true)
    }

    /// `append_nonce_rule = true` for fresh ingress (tx queues behind its
    /// sender's pooled txs); `false` for a restore, which only rejects a nonce
    /// already consumed on-chain. See `validate_tx_for_requeue`.
    fn validate_tx_content_inner(
        &self,
        tx: &Transaction,
        append_nonce_rule: bool,
    ) -> Result<(), &'static str> {
        if tx.from.0 == [0u8; 32] {
            return Err("null sender");
        }
        // CHEAP DETERMINISTIC REJECTS FIRST. Signature verification (~34us) and
        // the two O(mempool) scans below dominate this function, and the
        // requeue path re-runs it on attacker-suppliable txs, so anything
        // decidable from a single map lookup belongs ahead of them: a
        // zero-balance flood then costs ~1us per tx instead of ~60us.
        // Must reproduce the LATER gates' verdicts exactly, including their
        // exemptions — the faucet is exempt from fee-payability because
        // rejecting its tx at ingress strands the already-consumed
        // faucet_next_nonce and bricks dispensing until restart.
        {
            let on_chain = self.state.accounts.get(&tx.from);
            if tx.nonce < on_chain.map(|a| a.nonce).unwrap_or(0) {
                return Err("stale nonce");
            }
            let exempt = commputer::requeue_rules::payability_exempt(
                self.is_faucet_sender(tx),
                matches!(tx.kind, commputer_core::transaction::TxKind::ValidatorRegister { .. }),
            );
            if !exempt && on_chain.map(|a| a.balance.raw()).unwrap_or(0) < tx.fee {
                return Err("sender cannot cover fee");
            }
        }
        // Storage follow-up (findings [3]/[5]/[6] MultiSig, [11]/[22] StorageWill):
        // enforce the core structural caps at INGRESS so oversized MultiSig /
        // StorageWill / Batch payloads are rejected before they occupy mempool RAM
        // or reach the (now size-guarded) apply-side verify loops. Cheaper than
        // tx.verify(), so it runs first. Internal fee-exempt direct pushes bypass
        // this fn and are unaffected. Catch-all `_ => {}` in validate_kind_shape
        // leaves Transfer/ValidatorRegister/PoUW kinds untouched.
        tx.validate_shape()?;
        if !tx.verify() {
            return Err("signature verification failed");
        }
        // Feature 251: Validate memo length.
        if let Some(ref memo) = tx.memo
            && memo.len() > commputer_core::transaction::Transaction::MAX_MEMO_LENGTH {
                return Err("memo exceeds max length");
            }
        // Feature 260: Validate timelock.
        if let Some(timelock) = tx.timelock
            && self.state.blocks.height() < timelock {
                return Err("transaction timelocked");
            }
        // Minimum fee check (validator registration is fee-exempt).
        if tx.fee < commputer_core::transaction::MINIMUM_FEE
            && !matches!(tx.kind, commputer_core::transaction::TxKind::ValidatorRegister { .. })
        {
            return Err("fee below minimum");
        }

        // CLOSE THE ADMISSION GAP: enforce here what APPLY enforces.
        //
        // Any rule consensus checks at apply but the gate does not is a hole
        // that admits a tx the whole network accepts, gossips and packs into a
        // block — and then discards during production, with no receipt and no
        // error to the sender. That is not hypothetical: it silently killed
        // EVERY faucet dispense in this project's history and every
        // `commputer send` to a new address, for months, because both paid
        // MINIMUM_FEE while apply demands ACCOUNT_CREATION_FEE for a recipient
        // the chain has never seen.
        //
        // Fixing the two builders that tripped it only moved the hazard to the
        // next builder. The rules belong HERE, where every submitter meets
        // them — including anyone posting to /tx without our CLI.
        //
        // Mirrors storage/src/state.rs apply_transaction's Transfer arm. Both
        // sides read committed state only, so the verdicts agree.
        if let commputer_core::transaction::TxKind::Transfer { to, amount } = &tx.kind {
            if amount.raw() < commputer_core::transaction::DUST_LIMIT {
                return Err("amount below dust limit");
            }
            let recipient_exists = self.state.accounts.get(to).is_some();
            if !recipient_exists && tx.fee < commputer_core::transaction::ACCOUNT_CREATION_FEE {
                return Err("transfer to a new account requires the account-creation fee");
            }
        }
        // 1.2-MEMPOOL ingress pre-filter (C7): a PoUW verification-game tx (Commit/Reveal/CompleteJob/
        // ClaimJob) whose job_id has NEITHER an open lifecycle NOR a pending record is permanently
        // doomed at apply (post-flip: unknown job → whole-block reject), so reject it at ingress and
        // keep the mempool free of doomed PoUW txs. Read-only over committed ChainState → deterministic;
        // it never rejects legacy kinds (Transfer/Bond/…). A tx whose job EXISTS but is in the wrong
        // phase is admitted here and requeued later by `select_applicable_txs` (C3).
        match &tx.kind {
            commputer_core::transaction::TxKind::Commit { job_id, .. }
            | commputer_core::transaction::TxKind::Reveal { job_id, .. }
            | commputer_core::transaction::TxKind::CompleteJob { job_id, .. }
            | commputer_core::transaction::TxKind::ClaimJob { job_id } => {
                if !self.state.job_lifecycles.contains_key(job_id)
                    && !self.state.pending_jobs.contains_key(job_id)
                    && !self.state.escalation_rounds.contains_key(job_id)
                {
                    return Err("pouw tx references unknown job");
                }
            }
            _ => {}
        }
        // Nonce validation: must match expected next nonce for sender.
        // Account for pending txs already in mempool from the same sender.
        let on_chain_nonce = self.state.accounts
            .get(&tx.from)
            .map(|a| a.nonce)
            .unwrap_or(0);
        let pending_from_sender = self.pending_txs.iter()
            .filter(|ptx| ptx.from == tx.from)
            .count();

        // E3: the compiled faucet address is a trusted internal issuer whose nonce
        // is serialized in rpc.rs. Exempt it from BOTH the F-3 quota and the
        // fee-payability floor: an admission rejection would strand faucet_next_nonce
        // (already consumed on try_send) and brick the faucet until node restart.
        // If the const is None (faucet disabled) this is always false — correct.
        let faucet_exempt = self.is_faucet_sender(tx);

        // F-3 per-account mempool quota (independent gate; composes with the C7
        // kind-aware ingress filter above). REJECT (never evict — eviction would
        // orphan this sender's higher contiguous nonces).
        if !faucet_exempt {
            commputer::mempool_quota::account_quota_ok(
                pending_from_sender,
                commputer::mempool_quota::MAX_MEMPOOL_TXS_PER_ACCOUNT,
            )?;
        }

        // Finding [18]: fee-payability floor. The sender's on-chain balance must
        // cover this tx's fee PLUS the fees already committed by its pending mempool
        // txs. Closes free flooding (fresh 0-balance keypair streaming nonces) and
        // unpayable-max-fee eviction capture. Deliberately conservative (fees only;
        // transfer AMOUNT payability stays enforced at apply) so legitimate
        // future-funded chained sends are not false-rejected.
        if !faucet_exempt {
            let balance = self.state.accounts.get(&tx.from)
                .map(|a| a.balance.raw())
                .unwrap_or(0);
            let committed_fees = self.pending_txs.iter()
                .filter(|ptx| ptx.from == tx.from)
                .fold(0u64, |acc, ptx| acc.saturating_add(ptx.fee));
            if balance < committed_fees.saturating_add(tx.fee) {
                return Err("sender cannot cover fee");
            }
        }

        if append_nonce_rule {
            if !commputer::requeue_rules::append_nonce_ok(
                tx.nonce,
                on_chain_nonce,
                pending_from_sender,
            ) {
                return Err("invalid nonce");
            }
        } else if !commputer::requeue_rules::nonce_ok_for_requeue(tx.nonce, on_chain_nonce) {
            return Err("stale nonce");
        }
        Ok(())
    }

    /// The compiled faucet address is a trusted internal issuer whose nonce is
    /// serialized in rpc.rs. It is exempt from the mempool quota and the
    /// fee-payability floor: rejecting a dispense at ingress strands
    /// `faucet_next_nonce` (already consumed on try_send) and bricks the faucet
    /// until restart. Every payability gate must consult this.
    fn is_faucet_sender(&self, tx: &Transaction) -> bool {
        commputer::testnet_genesis::ALPHA_FAUCET_ADDRESS_HEX
            .and_then(|h| commputer_core::identity::Address::from_hex(h).ok())
            .is_some_and(|fa| fa == tx.from)
    }

    /// Content gates for a tx being RESTORED to the pool by `requeue_lost_txs`,
    /// as opposed to newly arriving. Everything `validate_tx_content` checks,
    /// except the nonce rule is staleness-only.
    ///
    /// The append rule (`nonce == on_chain + pending_from_sender`) is correct
    /// for fresh ingress — a new tx queues behind its sender's pooled txs — but
    /// WRONG for a restore, and applying it here silently discards exactly the
    /// tx this whole mechanism exists to save. Block production reads the
    /// expected nonce fresh from chain state per tx, so it packs only
    /// `nonce == on_chain` and returns the sender's higher nonces to the pool
    /// (see the 3-bucket filter). When that candidate loses, the restored tx
    /// has `nonce == on_chain` while its siblings sit pooled, so the append
    /// rule computes a strictly larger expectation and rejects it. Ordering is
    /// enforced by that same 3-bucket filter at the next pack, so all a restore
    /// must prove is that the tx is not already applied.
    fn validate_tx_for_requeue(&self, tx: &Transaction) -> Result<(), &'static str> {
        self.validate_tx_content_inner(tx, false)
    }

    fn handle_new_transaction(&mut self, tx: Transaction, source: libp2p::PeerId) {
        if let Err(reason) = self.validate_tx_for_mempool(&tx) {
            debug!("Rejected transaction from {}: {}", source, reason);
            return;
        }

        // If this is a ValidatorRegister tx, link the sender address to the peer.
        // Feature 175: Verify peer identity — tx signature must be valid for the claimed address.
        if matches!(tx.kind, commputer_core::transaction::TxKind::ValidatorRegister { .. }) {
            let validator_addr = tx.from;

            // The tx.verify() was already called in validate_tx_for_mempool,
            // so the signature matches. Store as verified mapping.
            info!(
                "Verified validator {} linked to peer {} via ValidatorRegister tx",
                validator_addr, source
            );
            self.peer_validators.insert(source, validator_addr);
            self.peer_last_seen.insert(source, std::time::Instant::now());

            // If we already know this peer's IP, register with compliance checker.
            if let Some(ip) = self.peer_ips.get(&source) {
                self.compliance.register_node(validator_addr, ip.clone());
            }
        }

        let hash = tx.hash();
        self.seen_tx_hashes.insert(hash);
        debug!("Accepted transaction into mempool: {:?}", hash);

        // Item 18-20: Wire compute job transactions into job pool.
        self.process_job_tx(&tx);

        self.mempool_added_at.insert(hash, std::time::Instant::now());
        self.pending_txs.push(tx);
        self.enforce_mempool_limit();
    }

    /// Validated re-admission of txs surrendered by LOSING candidates — fed by
    /// both the consensus apply path (take_finalized_with_lost) and the sync
    /// apply path (surrender_lost_at). Without requeue, a tx packed into a
    /// losing proposal is destroyed (the proposer mem::take'd it out of its
    /// mempool and no other pool prunes it back in).
    ///
    /// Hardened per review before first deploy:
    /// - CONTENT-VALIDATED: candidates are attacker-suppliable, and candidate
    ///   ingest skips signature checks for zero-from txs — every requeued tx
    ///   re-runs the ingress gates via `validate_tx_for_requeue` (restore
    ///   nonce rule; the seen-dedup is skipped because a restored tx is
    ///   legitimately already seen).
    /// - CAPPED PER TURN, on work EXAMINED: a height can hold 64 candidates ×
    ///   500 txs and validation is the expensive part, so the budget bounds
    ///   validations, not admissions. It is a per-TURN budget because one turn
    ///   can requeue many times (10-block sync batch, 200-deep orphan cascade,
    ///   a run of finalized heights); a per-call cap would hand each of those a
    ///   fresh 500.
    /// - HIGHEST-FEE FIRST: when the budget cannot cover everything surrendered,
    ///   examine in the same order the producer would pack. Truncating in
    ///   HashMap order would let a flood crowd out the genuine tx this exists to
    ///   save, converting a cost attack into silent loss of the property.
    /// - LAZY pool index: the O(mempool) hash set is built only once a tx is
    ///   actually worth checking, so an all-duplicate batch costs nothing.
    /// - EXPIRY-PRESERVING: entry().or_insert keeps the tx's original 1-hour
    ///   deadline. (A tx whose expiry swept while it rode an unresolved
    ///   candidate does get one fresh lease — bounded by the cap and by having
    ///   to keep losing races; alpha.7 item.)
    fn requeue_lost_txs(
        &mut self,
        winner_tx_hashes: &HashSet<TxHash>,
        lost_txs: Vec<Transaction>,
    ) {
        if lost_txs.is_empty() {
            return;
        }
        if self.requeue_budget == 0 {
            warn!(
                "requeue: turn budget exhausted — {} lost txs not considered",
                lost_txs.len()
            );
            return;
        }
        // Rank by BACKED fee: can the sender actually pay it, then how much.
        //
        // Ranking on `fee` alone inverts the protection it looks like it
        // provides. At sort time a fee is an unbacked CLAIM — the payability
        // check runs later, and the budget is charged per EXAMINATION, so a
        // fabricated `fee: u64::MAX` from a zero-balance key wins its slot for
        // free even though it is rejected a microsecond later. The tx this
        // mechanism exists to save (a faucet dispense) pays exactly
        // MINIMUM_FEE, i.e. the floor of that ordering — so fee-only ranking
        // hands a costless attacker deterministic priority over it.
        //
        // The affordability flag is one map lookup and makes the claim cost
        // something: unfunded txs sort last regardless of what they claim, and
        // outranking honest traffic requires really holding the balance. The
        // faucet is exempt for the same reason it is exempt at ingress.
        let mut ranked: Vec<(bool, u64, Transaction)> = lost_txs
            .into_iter()
            // Zero-from txs are the one class candidate ingest does NOT
            // signature-check, so they are free to fabricate in bulk. They can
            // never be admitted here (validate_tx_content_inner rejects null
            // senders), so dropping them before ranking denies a costless
            // flood any purchase on the budget at all.
            .filter(|tx| tx.from.0 != [0u8; 32])
            .map(|tx| {
                let affordable = self.is_faucet_sender(&tx)
                    || self.state.accounts.get(&tx.from)
                        .is_some_and(|a| a.balance.raw() >= tx.fee);
                let (a, f) = commputer::requeue_rules::requeue_rank(affordable, tx.fee);
                (a, f, tx)
            })
            .collect();
        ranked.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
        let lost_txs: Vec<Transaction> = ranked.into_iter().map(|(_, _, tx)| tx).collect();
        // Only txs we never looked at count as unexamined — a batch that was
        // entirely duplicates cost nothing and lost nothing, and reporting it
        // as loss would train operators to ignore the warn.
        let budget_at_entry = self.requeue_budget;
        let surrendered = lost_txs.len();
        let mut pooled: Option<HashSet<TxHash>> = None;
        let mut admitted = 0usize;
        // Iterate all, stopping when the BUDGET runs out — not `take(budget)`.
        // Skips that cost nothing (already in the winner, already pooled) must
        // not consume the window: competing producers pack overlapping
        // mempools, so under load the leading entries are mostly duplicates,
        // and a `take` window would be spent on them before ever reaching the
        // txs that were genuinely lost. That failure needs no attacker and is
        // silent.
        for tx in lost_txs {
            if self.requeue_budget == 0 {
                break;
            }
            let h = tx.hash();
            if winner_tx_hashes.contains(&h) {
                continue;
            }
            let pooled = pooled.get_or_insert_with(|| {
                self.pending_txs.iter().map(|p| p.hash()).collect()
            });
            if pooled.contains(&h) {
                continue;
            }
            self.requeue_budget = self.requeue_budget.saturating_sub(1);
            if let Err(reason) = self.validate_tx_for_requeue(&tx) {
                debug!("requeue: dropping lost tx {}: {}", hex::encode(h.0), reason);
                continue;
            }
            self.seen_tx_hashes.insert(h);
            self.mempool_added_at
                .entry(h)
                .or_insert_with(std::time::Instant::now);
            self.process_job_tx(&tx);
            pooled.insert(h);
            self.pending_txs.push(tx);
            admitted += 1;
        }
        let examined = budget_at_entry.saturating_sub(self.requeue_budget);
        let unexamined = surrendered.saturating_sub(examined);
        if unexamined > 0 && self.requeue_budget == 0 {
            warn!(
                "requeue: turn budget exhausted — up to {} surrendered txs not examined",
                unexamined
            );
        }
        if admitted > 0 {
            debug!("requeue: re-admitted {} txs from losing candidates", admitted);
            self.enforce_mempool_limit();
        }
    }

    /// Item 18-20: Process compute job transactions and update the job pool.
    fn process_job_tx(&mut self, tx: &Transaction) {
        let height = self.state.blocks.height();
        match &tx.kind {
            commputer_core::transaction::TxKind::SubmitJob {
                job_spec_hash,
                resources,
                max_duration_secs,
                comme_budget,
                l2_id,
            } => {
                // Derive job ID from tx hash.
                let tx_hash = tx.hash();
                let pool_job = PoolJob {
                    job_id: PoolJobId(tx_hash.0),
                    submitter: tx.from,
                    comme_budget: comme_budget.raw(),
                    cpu_cores: resources.cpu_cores,
                    gpu_vram_mb: resources.gpu_vram_mb,
                    ram_mb: resources.ram_mb,
                    storage_mb: resources.storage_mb,
                    bandwidth_mbps: resources.bandwidth_mbps,
                    max_duration_secs: *max_duration_secs,
                    job_spec_hash: *job_spec_hash,
                    status: commputer_storage::job_pool::PoolJobStatus::Pending,
                    submitted_height: height,
                    l2_id: l2_id.clone(),
                };
                self.job_pool.submit_job(pool_job);
                info!("Job pool: submitted job {}", hex::encode(&tx_hash.0[..8]));
            }
            commputer_core::transaction::TxKind::ClaimJob { job_id } => {
                let jid = PoolJobId(*job_id);
                if self.job_pool.assign_job(&jid, tx.from, height) {
                    info!("Job pool: assigned job {} to {}", hex::encode(&job_id[..8]), hex::encode(&tx.from.0[..8]));
                } else {
                    warn!("Job pool: failed to assign job {}", hex::encode(&job_id[..8]));
                }
            }
            commputer_core::transaction::TxKind::CompleteJob { job_id, result_hash } => {
                let jid = PoolJobId(*job_id);
                if self.job_pool.complete_job(&jid, *result_hash, height) {
                    info!("Job pool: completed job {}", hex::encode(&job_id[..8]));
                } else {
                    warn!("Job pool: failed to complete job {}", hex::encode(&job_id[..8]));
                }
            }
            commputer_core::transaction::TxKind::DisputeJob { job_id, .. } => {
                let jid = PoolJobId(*job_id);
                if self.job_pool.dispute_job(&jid, tx.from) {
                    info!("Job pool: disputed job {}", hex::encode(&job_id[..8]));
                }
            }
            // 1.2-POOL: mirror the V1 SubmitJob arm for V2 (the only escrowing kind) so executors
            // can see V2 jobs. Node-local pool only — no consensus effect, cannot fork. `job_id =
            // PoolJobId(tx.hash().0)` is the SAME id the escrow/pending maps and ClaimJob use (G-A).
            commputer_core::transaction::TxKind::SubmitJobV2 {
                program_hash,
                resources,
                max_duration_secs,
                comme_budget,
                l2_id,
                ..
            } => {
                let tx_hash = tx.hash();
                let pool_job = PoolJob {
                    job_id: PoolJobId(tx_hash.0),
                    submitter: tx.from,
                    comme_budget: comme_budget.raw(),
                    cpu_cores: resources.cpu_cores,
                    gpu_vram_mb: resources.gpu_vram_mb,
                    ram_mb: resources.ram_mb,
                    storage_mb: resources.storage_mb,
                    bandwidth_mbps: resources.bandwidth_mbps,
                    max_duration_secs: *max_duration_secs,
                    job_spec_hash: *program_hash, // program_hash is the V2 identity.
                    status: commputer_storage::job_pool::PoolJobStatus::Pending,
                    submitted_height: height,
                    l2_id: l2_id.clone(),
                };
                self.job_pool.submit_job(pool_job);
                info!("Job pool: submitted V2 job {}", hex::encode(&tx_hash.0[..8]));
            }
            _ => {}
        }
    }

    /// Maximum number of transactions in the mempool.
    const MAX_MEMPOOL_SIZE: usize = 5000;

    /// Requeue validations allowed per event-loop turn (one block's worth).
    /// Bounds the synchronous cost an attacker-supplied pile of losing
    /// candidates can impose on the block-apply path. See `requeue_lost_txs`.
    const REQUEUE_BUDGET_PER_TURN: usize = commputer_core::block::MAX_TRANSACTIONS_PER_BLOCK;

    /// Enforce mempool size limit. Finding [18]: evict fee-UNAFFORDABLE txs first
    /// (sender's on-chain balance cannot cover the tx fee), then lowest-fee — so a
    /// flood of unpayable high-fee txs cannot capture the pool by out-surviving
    /// honest lower-fee txs under a pure lowest-fee eviction.
    fn enforce_mempool_limit(&mut self) {
        let excess = self.pending_txs.len().saturating_sub(Self::MAX_MEMPOOL_SIZE);
        if excess == 0 {
            return;
        }
        // AMORTIZED: rank once, drop the worst `excess` in a single pass.
        // The previous shape was a full min_by_key scan (with a per-element
        // account lookup) plus an O(n) Vec::remove PER EVICTION — fine when
        // every caller inserted one tx, but requeue can admit a block's worth
        // at once, which turned it quadratic on the block-apply path.
        // Eviction ORDER is unchanged: `(affordable, fee)` ascending, so
        // fee-unaffordable txs go first, then lowest fee — a flood of unpayable
        // high-fee txs still cannot capture the pool.
        let mut ranked: Vec<(bool, u64, usize)> = self
            .pending_txs
            .iter()
            .enumerate()
            .map(|(idx, tx)| {
                let bal = self.state.accounts.get(&tx.from)
                    .map(|a| a.balance.raw())
                    .unwrap_or(0);
                (bal >= tx.fee, tx.fee, idx)
            })
            .collect();
        ranked.sort_unstable();
        let doomed: HashSet<usize> = ranked.into_iter().take(excess).map(|(_, _, i)| i).collect();
        let mut idx = 0usize;
        self.pending_txs.retain(|_| {
            let keep = !doomed.contains(&idx);
            idx += 1;
            keep
        });
        debug!("Evicted {} txs from mempool (affordable-first, lowest-fee)", doomed.len());
    }

    pub fn auto_register_validator(&mut self, contribution_percent: u8) {
        self.validator.register(contribution_percent);

        // Register ourselves in the epoch state so we count as a validator
        let summary = commputer_core::proof::EpochProofSummary {
            validator: *self.wallet.address(),
            epoch: self.state.current_epoch,
            processing_score: 100,
            gpu_score: 100,
            storage_score: 100,
            ram_score: 100,
            bandwidth_score: 100,
            diversity_bonus: 50,
        };
        self.epoch_state.record_summary(summary);

        // Create and sign a ValidatorRegister transaction with our wallet address.
        // This is broadcast to the network so peers can verify our identity.
        let nonce = self.state.accounts.get(self.wallet.address())
            .map(|a| a.nonce)
            .unwrap_or(0);
        let mut tx = Transaction {
            from: *self.wallet.address(),
            nonce,
            kind: commputer_core::transaction::TxKind::ValidatorRegister {
                hardware_fingerprint_hash: {
                    use sha2::{Sha256, Digest};
                    let hw_bytes = borsh::to_vec(&self.hardware).unwrap_or_default();
                    let hash = Sha256::digest(&hw_bytes);
                    let mut out = [0u8; 32];
                    out.copy_from_slice(&hash);
                    out
                },
                contribution_percent,
            },
            fee: 0, // Registration is fee-exempt
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        commputer_core::signing::sign_transaction(&mut tx, &self.wallet);

        // Broadcast ValidatorRegister tx to the network so peers learn our identity.
        if let Ok(data) = serde_json::to_vec(&tx) {
            let compressed = commputer_network::compress(&data);
            let topic = topics::tx_topic();
            if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, compressed) {
                debug!("Failed to broadcast ValidatorRegister tx (will retry on peer connect): {}", e);
            }
        }

        // Add to our own mempool so it gets included in the next block we produce.
        self.mempool_added_at.insert(tx.hash(), std::time::Instant::now());
        self.pending_txs.push(tx);

        info!(
            "Registered as validator at {}% contribution (address: {})",
            contribution_percent,
            self.wallet.address(),
        );
    }

    fn handle_epoch_tick(&mut self) {
        let epoch = self.epoch_state.epoch;
        let validator_count = self.epoch_state.validator_count() as u64;

        if validator_count == 0 {
            debug!("Epoch {} tick — no validators", epoch);
            self.epoch_state = EpochState::new(epoch + 1, 0);
            // Bound seen_tx_hashes memory: clear at epoch boundary (txs older than an epoch are either finalized or expired).
            self.seen_tx_hashes.clear();
            return;
        }

        // Item 53: Epoch transition logging (bookkeeping only, no emission).
        info!("--- Epoch {} Transition (bookkeeping) ---", epoch);
        info!("  Validators:       {}", validator_count);
        info!("  Total emitted:    {:.4} COMME (via per-block rewards)", self.state.total_emitted as f64 / UNITS_PER_COMME as f64);
        info!("  Total burned:     {:.4} COMME", self.state.total_burned as f64 / UNITS_PER_COMME as f64);
        info!("  Remaining supply: {:.4} COMME", self.state.remaining_supply() as f64 / UNITS_PER_COMME as f64);
        {
            let compliant = self.state.accounts.iter()
                .filter(|a| a.is_validator && a.compliance == commputer_core::compliance::ComplianceStatus::Compliant)
                .count();
            let nerfed = self.state.accounts.iter()
                .filter(|a| a.is_validator && a.compliance != commputer_core::compliance::ComplianceStatus::Compliant)
                .count();
            info!("  Compliant validators: {}, Nerfed: {}", compliant, nerfed);
        }

        // Feature 114 + Item 152: Dispatch verdict computation off-task.
        // Empirical evidence (stress runs of 2026-05-04): running the verifier
        // loop inline in this select! arm body blocks all other arms (including
        // block_interval) for the duration — chain stalled ~110s at every epoch
        // transition with 3 validators. block_in_place doesn't help inside a
        // select arm body; only deferring to another task does. See doc comment
        // on EpochFinalizeData for the design.
        let pending = self.proof_manager.pending_challenges_clone();
        let responses = self.proof_manager.responses_clone();
        let expired = self.proof_manager.expired_challenges_clone();
        let height = self.proof_manager.current_height;
        let multipliers = self.epoch_state.difficulty_multiplier.clone();
        let tx = self.epoch_finalize_tx.clone();
        tokio::task::spawn_blocking(move || {
            let verdicts = ProofManager::compute_epoch_verdicts(
                &pending, &responses, &expired, height,
            );
            let _ = tx.send(EpochFinalizeData {
                verdicts,
                multipliers,
                epoch_being_finalized: epoch,
                validator_count,
            });
        });
        // The remainder of the epoch transition (record summaries, account
        // scans, will processing, EpochState reset, next-epoch difficulty)
        // runs in handle_epoch_tick_post when the verdicts arrive.
    }

    /// Post-verdict half of the epoch transition. Called from the dedicated
    /// `epoch_finalize_rx` arm of the main `tokio::select!` once the
    /// spawn_blocking verdict computation completes. Splitting this off keeps
    /// the swarm/block_interval/etc. arms responsive during the verify window.
    fn handle_epoch_tick_post(&mut self, data: EpochFinalizeData) {
        let EpochFinalizeData { verdicts, multipliers, epoch_being_finalized, validator_count } = data;
        let epoch = epoch_being_finalized;

        let proof_summaries = self.proof_manager.finalize_epoch_with_precomputed_verdicts(
            &verdicts,
            &multipliers,
        );
        for (_addr, summary) in &proof_summaries {
            self.epoch_state.record_summary(summary.clone());
        }

        // Rewards are now credited per-block during apply_block_validated.
        // Epoch transitions handle bookkeeping only: scoring, compliance, difficulty.
        info!(
            "Epoch {} complete: {} validators (rewards via per-block production)",
            epoch, validator_count,
        );

        // Supply status logging.
        if self.state.is_emergency_access() {
            warn!("EMERGENCY ACCESS MODE: circulating supply below 1M COMME — any contribution = full access");
        }
        let remaining_pct = (self.state.remaining_supply() as f64
            / commputer_core::token::TOTAL_SUPPLY as f64) * 100.0;
        if remaining_pct <= 5.0 {
            warn!("WARNING: Only {:.2}% of supply remaining", remaining_pct);
        }

        // Refill grace period for our own validator (1 epoch = 3600s online).
        // Only update if our account exists on-chain (via block reward or registration).
        // Creating an account here would cause state divergence across nodes.
        {
            let our_addr = *self.wallet.address();
            if let Some(account) = self.state.accounts.get_mut(&our_addr) {
                account.cumulative_uptime_secs += 3600;
                account.refill_grace(3600);
            }
        }

        // 120-year inactive wallet cleanup: mark wallets inactive if last active > 120 years ago.
        {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let addrs: Vec<Address> = self.state.accounts.iter()
                .filter(|a| !a.is_inactive && a.last_active_timestamp > 0)
                .map(|a| a.address)
                .collect();
            let mut marked = 0u64;
            for addr in addrs {
                if let Some(acct) = self.state.accounts.get_mut(&addr) {
                    acct.check_inactive(now_secs);
                    if acct.is_inactive {
                        marked += 1;
                    }
                }
            }
            if marked > 0 {
                info!("Marked {} wallets as inactive (120+ years)", marked);
            }
        }

        // Will function processing: check grace period expiry and emit notifications.
        {
            use commputer_storage::account::WillStatus;
            let _now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let addrs: Vec<Address> = self.state.accounts.iter()
                .filter(|a| !a.will_contacts.is_empty() && a.will_status == WillStatus::Registered)
                .map(|a| a.address)
                .collect();
            for addr in addrs {
                if let Some(acct) = self.state.accounts.get_mut(&addr) {
                    // Check if grace period has fully expired.
                    if acct.grace_balance_secs == 0 && acct.cumulative_uptime_secs > 0 {
                        acct.will_status = WillStatus::Pending;
                        info!("Will triggered for {}: grace period expired, notifying contacts", addr);
                    }
                }
            }
        }

        // Update capacity RPC data.
        if let Some(ref rpc_state) = self.rpc_state {
            let total_cap = validator_count; // Each validator = 1 unit of capacity.
            let churn = 0.0; // TODO: compute real churn from validators joined/left.
            let breakdown = commputer_storage::job_pool::JobPool::new()
                .capacity_breakdown(total_cap, churn);
            if let Ok(mut cap) = rpc_state.capacity.try_lock() {
                *cap = breakdown;
            }
        }

        // Re-register ourselves for the next epoch
        let self_summary = commputer_core::proof::EpochProofSummary {
            validator: *self.wallet.address(),
            epoch: epoch + 1,
            processing_score: 100,
            gpu_score: 100,
            storage_score: 100,
            ram_score: 100,
            bandwidth_score: 100,
            diversity_bonus: 50,
        };

        // Feature 114: Compute next epoch's difficulty multipliers.
        let next_difficulty = self.epoch_state.compute_next_difficulty();

        // Feature 126: Emit epoch summary event.
        let epoch_summary = commputer_consensus::epoch::EpochSummary {
            epoch,
            validator_count,
            total_emission: 0, // Emission now per-block, not per-epoch
            difficulty_adjustments: next_difficulty.clone(),
            active_validator_count: self.epoch_state.active_validators.len(),
        };
        info!(
            "Epoch {} summary: {} validators, {} active, emission={}, difficulty adjustments: {:?}",
            epoch_summary.epoch,
            epoch_summary.validator_count,
            epoch_summary.active_validator_count,
            epoch_summary.total_emission,
            epoch_summary.difficulty_adjustments,
        );

        // Feature 124: Snapshot current validators for the next epoch.
        // All validators who submitted proof summaries this epoch become the active set.
        let next_active_validators: std::collections::HashSet<_> = self.epoch_state
            .summaries.keys().copied().collect();

        // Feature 125: Reset slashing state for the new epoch.
        self.consensus.reset_epoch_slashing();

        // Feature 9: Create epoch summary to include in next block.
        let compliant_count = self.state.accounts.iter()
            .filter(|a| a.is_validator && a.compliance == commputer_core::compliance::ComplianceStatus::Compliant)
            .count() as u64;
        let nerfed_count = self.state.accounts.iter()
            .filter(|a| a.is_validator && a.compliance != commputer_core::compliance::ComplianceStatus::Compliant)
            .count() as u64;
        let proof_scores_total: u64 = self.epoch_state.summaries.values()
            .map(|s| s.composite_score())
            .sum();
        self.pending_epoch_summary = Some(commputer_core::block::EpochSummary {
            epoch,
            total_emission: 0, // Emission now per-block, not per-epoch
            total_burned: self.state.total_burned,
            validator_count,
            proof_scores_total,
            compliant_count,
            nerfed_count,
        });

        // Ratchet, never regress: sync-applied blocks can carry the chain's
        // epoch ahead of this node's wall-clock tick counter; the tick must
        // not roll a synced epoch back (live: /status epochs diverged 0 vs 2
        // across nodes after resyncs).
        self.state.current_epoch = self.state.current_epoch.max(epoch + 1);
        self.epoch_state = EpochState::new(epoch + 1, 0);
        self.epoch_state.difficulty_multiplier = next_difficulty;
        self.epoch_state.snapshot_validators(next_active_validators);
        self.epoch_state.record_summary(self_summary);
        // Bound seen_tx_hashes memory: clear at epoch boundary (txs older than an epoch are either finalized or expired).
        self.seen_tx_hashes.clear();
    }

    fn handle_block_tick(&mut self) {
        trace!("block tick: height={} state={:?}", self.state.blocks.height(), self.node_state.state());
        // Item 51: Expire mempool transactions older than 1 hour.
        let now = std::time::Instant::now();
        let expiry_threshold = Duration::from_secs(3600);
        let expired: Vec<TxHash> = self.mempool_added_at.iter()
            .filter(|(_, added_at)| now.duration_since(**added_at) >= expiry_threshold)
            .map(|(hash, _)| *hash)
            .collect();
        if !expired.is_empty() {
            let expired_set: HashSet<TxHash> = expired.iter().copied().collect();
            let before = self.pending_txs.len();
            self.pending_txs.retain(|tx| !expired_set.contains(&tx.hash()));
            for hash in &expired {
                self.mempool_added_at.remove(hash);
            }
            let removed = before - self.pending_txs.len();
            if removed > 0 {
                info!("Expired {} mempool transactions older than 1 hour", removed);
            }
        }

        // Re-broadcast self-signed ValidatorRegister until it lands on-chain.
        // Why: the initial gossipsub.publish in auto_register_validator and the
        // single-shot retry on ConnectionEstablished both race against gossipsub
        // topic-mesh formation. If the mesh isn't ready, the message is silently
        // dropped and the joiner is stuck as a non-validator forever.
        if self.has_ever_connected && self.state.blocks.height() % 15 == 0 {
            let our_addr = *self.wallet.address();
            let already_registered = self.state.accounts.get(&our_addr)
                .map(|a| a.is_validator)
                .unwrap_or(false);
            if !already_registered {
                let pending_register = self.pending_txs.iter()
                    .find(|tx| tx.from == our_addr
                        && matches!(tx.kind, commputer_core::transaction::TxKind::ValidatorRegister { .. }))
                    .cloned();
                if let Some(tx) = pending_register
                    && let Ok(data) = serde_json::to_vec(&tx)
                {
                    let compressed = commputer_network::compress(&data);
                    let topic = topics::tx_topic();
                    match self.network.swarm.behaviour_mut().gossipsub.publish(topic, compressed) {
                        Ok(_) => info!("Re-broadcast ValidatorRegister tx (awaiting on-chain confirmation)"),
                        Err(e) => debug!("ValidatorRegister re-broadcast failed: {}", e),
                    }
                }
            }
        }

        // Don't produce blocks until node is Active (synced with network).
        if !self.node_state.is_active() {
            debug!("Skipping block production — node_state is {:?}, not Active", self.node_state.state());
            return;
        }

        // Only produce blocks if we're a registered validator.
        if self.validator.status() != ValidatorStatus::Active {
            debug!("Skipping block production — local validator status is {:?}", self.validator.status());
            return;
        }

        // Feature 5: Validator cooldown — skip block production if within cooldown period.
        // Exempt validators registered in the first few blocks (bootstrap validators).
        let our_addr = *self.wallet.address();
        if let Some(acct) = self.state.accounts.get(&our_addr)
            && let Some(reg_height) = acct.validator_registered_height
            && reg_height >= commputer_core::transaction::VALIDATOR_COOLDOWN_BLOCKS {
                let current_height = self.state.blocks.height();
                if current_height < reg_height + commputer_core::transaction::VALIDATOR_COOLDOWN_BLOCKS {
                    debug!("Skipping block production — validator cooldown ({} blocks remaining)",
                        reg_height + commputer_core::transaction::VALIDATOR_COOLDOWN_BLOCKS - current_height);
                    return;
                }
            }

        // Feature 172: Skip block production during network partition.
        // Bootstrap leader (no seeds) is exempt -- it must produce the first blocks solo.
        if self.partition_detected && self.is_seed_connector {
            debug!("Skipping block production — network partition detected");
            return;
        }

        let next_height = self.state.blocks.height() + 1;

        // Feature 174: Check protocol upgrade activation.
        if next_height >= PROTOCOL_V2_ACTIVATION_HEIGHT.saturating_sub(100)
            && PROTOCOL_V2_ACTIVATION_HEIGHT != u64::MAX
        {
            warn!(
                "Protocol v2 activates at height {} (current: {}). Ensure you are running the latest version.",
                PROTOCOL_V2_ACTIVATION_HEIGHT, next_height
            );
        }

        // Leader election: only produce if we're the elected leader for this height.
        // STAKE-WEIGHTED schedule with view change fallback every 6 seconds.
        // Skip leader check during bootstrap (< 2 known validators on-chain).
        // Same derivation as the validation side (see consensus_cycle).
        let validators: Vec<Address> = self.consensus_cycle();
        let our_addr = *self.wallet.address();
        let seconds_waiting = self.last_block_seen_time
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);

        // DISTINCT validators, not cycle length: a cycle's len() is Σweights, so
        // one validator weighted 2 would otherwise read as a two-validator
        // network and be cleared to produce alone.
        if commputer::leader::distinct_validator_count(&validators) >= 2 {
            // Strict leader election: only the elected leader produces.
            // View change fallback handles leader unavailability (6s intervals).
            // If consensus stalls, the stall timer in handle_consensus_tick handles it.
            if !commputer::leader::cycle_is_valid_leader(&validators, next_height, &our_addr, seconds_waiting) {
                // Silent until now: this return is the most common reason a
                // node produces nothing, and with no log a stalled network
                // could not be diagnosed without a source-level bisect.
                debug!(
                    "Skipping block production — not leader for height {} (waiting {}s, {} validators)",
                    next_height,
                    seconds_waiting,
                    // DISTINCT, not validators.len(): that is Σweights now. This
                    // line exists to diagnose "why is nothing being produced",
                    // and printing 4095 for a three-node network would send the
                    // reader after a corrupt validator set that does not exist.
                    commputer::leader::distinct_validator_count(&validators)
                );
                return;
            }
        } else if self.is_seed_connector {
            // Bootstrap phase (< 2 validators): only the seed node (bootstrap leader) produces.
            // Nodes started with --seeds defer to the seed to prevent competing blocks.
            return;
        }

        // Never produce a second block at the same height -- that's equivocation.
        // The view change bypass (6s) allows a DIFFERENT validator to produce,
        // not the same one to produce again.
        if self.consensus.we_produced_at(next_height, &our_addr) {
            debug!("Skipping block production — we already produced at height {}", next_height);
            return;
        }

        // Don't produce if there's already an active vote at this height,
        // UNLESS we've been waiting 6+ seconds (view change).
        if seconds_waiting < 6
            && (self.consensus.has_active_vote(next_height) || self.consensus.has_height(next_height))
        {
            debug!(
                "Skipping block production — deferring at height {} (waiting {}s < 6, candidate/vote present)",
                next_height, seconds_waiting
            );
            return;
        }

        let parent = self
            .state
            .blocks
            .latest()
            .map(|b| b.hash())
            .unwrap_or(BlockHash::GENESIS);

        // Create a new block with pending transactions (capped to block size limit).
        // 3-bucket nonce filter:
        //   1. strictly-stale  (tx.nonce <  expected): drop permanently — already applied or duplicate.
        //   2. exact-match     (tx.nonce == expected): include as block candidate.
        //   3. future-nonce    (tx.nonce >  expected): return to pending_txs for a later block.
        //
        // Using '>=' (the previous logic) let future-nonce txs into the candidate list.
        // apply_transaction requires an exact nonce match and would reject them, causing block
        // rejection.  Future-nonce txs were also permanently lost because std::mem::take already
        // emptied pending_txs before the retain ran.
        let all_txs = std::mem::take(&mut self.pending_txs);
        let mut candidates: Vec<Transaction> = Vec::new();
        let mut future_txs: Vec<Transaction> = Vec::new();
        for tx in all_txs {
            let expected_nonce = self.state.accounts.get(&tx.from)
                .map(|a| a.nonce)
                .unwrap_or(0);
            match tx.nonce.cmp(&expected_nonce) {
                std::cmp::Ordering::Less    => { /* strictly-stale: drop permanently */ }
                std::cmp::Ordering::Equal   => candidates.push(tx),
                std::cmp::Ordering::Greater => future_txs.push(tx),
            }
        }
        // Return future-nonce txs to the mempool so they can be included once
        // the intermediate nonces land in a confirmed block.
        self.pending_txs = future_txs;
        // Feature 6: Sort candidates by (fee desc, nonce asc) using a stable sort so
        // that within the same sender higher-fee txs sort first but lower nonces always
        // precede higher nonces for the same fee, preventing N+1-before-N reordering.
        candidates.sort_by_key(|tx| (std::cmp::Reverse(tx.fee), tx.nonce));
        let txs: Vec<Transaction> = if candidates.len() > commputer_core::block::MAX_TRANSACTIONS_PER_BLOCK {
            let overflow = candidates.split_off(commputer_core::block::MAX_TRANSACTIONS_PER_BLOCK);
            self.pending_txs.extend(overflow); // Put excess back in mempool.
            candidates
        } else {
            candidates
        };

        // B7 (C8): producer-side capacity admission — SOFT scheduling (v1), NOT apply-enforced, so it
        // cannot fork. Split compute-job txs from the rest, admit via the tested 51/49 scheduler, keep
        // only admitted job_ids, and REQUEUE (never drop) the deferred job txs. Non-job txs are wholly
        // untouched (legacy path byte-identical). When there are no job txs this is a pure no-op.
        let churn = commputer_pouw_onchain::capacity::validator_churn_bps(0, 0, 0); // v1: no churn tracking → 0.
        // Only the NEW escrow-based SubmitJobV2 jobs are capacity-managed. Legacy V1 SubmitJob bypasses
        // admission entirely so its block-inclusion scheduling stays byte-identical to the pre-flip node
        // (pending_job_from_tx maps V1 too, but capacity gating is a G6/flip concept that must not alter
        // any legacy path).
        let admission_job = |tx: &Transaction| -> Option<commputer_pouw_onchain::capacity::PendingJob> {
            if matches!(tx.kind, commputer_core::transaction::TxKind::SubmitJobV2 { .. }) {
                commputer_storage::state::pending_job_from_tx(tx)
            } else {
                None
            }
        };
        let pending: Vec<commputer_pouw_onchain::capacity::PendingJob> =
            txs.iter().filter_map(admission_job).collect();
        let txs: Vec<Transaction> = if pending.is_empty() {
            txs
        } else {
            let adm = commputer_pouw_onchain::capacity::admit(
                self.state.capacity_params(),
                churn,
                &pending,
            );
            let admitted: HashSet<[u8; 32]> = adm.admitted.into_iter().collect();
            let mut kept = Vec::with_capacity(txs.len());
            let mut deferred = Vec::new();
            for tx in txs {
                match admission_job(&tx) {
                    Some(pj) if !admitted.contains(&pj.job_id) => deferred.push(tx),
                    _ => kept.push(tx), // admitted V2 job OR non-admission tx (incl. legacy V1)
                }
            }
            self.pending_txs.extend(deferred); // requeue non-admitted jobs, like the future-nonce path.
            kept
        };

        // 1.2-MEMPOOL (C2/C3): speculative apply — keep only the txs that apply CLEANLY in sequence on
        // top of current state, so the produced block can never fail apply (which would strand every
        // node at this height — a zero-cost DoS). `select_applicable_txs` trial-applies on a snapshot
        // and FULLY restores `self.state` before returning (ChainState is not Clone), so the producer
        // state root below is byte-identical to a run without this call. Phase/window-deferred txs are
        // requeued (not dropped); permanently-doomed txs are discarded.
        let (txs, requeue) = self.state.select_applicable_txs(txs);
        self.pending_txs.extend(requeue);

        let mut block = Block {
            header: BlockHeader {
                protocol_version: commputer_core::block::CURRENT_PROTOCOL_VERSION,
                height: next_height,
                parent_hash: parent,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                producer: *self.wallet.address(),
                epoch: self.state.current_epoch,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: commputer_core::genesis::TESTNET_CHAIN_ID.to_string(),
            },
            transactions: txs,
            proof_summaries: vec![],
            compliance_summary: None,
            // Feature 9: Include epoch summary if one is pending from the last epoch tick.
            epoch_summary: self.pending_epoch_summary.take(),
        };

        // Compute and set merkle roots and state root.
        block.header.tx_root = block.compute_tx_root();
        block.header.proof_root = block.compute_proof_root();
        block.header.state_root = self.state.compute_state_root();

        // Feature 248: Set checkpoint hash at checkpoint intervals.
        if next_height.is_multiple_of(commputer_core::block::CHECKPOINT_HASH_INTERVAL) && next_height > 0 {
            block.header.checkpoint_hash = Some(self.state.compute_state_root());
            if let Some(ref hash) = block.header.checkpoint_hash {
                info!("Checkpoint at height {}: state root = {}", next_height, hex::encode(hash));
            }
        }

        // Sign the block header with our wallet key.
        commputer_core::signing::sign_block(&mut block, &self.wallet);

        info!("Produced block candidate at height {}", next_height);

        // Send block proposal directly to each peer via consensus request-response.
        // Direct delivery — no gossipsub mesh issues, guaranteed delivery.
        let block_bytes = serde_json::to_vec(&block).unwrap_or_default();
        let peers: Vec<libp2p::PeerId> = self.peer_ips.keys().copied().collect();
        for peer in &peers {
            let request = commputer_network::consensus_protocol::ConsensusRequest::BlockProposal {
                block_bytes: block_bytes.clone(),
                height: next_height,
            };
            self.network.swarm.behaviour_mut().consensus.send_request(peer, request);
        }
        debug!("Sent block proposal for height {} to {} peers via request-response", next_height, peers.len());

        // Feature 7: Broadcast compact BlockAnnounce on the blocks topic instead of full block.
        // Peers that need the full block will request it via the block request protocol.
        let announce = commputer_core::block::BlockAnnounce {
            hash: block.hash(),
            height: block.height(),
            producer: *self.wallet.address(),
        };
        if let Ok(data) = serde_json::to_vec(&announce) {
            let compressed = commputer_network::compress(&data);
            let topic = topics::block_topic();
            if let Err(e) = self
                .network
                .swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic, compressed)
            {
                warn!("Failed to broadcast block announce: {}", e);
            }
        }

        // Enter our own block as a candidate and start the vote.
        // Use add_local_candidate to avoid false equivocation detection on retries.
        self.consensus.add_local_candidate(block);

        // Don't try to finalize here. Wait for the consensus tick (500ms) to
        // collect peer votes before attempting finalization. This prevents
        // solo-finalization before peers have a chance to participate.
    }

    /// Consensus round tick (500ms): for each active height, publish a query
    /// and attempt to finalize the round from accumulated responses.
    /// The hash of our current chain tip (or GENESIS before block 1). Recomputed
    /// FRESH on every call — the consensus content filter (QC-004/QC-005) reads
    /// it to keep only tip-parented votes, so a cached value would reject a
    /// just-applied block's own child as foreign-parent and strand the height.
    fn tip_hash(&self) -> BlockHash {
        self.state
            .blocks
            .latest()
            .map(|b| b.hash())
            .unwrap_or(BlockHash::GENESIS)
    }

    /// QC-009 liveness floor: grace window before the unbound-vote fallback
    /// engages after ALL validator bindings are lost. Must be >> handshake RTT
    /// (so a normal reconnect never opens the window) and < the stall window.
    const GRACE_T: Duration = Duration::from_secs(5);

    /// True when NO connected peer is bound to a currently-eligible validator AND
    /// that has held for at least GRACE_T since the last active tick that had one.
    /// Only then are unbound votes counted, so a genuinely isolated node keeps
    /// producing at clamp semantics rather than halting; any single honest bound
    /// peer keeps `last_bound_at` fresh (in the tick) and holds this false,
    /// locking an attacker's unbound sockets out. An attacker cannot force this
    /// true — it cannot unbind our honest peers.
    fn unbound_fallback_active(&self, eligible: &[Address]) -> bool {
        let has_bound_eligible = self
            .attested_peers
            .iter()
            .any(|(p, a)| self.peer_ips.contains_key(p) && eligible.contains(a));
        !has_bound_eligible
            && self
                .last_bound_at
                .is_some_and(|t| t.elapsed() >= Self::GRACE_T)
    }

    fn handle_consensus_tick(&mut self) {
        // Don't participate in consensus while syncing.
        if !self.node_state.is_active() {
            return;
        }

        let peer_count = self.peer_ips.len();
        // QC-001 clamp: size the rung from on-chain truth, not the raw socket
        // count. distinct_eligible is derived fresh from the current cycle;
        // RungInput::derive returns min(peer_count, distinct_eligible-1) (0 iff no
        // peers), so extra sockets can no longer inflate the quorum bar. The
        // consensus_cycle() call is cache-guarded and is taken again below for
        // set_consensus_validators; the second hit is an early-return no-op.
        let distinct_eligible =
            commputer::leader::distinct_validator_count(&self.consensus_cycle());
        self.consensus.update_params_for_rung(
            crate::consensus_manager::RungInput::derive(peer_count, distinct_eligible),
        );

        // QC-009 liveness floor: keep the grace clock fresh while we hold any peer
        // bound to a currently-eligible validator. The unbound-vote fallback (in
        // the vote gate) engages only after GRACE_T with zero such peers — a
        // sustained isolation / kill test, never a transient reconnect. Start the
        // clock at the FIRST active tick, not process boot, so a node that boots
        // isolated does not immediately count unbound votes.
        if self.last_bound_at.is_none() {
            self.last_bound_at = Some(std::time::Instant::now());
        }
        let has_bound_eligible = {
            let eligible = self.consensus_validators();
            self.attested_peers
                .iter()
                .any(|(p, a)| self.peer_ips.contains_key(p) && eligible.contains(a))
        };
        if has_bound_eligible {
            self.last_bound_at = Some(std::time::Instant::now());
        }
        // A wall-clock stall accumulated with nobody to talk to is meaningless —
        // without this, a node that rejoins after a 0-peer stretch fires an
        // immediate (destructive) recovery off stale timing.
        if peer_count == 0 {
            self.stall_start = None;
        }

        // Only vote/finalize for the NEXT height (tip+1).
        let next_height = self.state.blocks.height() + 1;
        if self.consensus.has_height(next_height) {
            if !self.consensus.proposal_sent(next_height) {
                // First time: send full block proposal to all peers via request-response.
                if let Some(block) = self.consensus.get_candidate_block(next_height) {
                    let block_bytes = serde_json::to_vec(&block).unwrap_or_default();
                    let peers: Vec<libp2p::PeerId> = self.peer_ips.keys().copied().collect();
                    for peer in &peers {
                        let request = commputer_network::consensus_protocol::ConsensusRequest::BlockProposal {
                            block_bytes: block_bytes.clone(),
                            height: next_height,
                        };
                        self.network.swarm.behaviour_mut().consensus.send_request(peer, request);
                    }
                    self.consensus.mark_proposal_sent(next_height);
                    info!("Sent BlockProposal at height {} to {} peers", next_height, peers.len());
                }
            } else {
                // Retry: send full BlockProposal to peers who haven't voted yet.
                // Late-joiners (peers who connected after the initial proposal) need the
                // full block to vote. Sending just a VoteRequest (hash only) would cause
                // them to respond NotReady because they don't have the block candidate.
                if let Some(block) = self.consensus.get_candidate_block(next_height) {
                    let block_bytes = serde_json::to_vec(&block).unwrap_or_default();
                    let non_voters: Vec<libp2p::PeerId> = self.peer_ips.keys()
                        .filter(|p| !self.voted_peers.contains(p))
                        .copied()
                        .collect();
                    for peer in &non_voters {
                        let request = commputer_network::consensus_protocol::ConsensusRequest::BlockProposal {
                            block_bytes: block_bytes.clone(),
                            height: next_height,
                        };
                        self.network.swarm.behaviour_mut().consensus.send_request(peer, request);
                    }
                    if !non_voters.is_empty() {
                        info!("Re-sent BlockProposal at height {} to {} non-voters", next_height, non_voters.len());
                    }
                }
            }

            // Try to finalize from responses accumulated in previous ticks.
            let result = self.consensus.try_finalize_round(next_height, peer_count, self.tip_hash());
            // Clear voted_peers after each round so the next Snowball sampling
            // round re-queries all peers (needed for decision_threshold rounds).
            self.voted_peers.clear();
            match result {
                crate::consensus_manager::ConsensusRoundResult::Finalized => {
                    // Consensus is working -- reset stall timer.
                    // Block application is handled by the existing finalized_heights loop below.
                    self.stall_start = None;
                }
                crate::consensus_manager::ConsensusRoundResult::Stalled => {
                    if peer_count == 0 {
                        // Solo node: we ARE the network. Self-finalize by voting for our own block.
                        // This is safe because there are no peers to disagree with.
                        match self.consensus.query_preference(next_height) {
                            Some(pref) => {
                                info!("Solo self-vote at height {} for {}", next_height, pref);
                                // Slice 2 feed site: attribute the solo self-vote to our own PeerId
                                // (peer_count == 0 ⇒ params (1,1,1); per-round aggregator reset lets
                                // each round's self-vote count fresh).
                                self.consensus.record_peer_response(next_height, pref, self.network.local_peer_id);
                            }
                            None => {
                                warn!("Solo stall at height {} but no preference -- voter not initialized?", next_height);
                            }
                        }
                        // Don't start stall timer for solo nodes -- stalling is expected.
                    } else {
                        // Multi-node: start or check stall timer.
                        let stall_start = *self.stall_start.get_or_insert_with(std::time::Instant::now);
                        let elapsed = stall_start.elapsed().as_secs();
                        // Per-node jitter: a shared network event must not make
                        // every node fire recovery in the same second (live
                        // 2026-07-24: synchronized destructive resyncs would
                        // truncate the chain to the slowest peer's height).
                        let jitter = (self.network.local_peer_id.to_bytes().last().copied().unwrap_or(0) as u64) % 30;
                        if elapsed >= 60 + jitter {
                            let local = self.state.blocks.height();
                            let target = self.node_state.network_height();
                            // The FIRST stall at any height is never destructive:
                            // re-engage (a no-op when there is no gap) and record.
                            // Destructive recovery requires a REPEAT stall at the
                            // same height — a mis-read height signal must not be
                            // able to wipe state on one bad sample (live
                            // 2026-07-25: a fresh peer's low sample turned
                            // "behind" into "at-tip fork" and truncated the chain).
                            if self.stall_reengage_height != local {
                                // Merely behind: non-destructive recovery — restart
                                // the sync machine and let it pull the gap. A plain
                                // stall must never wipe state (reset_to_genesis is
                                // stranger-triggerable via induced quorum
                                // starvation); the destructive path is reserved for
                                // the fork detector and the escalation below.
                                warn!(
                                    "Consensus stall for {}s at height {} with network at {} — re-engaging sync (non-destructive)",
                                    elapsed, next_height, target
                                );
                                self.stall_reengage_height = local;
                                self.sync_complete = false;
                                self.sync_machine.reset();
                                self.stall_start = None;
                            } else if target > local {
                                // Behind and a previous re-engage did not close the
                                // gap. Re-engage AGAIN rather than wiping: being
                                // behind is sync's problem, and reset_to_genesis
                                // "fixes" it by re-downloading the entire chain —
                                // the most expensive possible response, and one that
                                // destroys any block only we hold.
                                //
                                // A plain stall is NEVER destructive now. Live
                                // 2026-07-25 this path wiped two chains: 2004 blocks
                                // when peers lagged, then ~805 when the seed itself
                                // fell behind. reset_to_genesis belongs solely to the
                                // fork detector, which has actual evidence of
                                // divergence (consecutive parent mismatches) rather
                                // than mere silence.
                                warn!(
                                    "Consensus stall for {}s at height {} (network {}) — re-engaging sync again, not resetting",
                                    elapsed, next_height, target
                                );
                                self.sync_complete = false;
                                self.sync_machine.reset();
                                self.stall_start = None;
                            } else {
                                // We are at or AHEAD of every peer. A stall here
                                // means the others are behind and cannot vote for
                                // our next height — under vote-height discipline a
                                // lagging peer must refuse — so waiting is correct
                                // and destroying our chain is exactly wrong: we
                                // hold the longest one.
                                //
                                // Live 2026-07-25: the seed stalled at 2004 with
                                // peers at 1931 and 0, escalated, and wiped a
                                // 2004-block chain. The node with the most history
                                // deleted it because the others were lagging.
                                warn!(
                                    "Consensus stall for {}s at height {} but network is at {} (not ahead) — holding chain, waiting for peers",
                                    elapsed, next_height, target
                                );
                                self.stall_start = None;
                            }
                            return;
                        }
                    }
                }
                crate::consensus_manager::ConsensusRoundResult::NotReady => {
                    // Normal in-progress voting -- don't touch stall timer.
                }
            }
        }

        // Keep the consensus manager's view of the schedule fresh, so the
        // SELF-vote in try_finalize_round ranks candidates by the same
        // anti-grinding rule as the answers we give peers — even at heights
        // where no peer has queried us. This also refreshes the epoch schedule,
        // which is what logs the digest on an epoch change.
        let vs = self.consensus_cycle();
        self.consensus.set_consensus_validators(&vs);

        // Apply any newly finalized blocks (in height order).
        let mut finalized = self.consensus.finalized_heights();
        finalized.sort();
        for height in finalized {
            self.try_apply_finalized(height);
        }

        // Clean up stale consensus state below applied chain tip.
        self.consensus.cleanup_below(self.state.blocks.height());
    }

    /// If the consensus manager has a finalized block at `height`, apply it
    /// to the chain state.
    fn try_apply_finalized(&mut self, height: u64) {
        // Only apply if this is the next expected height.
        let expected = self.state.blocks.height() + 1;
        if height != expected {
            if height > expected {
                // We're behind — request missing blocks via sync protocol.
                // [1]/E6: bound the gap-request to one sync batch (MAX_SYNC_GAP).
                // This is the TWIN of the apply_synced_block loop that E6 alone
                // does NOT clamp; finding [1] requires both. `height` here is a
                // finalized height (needs Snowball quorum to reach) but is clamped
                // identically for symmetry and defense in depth.
                let gap_end = height.min(expected.saturating_add(commputer::sync_machine::MAX_SYNC_GAP));
                for h in expected..gap_end {
                    self.request_block(h);
                }
            }
            return;
        }

        // take_finalized CONSUMES the round, so it must be called exactly once.
        // The _with_lost variant also surrenders every tx that was packed into
        // a LOSING candidate at this height — without requeueing those, a tx
        // that rode a losing proposal is destroyed (the proposer mem::take'd
        // it out of its mempool and no other pool prunes it back in).
        let finalized = self.consensus.take_finalized_with_lost(height);
        // A quorum can settle on a hash whose BODY never reached us (votes carry
        // hashes, not blocks). take_finalized deliberately keeps such a round
        // intact rather than destroying the quorum — but the body has to be
        // fetched or the node waits on it forever, a permanent stall instead of
        // a self-clearing one.
        if finalized.is_none() && self.consensus.finalized_at_height(height).is_some() {
            debug!("Finalized hash at height {} but no body — requesting it", height);
            self.request_block(height);
            return;
        }

        if let Some((block, lost_txs)) = finalized {
            let hash = block.hash();

            // Fork detection: check if this block's parent matches our chain tip.
            let our_tip_hash = self.state.blocks.latest()
                .map(|b| b.hash())
                .unwrap_or(commputer_core::block::BlockHash::GENESIS);

            if block.header.parent_hash != our_tip_hash {
                warn!("Fork detected at height {}: parent {} != our tip {}",
                    height, block.header.parent_hash, our_tip_hash);

                self.fork_detector.record_mismatch();

                if self.fork_detector.should_resync() {
                    self.initiate_chain_resync(&format!(
                        "fork detector triggered after {} consecutive mismatches at height {}",
                        self.fork_detector.consecutive_mismatches(), height
                    ));
                }
                return;
            }

            // Normal path: block extends our chain.
            for tx in &block.transactions {
                self.seen_tx_hashes.insert(tx.hash());
            }

            if let Some(prev) = self.state.blocks.latest() {
                let delta = block.header.timestamp.saturating_sub(prev.header.timestamp);
                if delta > 0 {
                    debug!("Block time: {}s (target: 2s)", delta);
                    if delta > 10 {
                        warn!("Block time drift: {}s between blocks {} and {}", delta, height - 1, height);
                    }
                }
            }

            match self.state.apply_block_validated(&block) {
                Ok(()) => {
                    info!("Finalized and applied block {} at height {}", hash, height);
                    self.fork_detector.record_success();
                    self.stall_start = None;
                    self.health_monitor.record_block(height, block.header.timestamp, FinalizeMethod::Snowball);
                    self.last_block_seen_time = Some(std::time::Instant::now());
                    self.voted_peers.clear();
                    self.print_status();

                    // Feature 241: Broadcast block event to WebSocket clients.
                    self.broadcast_ws_event(&serde_json::json!({
                        "type": "new_block",
                        "height": height,
                        "hash": hex::encode(hash.0),
                        "tx_count": block.transactions.len(),
                        "timestamp": block.header.timestamp,
                    }));

                    // Push receipts to RPC state.
                    if let Some(ref rpc) = self.rpc_state
                        && let Ok(mut rcpt_guard) = rpc.receipts.try_lock() {
                            for tx in &block.transactions {
                                let tx_hash_hex = hex::encode(tx.hash().0);
                                if let Some(receipt) = self.state.receipts.get(&tx.hash())
                                    && let Ok(json) = serde_json::to_value(receipt) {
                                        rcpt_guard.insert(tx_hash_hex, json);
                                    }
                            }
                        }

                    // Auto-snapshot every 100 blocks — into the data dir (CWD is
                    // read-only under systemd ProtectSystem=strict).
                    if height.is_multiple_of(100) && height > 0
                        && let Some(ref dir) = self.data_dir {
                            let snap_path = dir.join(format!("snapshot-{}.json", height));
                            if let Err(e) = self.state.save_snapshot(&snap_path) {
                                warn!("Failed to save snapshot at height {}: {}", height, e);
                            }
                        }

                    // Prune finalized txs from the local mempool, and requeue
                    // this height's losing-candidate txs (capped + re-validated;
                    // see requeue_lost_txs). BOTH must run before
                    // process_orphans: that call recurses into try_apply_finalized
                    // for H+1, whose own requeue validates against this pool —
                    // if H's applied txs were still sitting here, they would
                    // inflate the sender's pending count and cause H+1's restore
                    // to be rejected.
                    let block_tx_hashes: HashSet<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
                    self.pending_txs.retain(|tx| !block_tx_hashes.contains(&tx.hash()));
                    for h in &block_tx_hashes {
                        self.mempool_added_at.remove(h);
                    }
                    self.requeue_lost_txs(&block_tx_hashes, lost_txs);

                    // Feature 127: Check for orphaned blocks that can now be processed.
                    self.process_orphans(hash);

                    // Track-2 (Phase B): feed the PoUW loops the just-applied state (no-op unless attached + bonded).
                    self.push_executor_snapshot();
                    self.push_verifier_snapshot();
                }
                Err(e) => {
                    warn!("Rejected finalized block {}: {}", hash, e);
                }
            }
        }
    }

    /// Request a block at a specific height from the network.
    /// Request a block via the dedicated sync protocol (direct peer-to-peer).
    /// Falls back to gossipsub BlockRequest if no peers support sync.
    pub fn request_block(&mut self, height: u64) {
        // Deduplicate by height. Callers include two gap loops that each fire
        // up to MAX_SYNC_GAP requests per received proposal, with no backoff —
        // un-deduplicated that is ~18 requests/second at a single peer, which
        // exhausts its sync rate limit and starves the sync machine's own bulk
        // batches, so the node never actually receives the chain. Live
        // 2026-07-25: a rejoining validator sent 1360 requests in 75s, got zero
        // blocks back, and sat at height 0 while the tip passed 1900.
        let now = std::time::Instant::now();
        self.block_request_at
            .retain(|_, t| now.duration_since(*t) < Duration::from_secs(30));
        if let Some(prev) = self.block_request_at.get(&height)
            && now.duration_since(*prev) < Duration::from_secs(5)
        {
            return;
        }
        self.block_request_at.insert(height, now);

        // Try the sync protocol first — direct request to a connected peer.
        let peers: Vec<libp2p::PeerId> = self.peer_ips.keys().copied().collect();
        if let Some(&peer) = peers.first() {
            let request = commputer_network::sync_protocol::SyncRequest::GetBlock { height };
            self.network.swarm.behaviour_mut().sync.send_request(&peer, request);
            debug!("Sync: requested block {} from peer {}", height, peer);
        } else {
            // No peers — fall back to gossipsub broadcast.
            let msg = ConsensusMessage::BlockRequest { height };
            self.publish_consensus_message(&msg);
            debug!("Gossipsub: requested block at height {}", height);
        }
    }

    /// Apply a block received via the sync protocol directly to chain state.
    /// Synced blocks are already consensus-finalized — no Snowball needed.
    /// Blocks must arrive in order (height == tip+1) or they are buffered as orphans.
    fn apply_synced_block(&mut self, block: Block, source: libp2p::PeerId) {
        let height = block.height();
        let hash = block.hash();

        if self.state.blocks.contains(&hash) {
            return; // Already have it.
        }

        if !self.validate_block_from_peer(&block, source) {
            warn!("Sync: rejected invalid block {} at height {}", hash, height);
            return;
        }

        let expected = self.state.blocks.height() + 1;
        // [1]: reject implausibly-far-ahead synced blocks. `height` is an
        // attacker-controlled header field; a bogus (e.g. u64::MAX) height would
        // otherwise pollute the orphan pool and drive the gap-request path. A
        // genuinely far-behind node catches up via the bounded sync_machine
        // batches, never via an unsolicited jump this large.
        if height > expected.saturating_add(commputer::sync_machine::MAX_SYNC_TARGET_GAP) {
            warn!("Sync: dropping implausibly-far-ahead block {} at height {} (tip {})",
                hash, height, expected - 1);
            return;
        }
        if height != expected {
            if height > expected {
                // Out of order — buffer as orphan, request missing blocks.
                debug!("Sync: buffering block at height {} (expected {})", height, expected);
                // SECURITY[13] (P2 merge): per-parent + total orphan caps (was
                // orphan_pool.len() < 100). Note apply_synced_block is the direct-
                // sync path; its blocks are consensus-finalized upstream.
                commputer::block_maps::bounded_orphan_insert(
                    &mut self.orphan_pool,
                    block.header.parent_hash,
                    block,
                );
                // [1]/E6: bound the gap-request to one sync batch (MAX_SYNC_GAP).
                // Unbounded `for h in expected..height` runs ~1.8e19 iterations
                // for height=u64::MAX (permanent event-loop freeze + outbound
                // GetBlock flood). Bulk catch-up is the sync_machine's job.
                let gap_end = height.min(expected.saturating_add(commputer::sync_machine::MAX_SYNC_GAP));
                for h in expected..gap_end {
                    self.request_block(h);
                }
            }
            return;
        }

        // Mark txs as seen.
        for tx in &block.transactions {
            self.seen_tx_hashes.insert(tx.hash());
        }

        match self.state.apply_block_validated(&block) {
            Ok(()) => {
                info!("Sync: applied block {} at height {}", hash, height);
                self.last_block_seen_time = Some(std::time::Instant::now());
                // A sync-applied block is chain progress: without this the 62s
                // consensus-stall timer fires mid-catch-up (live 2026-07-24: it
                // triggered a destructive resync 6s after block 9 applied).
                self.stall_start = None;
                self.print_status();

                // Broadcast to WebSocket clients.
                self.broadcast_ws_event(&serde_json::json!({
                    "type": "new_block",
                    "height": height,
                    "hash": hex::encode(hash.0),
                    "tx_count": block.transactions.len(),
                    "timestamp": block.header.timestamp,
                }));

                // Prune this block's txs from the mempool. The consensus path
                // has always done this; the sync path never did, so an applied
                // tx could linger in the pool — inflating its sender's pending
                // count and making the requeue below reject that sender's
                // restored txs.
                let block_tx_hashes: HashSet<TxHash> =
                    block.transactions.iter().map(|tx| tx.hash()).collect();
                self.pending_txs.retain(|tx| !block_tx_hashes.contains(&tx.hash()));
                for h in &block_tx_hashes {
                    self.mempool_added_at.remove(h);
                }

                // Requeue-on-loss, sync twin: a height applied via sync never
                // goes through take_finalized, so cleanup_below would destroy
                // any losing candidates still held here — the loss path a
                // WAN-lagged node hits most, since its recovery IS sync.
                let lost_txs = self.consensus.surrender_lost_at(height, &hash);
                self.requeue_lost_txs(&block_tx_hashes, lost_txs);

                // Process any orphans that can now be applied (after the prune
                // + requeue above, for the same reason as the consensus path).
                self.process_orphans(hash);

                // Track-2 (Phase B): feed the PoUW loops the just-applied state.
                self.push_executor_snapshot();
                self.push_verifier_snapshot();
            }
            Err(e) => {
                warn!("Sync: failed to apply block {} at height {}: {}", hash, height, e);
                // A parent mismatch on the SYNC path means this block builds on
                // a chain we do not have — we are on a divergent fork. Only the
                // consensus path fed the fork detector, and a forked node is by
                // definition BEHIND, so the majority's blocks arrive here
                // instead: the divergence was invisible, the node retried the
                // same block forever, and rejoining required archiving its data
                // directory by hand. Live 2026-07-25/26 a validator stranded
                // three times this way, and a harness node that finalized a
                // different block 26 after a mass restart never came back.
                //
                // Repeated mismatches while peers keep handing us blocks is
                // EVIDENCE of a fork, which is the one case reset_to_genesis is
                // reserved for (a plain stall is silence, and silence is not
                // evidence — see the stall path).
                if matches!(&e, commputer_storage::state::StateError::InvalidBlock(m)
                    if m.contains("parent hash mismatch"))
                {
                    self.fork_detector.record_mismatch();
                    if self.fork_detector.should_resync() {
                        self.initiate_chain_resync(&format!(
                            "fork detector: {} consecutive parent mismatches syncing height {}",
                            self.fork_detector.consecutive_mismatches(), height
                        ));
                    }
                }
            }
        }
    }

    /// Publish a ConsensusMessage on the consensus gossipsub topic.
    /// Feature 173: Compress before publishing.
    fn publish_consensus_message(&mut self, msg: &ConsensusMessage) {
        if let Ok(data) = serde_json::to_vec(msg) {
            let compressed = commputer_network::compress(&data);
            let topic = topics::consensus_topic();
            if let Err(e) = self
                .network
                .swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic, compressed)
            {
                debug!("Failed to publish consensus message: {}", e);
            }
        }
    }

    fn handle_proof_tick(&mut self) {
        // Keep proof manager aware of current chain height for timeout detection.
        self.proof_manager.current_height = self.state.blocks.height();
        let seed = self.state.blocks.latest()
            .map(|b| b.hash().0)
            .unwrap_or([0u8; 32]);
        let deadline = self.state.blocks.height() + 100;

        // Challenge ourselves (in a real multi-node network, challenge all known validators)
        let challenges = self.proof_manager.generate_challenges(
            self.state.current_epoch, &seed, *self.wallet.address(), deadline,
        );

        for challenge in &challenges {
            // Broadcast challenge
            let msg = ProofMessage::Challenge(challenge.clone());
            self.publish_proof_message(&msg);

            // Solve off the event-loop task to keep the libp2p swarm poll responsive.
            // The result is sent back via solver_response_tx and handled in the
            // dedicated select! arm below.
            if challenge.target == *self.wallet.address() {
                let challenge_clone = challenge.clone();
                let storage_data = self.proof_manager.storage_data_clone();
                let our_address = *self.wallet.address();
                let tx = self.solver_response_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let response = ProofManager::solve_challenge_pure(
                        &challenge_clone, &storage_data, our_address,
                    );
                    // Receiver drop is the only failure mode; ignore it.
                    let _ = tx.send(response);
                });
            }
        }

        info!("Proof challenges issued and solved for epoch {}", self.state.current_epoch);
    }

    fn handle_proof_message(&mut self, msg: ProofMessage) {
        match msg {
            ProofMessage::Challenge(challenge) => {
                if challenge.target == *self.wallet.address() {
                    debug!("Received proof challenge for {:?}", challenge.channel);
                    // Same off-runtime pattern as handle_proof_tick: spawn_blocking the
                    // PoW work so the swarm-arm of the select! stays responsive.
                    // Result returns via solver_response_tx and is recorded + published
                    // by the dedicated select! arm.
                    let challenge_clone = challenge.clone();
                    let storage_data = self.proof_manager.storage_data_clone();
                    let our_address = *self.wallet.address();
                    let tx = self.solver_response_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let response = ProofManager::solve_challenge_pure(
                            &challenge_clone, &storage_data, our_address,
                        );
                        let _ = tx.send(response);
                    });
                }
            }
            ProofMessage::Response(response) => {
                debug!("Received proof response from {:?}", response.validator);
                // Feature 113: Cross-node proof verification — verify before accepting.
                if let Some(challenge) = self.proof_manager.get_pending_challenge(&response.challenge_id) {
                    let verdict = commputer_proofs::ProofVerifier::verify(&challenge, &response);
                    match verdict {
                        commputer_core::proof::ProofVerdict::Valid |
                        commputer_core::proof::ProofVerdict::Suspicious => {
                            self.proof_manager.record_response(response);
                        }
                        _ => {
                            warn!("Rejected invalid proof response from {:?}", response.validator);
                            // Broadcast rejection so other nodes know.
                            let rejection = ProofMessage::Rejection {
                                challenge_id: response.challenge_id,
                                validator: response.validator,
                                reason: format!("{:?}", verdict),
                            };
                            self.publish_proof_message(&rejection);
                        }
                    }
                } else {
                    // Unknown challenge — still record for aggregation.
                    self.proof_manager.record_response(response);
                }
            }
            ProofMessage::Rejection { challenge_id: _, validator, reason } => {
                debug!("Received proof rejection for {:?}: {}", validator, reason);
                // Could track rejection counts per validator for reputation.
            }
        }
    }

    /// Feature 173: Compress proof messages before publishing.
    fn publish_proof_message(&mut self, msg: &ProofMessage) {
        if let Ok(data) = serde_json::to_vec(msg) {
            let compressed = commputer_network::compress(&data);
            let topic = topics::proof_topic();
            if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, compressed) {
                debug!("Failed to publish proof message: {}", e);
            }
        }
    }

    fn print_status(&self) {
        let circulating = self.state.circulating_supply() / UNITS_PER_COMME;
        let burned = self.state.total_burned / UNITS_PER_COMME;
        info!(
            "Chain: height={}, circulating={} COMME, burned={} COMME, accounts={}",
            self.state.blocks.height(),
            circulating,
            burned,
            self.state.accounts.len(),
        );
    }

    /// Feature 169: Peer exchange — share known peer addresses periodically.
    ///
    /// Replaced by peer_exchange_fix logic: broadcasts ALL known peer addresses
    /// (not just our own) so that nodes in a 3+ node network can discover each
    /// other through a shared seed rather than only learning about the seed itself.
    fn handle_peer_exchange_tick(&mut self) {
        // Build our own listen addresses.
        let our_addrs: Vec<String> = self.network.swarm.listeners()
            .map(|a| a.to_string())
            .collect();

        // Build the peer map: "us" + each connected peer with their addresses.
        let mut peers: HashMap<String, Vec<String>> = HashMap::new();

        // Add ourselves.
        peers.insert("us".to_string(), our_addrs.clone());

        // Add connected peers — this is the key fix: previously only self was announced.
        for (peer_id, ip) in self.peer_ips.iter().take(MAX_PEERS_PER_EXCHANGE - 1) {
            let addrs = vec![
                format!("/ip4/{}/tcp/30303", ip),
                format!("/ip4/{}/udp/30303/quic-v1", ip),
            ];
            peers.insert(peer_id.to_string(), addrs);
        }

        debug!(
            "[peer_exchange] building message: {} peer entries (including self)",
            peers.len()
        );

        let msg = PeerExchangeMessage {
            peers,
            our_addresses: our_addrs,
        };

        if let Ok(data) = serde_json::to_vec(&msg) {
            let topic = topics::peer_addrs_topic();
            if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, data) {
                debug!("Failed to publish peer exchange: {}", e);
            }
        }
        debug!("Shared {} peer addresses via peer exchange", self.peer_ips.len().min(MAX_PEERS_PER_EXCHANGE));
    }

    /// Feature 171: Send ping to all connected peers for latency measurement.
    fn handle_ping_tick(&mut self) {
        let now = std::time::Instant::now();
        let peer_ids: Vec<libp2p::PeerId> = self.peer_ips.keys().copied().collect();
        for peer_id in peer_ids {
            self.ping_timestamps.insert(peer_id, now);
        }
        // Broadcast a ping message with current timestamp.
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let msg = commputer_network::message::NetworkMessage {
            sender: commputer_network::peer::PeerId([0u8; 32]),
            nonce: timestamp_ms,
            kind: commputer_network::message::MessageKind::Ping { timestamp_ms },
        };

        if let Ok(data) = serde_json::to_vec(&msg) {
            let compressed = commputer_network::compress(&data);
            let topic = topics::consensus_topic();
            if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, compressed) {
                debug!("Failed to publish ping: {}", e);
            }
        }

        // Calculate average RTT from existing measurements.
        if !self.peer_rtts.is_empty() {
            let total: u64 = self.peer_rtts.values().sum();
            let avg = total / self.peer_rtts.len() as u64;
            debug!("Average peer RTT: {}ms across {} peers", avg, self.peer_rtts.len());
        }
    }

    /// Feature 172: Check for network partition.
    fn check_network_partition(&mut self) {
        let peer_count = self.peer_ips.len();
        let was_partitioned = self.partition_detected;

        if peer_count < MINIMUM_PEERS {
            if !was_partitioned {
                error!(
                    "CRITICAL: Network partition detected! Only {} peers connected (minimum: {}). Block production paused.",
                    peer_count, MINIMUM_PEERS
                );
                self.partition_detected = true;
            }
        } else if was_partitioned {
            info!("Network partition resolved. {} peers connected. Resuming block production.", peer_count);
            self.partition_detected = false;
        }
    }

    /// Feature 178: Reconnect to seed nodes when we have no peers.
    fn reconnect_seeds(&mut self) {
        // Seed keepalive, NOT a last-resort sweep: runs regardless of current
        // peer count and covers the compiled-in default seeds, not just
        // --seeds. The old zero-peers-only + custom-seeds-only guards meant a
        // validator holding any peer never re-dialed a restarted seed, and a
        // default-seed node had no re-dial path at all (live 2026-07-24: the
        // star topology never re-knit around the returned seed). Dedupe
        // against live connections, per-seed backoff, and self-dial skip all
        // live in the network crate.
        let dialed = self.network.ensure_seed_connections(&self.custom_seeds);
        if dialed > 0 {
            info!("Seed keepalive: dialed {} seed(s)", dialed);
        }
    }

    /// Feature 170: Count how many peers are in each /16 subnet.
    fn count_subnets(&self) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for subnet in self.peer_subnets.values() {
            *counts.entry(subnet.clone()).or_insert(0) += 1;
        }
        counts
    }

    /// Feature 170: Find a peer from a /16 subnet that has more than 1 peer.
    fn find_duplicate_subnet_peer(&self, exclude: &libp2p::PeerId) -> Option<libp2p::PeerId> {
        let counts = self.count_subnets();
        for (peer_id, subnet) in &self.peer_subnets {
            if peer_id != exclude
                && let Some(&count) = counts.get(subnet)
                    && count > 1 {
                        return Some(*peer_id);
                    }
        }
        None
    }

    /// Feature 177: Get average latency across all peers.
    pub fn average_peer_latency(&self) -> u64 {
        if self.peer_rtts.is_empty() {
            return 0;
        }
        let total: u64 = self.peer_rtts.values().sum();
        total / self.peer_rtts.len() as u64
    }

    /// Feature 170: Get count of unique /16 subnets.
    pub fn unique_subnet_count(&self) -> usize {
        let subnets: HashSet<&String> = self.peer_subnets.values().collect();
        subnets.len()
    }

    /// Feature 180: Compute partition risk level based on peer count.
    pub fn partition_risk(&self) -> &'static str {
        let peer_count = self.peer_ips.len();
        if peer_count < MINIMUM_PEERS {
            "high"
        } else if peer_count < 5 {
            "medium"
        } else {
            "low"
        }
    }
}

/// Extract an IPv4 or IPv6 address from a multiaddr string.
/// Multiaddrs look like: /ip4/192.168.1.10/tcp/9000
fn extract_ip_from_multiaddr(addr: &str) -> Option<String> {
    let parts: Vec<&str> = addr.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if (*part == "ip4" || *part == "ip6") && i + 1 < parts.len() {
            return Some(parts[i + 1].to_string());
        }
        // Connections dialed via the compiled-in DNS seed default carry a
        // /dns4-style remote address, not /ip4. The hostname is a valid peer
        // identifier for tracking: without it a DNS-dialed node never enters
        // peer_ips, the sync driver never queries heights, and the 30s solo
        // fallback wedges the node at height 0 (observed live, 2026-07-24).
        if (*part == "dns4" || *part == "dns6" || *part == "dns" || *part == "dnsaddr")
            && i + 1 < parts.len()
        {
            return Some(parts[i + 1].to_string());
        }
    }
    None
}

/// Feature 170: Extract /16 subnet from an IP address (first two octets for IPv4).
fn extract_slash16_subnet(ip: &str) -> String {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        ip.to_string()
    }
}

/// Feature 167: Check if an IP address is private (behind NAT).
fn is_private_ip(ip: &str) -> bool {
    if let Ok(addr) = ip.parse::<std::net::Ipv4Addr>() {
        addr.is_private() || addr.is_loopback() || addr.is_link_local()
    } else {
        false
    }
}

// ── Mainnet readiness: Additional features ──

/// Feature 15: Validator performance history.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ValidatorPerformance {
    pub blocks_produced: u64,
    pub proofs_passed: u64,
    pub uptime_secs: u64,
    pub epochs_active: u64,
}

/// Feature 9: Validator registration cooldown height.
#[allow(dead_code)]
pub const VALIDATOR_COOLDOWN_BLOCKS: u64 = 10;

/// Feature 16: Network bootstrap — on first start, request state snapshot.
/// Falls back to block-by-block sync (already implemented via sync protocol).
#[allow(dead_code)]
pub fn bootstrap_note() {
    tracing::info!("Feature 16: Network bootstrap optimization — requesting state snapshot from peers");
    tracing::info!("Falling back to block-by-block sync if snapshot unavailable");
}

/// Feature 20: Config hot reload placeholder — log level can be changed via SIGHUP.
/// Full implementation requires tracing_subscriber::reload which needs the reload layer.
#[allow(dead_code)]
pub fn config_reload_note() {
    tracing::info!("Feature 20: Config hot reload — send SIGHUP to reload log level");
}

#[cfg(test)]
mod multiaddr_ip_tests {
    use super::extract_ip_from_multiaddr;

    #[test]
    fn extracts_ip4_and_ip6() {
        assert_eq!(
            extract_ip_from_multiaddr("/ip4/174.138.35.16/tcp/9000/p2p/12D3KooWabc"),
            Some("174.138.35.16".to_string())
        );
        assert_eq!(
            extract_ip_from_multiaddr("/ip6/::1/udp/9000/quic-v1"),
            Some("::1".to_string())
        );
    }

    #[test]
    fn extracts_dns_seed_hostname() {
        // The exact remote-address shape of a compiled-DNS-default dial — the
        // 2026-07-24 launch-night wedge: this returning None kept the seed out
        // of peer_ips, so sync never started and the node stuck at height 0.
        assert_eq!(
            extract_ip_from_multiaddr("/dns4/seed.commputer.xyz/udp/9000/quic-v1"),
            Some("seed.commputer.xyz".to_string())
        );
        assert_eq!(
            extract_ip_from_multiaddr("/dns4/seed.commputer.xyz/tcp/9000"),
            Some("seed.commputer.xyz".to_string())
        );
        assert_eq!(
            extract_ip_from_multiaddr("/dnsaddr/seed.commputer.xyz"),
            Some("seed.commputer.xyz".to_string())
        );
    }

    #[test]
    fn no_address_component_returns_none() {
        assert_eq!(extract_ip_from_multiaddr("/p2p/12D3KooWabc"), None);
        assert_eq!(extract_ip_from_multiaddr(""), None);
    }
}
