use std::collections::{HashMap, HashSet};
use commputer_core::transaction::TxHash;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;
use tracing::{info, warn, debug};
use futures::StreamExt;

use commputer_core::block::{Block, BlockHeader, BlockHash};
use commputer_core::identity::Address;
use commputer_core::transaction::Transaction;
use commputer_core::token::UNITS_PER_COMME;
use commputer_core::wallet::Wallet;

use commputer_consensus::emission::{EmissionSchedule, ChannelAllocation};
use commputer_consensus::epoch::EpochState;

use commputer_storage::state::ChainState;

use commputer_network::transport::{CommpNetwork, CommpBehaviourEvent};
use commputer_network::topics;

use commputer_validator::lifecycle::{ValidatorState, ValidatorStatus};
use commputer_validator::compliance_check::ComplianceChecker;

use crate::consensus_manager::{ConsensusManager, ConsensusMessage};
use crate::proof_manager::{ProofManager, ProofMessage};

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
    pub rpc_rx: Option<mpsc::Receiver<Transaction>>,
    /// Shared RPC state (for updating the status snapshot).
    pub rpc_state: Option<Arc<crate::rpc::RpcState>>,
}

impl EventLoop {
    pub fn new(
        state: ChainState,
        wallet: Wallet,
        network: CommpNetwork,
    ) -> Self {
        let epoch_state = EpochState::new(0, 0);
        let our_address = *wallet.address();
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
            peer_ips: HashMap::new(),
            peer_validators: HashMap::new(),
            banned_peers: HashSet::new(),
            seen_tx_hashes: HashSet::new(),
            peer_msg_rates: HashMap::new(),
            peer_scores: HashMap::new(),
            rpc_rx: None,
            rpc_state: None,
        }
    }

    /// Attach the RPC channel and shared state for the RPC server.
    pub fn attach_rpc(
        &mut self,
        rx: mpsc::Receiver<Transaction>,
        state: Arc<crate::rpc::RpcState>,
    ) {
        self.rpc_rx = Some(rx);
        self.rpc_state = Some(state);
        self.update_rpc_status();
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
                    peers.push(crate::rpc::PeerInfo {
                        peer_id: peer_id.to_string(),
                        ip: Some(ip.clone()),
                        validator_address,
                        compliance_status,
                    });
                }
                *peers_guard = peers;
            }

            // Update recent blocks for explorer endpoint.
            if let Ok(mut blk_guard) = rpc.blocks.try_lock() {
                let height = self.state.blocks.height();
                // Add any new blocks not yet tracked.
                let start = if height > 100 { height - 100 } else { 0 };
                for h in start..=height {
                    if !blk_guard.contains_key(&h) {
                        if let Some(block) = self.state.blocks.get_by_height(h) {
                            if let Ok(json) = serde_json::to_value(block) {
                                blk_guard.insert(h, json);
                            }
                        }
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

            // Update node metrics.
            if let Ok(mut met_guard) = rpc.metrics.try_lock() {
                met_guard.height = self.state.blocks.height();
                met_guard.epoch = self.state.current_epoch;
                met_guard.peers_connected = self.peer_ips.len();
                met_guard.peers_banned = self.banned_peers.len();
                met_guard.pending_txs = self.pending_txs.len();
                met_guard.seen_tx_count = self.seen_tx_hashes.len();
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
            if let Some(validator_addr) = self.peer_validators.remove(&peer_id) {
                self.compliance.deregister_node(&validator_addr);
            }
        }
    }

    pub async fn run(&mut self) {
        let mut epoch_interval = time::interval(Duration::from_secs(3600));
        let mut block_interval = time::interval(Duration::from_secs(2));
        let mut consensus_interval = time::interval(Duration::from_millis(500));
        let mut proof_interval = time::interval(Duration::from_secs(300));

        info!("Event loop started at height {}. Listening for peers...", self.state.blocks.height());

        // Initial sync: request any missing blocks we might have missed.
        // This runs once at startup after peers connect.
        let mut sync_requested = false;
        let mut sync_timer = time::interval(Duration::from_secs(5));

        // Set up graceful shutdown signal handler.
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ).expect("failed to register SIGTERM handler");

        loop {
            // Take the RPC receiver out to satisfy the borrow checker in select!
            let rpc_recv = async {
                if let Some(ref mut rx) = self.rpc_rx {
                    rx.recv().await
                } else {
                    // No RPC channel — park forever.
                    std::future::pending::<Option<Transaction>>().await
                }
            };

            tokio::select! {
                event = self.network.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }
                Some(tx) = rpc_recv => {
                    info!("Received transaction from RPC: {}", hex::encode(tx.hash().0));
                    self.handle_rpc_transaction(tx);
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
                _ = sync_timer.tick() => {
                    // If we have peers but haven't synced yet, request the next block.
                    if !sync_requested && !self.peer_ips.is_empty() {
                        let our_height = self.state.blocks.height();
                        // Request the next few blocks we might be missing.
                        for h in (our_height + 1)..=(our_height + 10) {
                            self.request_block(h);
                        }
                        sync_requested = true;
                        info!("Initial sync: requested blocks {} to {}", our_height + 1, our_height + 10);
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("Received SIGINT — shutting down gracefully");
                    self.shutdown();
                    return;
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM — shutting down gracefully");
                    self.shutdown();
                    return;
                }
            }
        }
    }

    /// Flush state to disk and clean up before exit.
    fn shutdown(&mut self) {
        info!("Flushing chain state to disk...");
        if let Err(e) = self.state.flush() {
            warn!("Failed to flush state on shutdown: {}", e);
        } else {
            info!("Chain state flushed successfully. Height: {}", self.state.blocks.height());
        }
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
                        self.ban_peer(propagation_source, "exceeded message rate limit");
                        return;
                    }
                }

                let topic = message.topic.as_str();
                debug!("Gossipsub message on topic: {} from {}", topic, propagation_source);

                if topic == topics::TOPIC_BLOCKS {
                    if let Ok(block) = serde_json::from_slice::<Block>(&message.data) {
                        self.handle_received_block(block, propagation_source);
                    }
                } else if topic == topics::TOPIC_TRANSACTIONS {
                    if let Ok(tx) = serde_json::from_slice::<Transaction>(&message.data) {
                        self.handle_new_transaction(tx, propagation_source);
                    }
                } else if topic == topics::TOPIC_CONSENSUS {
                    if let Ok(msg) = serde_json::from_slice::<ConsensusMessage>(&message.data) {
                        self.handle_consensus_message(msg, propagation_source);
                    }
                } else if topic == topics::TOPIC_PROOFS {
                    if let Ok(msg) = serde_json::from_slice::<ProofMessage>(&message.data) {
                        self.handle_proof_message(msg);
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
                // Reject connections from banned peers.
                if self.banned_peers.contains(&peer_id) {
                    info!("Rejecting connection from banned peer {}", peer_id);
                    let _ = self.network.swarm.disconnect_peer_id(peer_id);
                    return;
                }
                // Extract the IP address from the multiaddr.
                let addr_str = endpoint.get_remote_address().to_string();
                if let Some(ip) = extract_ip_from_multiaddr(&addr_str) {
                    self.peer_ips.insert(peer_id, ip.clone());
                    // If we know this peer's validator address, register with compliance.
                    if let Some(validator_addr) = self.peer_validators.get(&peer_id) {
                        self.compliance.register_node(*validator_addr, ip);
                    }
                }
                // Enforce connection limit: max 50 peers.
                const MAX_PEERS: usize = 50;
                if self.peer_ips.len() >= MAX_PEERS {
                    info!("Connection limit reached ({}) — disconnecting new peer {}", MAX_PEERS, peer_id);
                    let _ = self.network.swarm.disconnect_peer_id(peer_id);
                    return;
                }

                info!("Connected to peer: {} at {}", peer_id, addr_str);
                // Initialize peer reputation score.
                self.peer_scores.entry(peer_id).or_insert(100);
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
                }
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                // Clean up peer tracking.
                self.peer_ips.remove(&peer_id);
                if let Some(validator_addr) = self.peer_validators.remove(&peer_id) {
                    self.compliance.deregister_node(&validator_addr);
                }
                self.peer_scores.remove(&peer_id);
                // Drain grace period for disconnected validators.
                if let Some(validator_addr) = self.peer_validators.get(&peer_id) {
                    if let Some(account) = self.state.accounts.get_mut(validator_addr) {
                        // Drain 1 epoch's worth of grace (3600s) on disconnect.
                        account.drain_grace(3600);
                        debug!("Drained grace for disconnected validator {}", validator_addr);
                    }
                }
                info!("Disconnected from peer: {}", peer_id);
            }
            _ => {}
        }
    }

    /// Handle a consensus protocol message from a peer.
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

                debug!("Received block candidate {} at height {}", hash, height);
                self.consensus.add_candidate(block);

                // Attempt finalization (handles single-candidate fast-path).
                self.consensus.try_finalize_round(height);
                self.try_apply_finalized(height);
            }
            ConsensusMessage::SnowballQuery { height, querier_preference: _ } => {
                // Respond with our preference for this height.
                if let Some(pref) = self.consensus.query_preference(height) {
                    let response = ConsensusMessage::SnowballResponse {
                        height,
                        preference: pref,
                    };
                    self.publish_consensus_message(&response);
                }
            }
            ConsensusMessage::SnowballResponse { height, preference } => {
                self.consensus.record_response(height, preference);
            }
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
                    self.consensus.add_candidate(block);
                    self.consensus.try_finalize_round(height);
                    self.try_apply_finalized(height);
                }
            }
            ConsensusMessage::BlockResponse { block: None, requested_height } => {
                debug!("Peer doesn't have block at height {}", requested_height);
            }
        }
    }

    /// Validate a block received from a peer. Returns false and bans the peer
    /// if the block has bad merkle roots, invalid signatures, or exceeds size limits.
    fn validate_block_from_peer(&mut self, block: &Block, source: libp2p::PeerId) -> bool {
        // Check block size limits.
        if !block.within_size_limits() {
            self.ban_peer(source, "sent oversized block");
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

        // Timestamp must be >= parent block timestamp (no going backward).
        if let Some(parent) = self.state.blocks.latest() {
            if block.header.timestamp < parent.header.timestamp {
                self.ban_peer(source, "sent block with timestamp before parent");
                return false;
            }
        }

        // Check merkle roots.
        if !block.verify_roots() {
            self.ban_peer(source, "sent block with invalid merkle roots");
            return false;
        }

        // Verify all transaction signatures in the block.
        for tx in &block.transactions {
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

        // Block passed all checks — reward the peer.
        self.adjust_peer_score(source, 1);
        true
    }

    /// A block received on the blocks topic (legacy path). Enter it as a candidate
    /// instead of applying directly.
    fn handle_received_block(&mut self, block: Block, source: libp2p::PeerId) {
        let hash = block.hash();
        let height = block.height();

        if self.state.blocks.contains(&hash) {
            return; // Already have this block.
        }

        // Validate before accepting.
        if !self.validate_block_from_peer(&block, source) {
            return;
        }

        debug!("Received block {} at height {} — entering as candidate", hash, height);
        self.consensus.add_candidate(block);

        // Attempt finalization (handles single-candidate fast-path).
        self.consensus.try_finalize_round(height);
        self.try_apply_finalized(height);
    }

    /// Handle a transaction submitted via the RPC server: validate, add to mempool, broadcast.
    fn handle_rpc_transaction(&mut self, tx: Transaction) {
        if let Err(reason) = self.validate_tx_for_mempool(&tx) {
            warn!("RPC transaction rejected: {}", reason);
            return;
        }

        let tx_hash = tx.hash();
        self.seen_tx_hashes.insert(tx_hash);

        // Broadcast on the transactions gossipsub topic.
        if let Ok(data) = serde_json::to_vec(&tx) {
            let topic = topics::tx_topic();
            if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, data) {
                warn!("Failed to broadcast RPC transaction: {}", e);
            }
        }

        info!("Broadcast transaction {} from RPC", hex::encode(tx_hash.0));
        self.pending_txs.push(tx);
        self.update_rpc_status();
    }

    /// Validate a transaction for mempool admission: signature, nonce, dedup.
    fn validate_tx_for_mempool(&self, tx: &Transaction) -> Result<(), &'static str> {
        if tx.from.0 == [0u8; 32] {
            return Err("null sender");
        }
        if !tx.verify() {
            return Err("signature verification failed");
        }
        // Reject duplicates (already in mempool or finalized).
        let hash = tx.hash();
        if self.seen_tx_hashes.contains(&hash) {
            return Err("duplicate transaction");
        }
        // Minimum fee check.
        if tx.fee < commputer_core::transaction::MINIMUM_FEE {
            return Err("fee below minimum");
        }
        // Nonce validation: must match expected next nonce for sender.
        // Account for pending txs already in mempool from the same sender.
        let on_chain_nonce = self.state.accounts
            .get(&tx.from)
            .map(|a| a.nonce)
            .unwrap_or(0);
        let pending_from_sender = self.pending_txs.iter()
            .filter(|ptx| ptx.from == tx.from)
            .count() as u64;
        let expected_nonce = on_chain_nonce + pending_from_sender;
        if tx.nonce != expected_nonce {
            return Err("invalid nonce");
        }
        Ok(())
    }

    fn handle_new_transaction(&mut self, tx: Transaction, source: libp2p::PeerId) {
        if let Err(reason) = self.validate_tx_for_mempool(&tx) {
            debug!("Rejected transaction from {}: {}", source, reason);
            return;
        }

        // If this is a ValidatorRegister tx, link the sender address to the peer.
        if matches!(tx.kind, commputer_core::transaction::TxKind::ValidatorRegister { .. }) {
            let validator_addr = tx.from;
            info!(
                "Linking validator {} to peer {} via ValidatorRegister tx",
                validator_addr, source
            );
            self.peer_validators.insert(source, validator_addr);

            // If we already know this peer's IP, register with compliance checker.
            if let Some(ip) = self.peer_ips.get(&source) {
                self.compliance.register_node(validator_addr, ip.clone());
            }
        }

        let hash = tx.hash();
        self.seen_tx_hashes.insert(hash);
        debug!("Accepted transaction into mempool: {:?}", hash);
        self.pending_txs.push(tx);
        self.enforce_mempool_limit();
    }

    /// Maximum number of transactions in the mempool.
    const MAX_MEMPOOL_SIZE: usize = 5000;

    /// Enforce mempool size limit by evicting lowest-fee transactions.
    fn enforce_mempool_limit(&mut self) {
        while self.pending_txs.len() > Self::MAX_MEMPOOL_SIZE {
            // Find the index of the lowest-fee transaction.
            if let Some((min_idx, _)) = self.pending_txs.iter()
                .enumerate()
                .min_by_key(|(_, tx)| tx.fee)
            {
                let evicted = self.pending_txs.remove(min_idx);
                debug!("Evicted low-fee tx from mempool: fee={}", evicted.fee);
            } else {
                break;
            }
        }
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

        info!(
            "Registered as validator at {}% contribution",
            contribution_percent,
        );
    }

    fn handle_epoch_tick(&mut self) {
        let epoch = self.epoch_state.epoch;
        let validator_count = self.epoch_state.validator_count() as u64;

        if validator_count == 0 {
            debug!("Epoch {} tick — no validators", epoch);
            self.epoch_state = EpochState::new(epoch + 1, 0);
            return;
        }

        let epoch_emission = self.emission.per_epoch_emission(validator_count);
        let remaining = self.state.remaining_supply();
        let actual_emission = epoch_emission.min(remaining);

        // Finalize proof results and update epoch summaries
        let proof_summaries = self.proof_manager.finalize_epoch();
        for (_addr, summary) in &proof_summaries {
            self.epoch_state.record_summary(summary.clone());
        }

        if actual_emission > 0 {
            let _allocation =
                ChannelAllocation::from_demand(actual_emission, &self.epoch_state.demand);

            // Distribute rewards based on composite resource score
            let summaries: Vec<_> = self.epoch_state.summaries.values().cloned().collect();
            let total_score: u64 = summaries.iter().map(|s| s.composite_score()).sum();

            if total_score > 0 {
                let mut distributed = 0u64;
                for summary in &summaries {
                    let score = summary.composite_score();
                    let reward = actual_emission * score / total_score;

                    if reward > 0 {
                        // Check compliance — nerfed validators earn less
                        let compliance = self.compliance.check(&summary.validator);
                        let effective_reward = match compliance {
                            commputer_core::compliance::ComplianceStatus::Compliant => reward,
                            _ => {
                                let multiplier = self.state.nerf_rate.reward_multiplier();
                                (reward as f64 * multiplier).round() as u64
                            }
                        };

                        if effective_reward > 0 {
                            let account = self.state.accounts.get_or_create(summary.validator);
                            if let Some(new_balance) = account.balance.checked_add(
                                commputer_core::token::Amount::from_raw(effective_reward),
                            ) {
                                account.balance = new_balance;
                                account.total_mined = account
                                    .total_mined
                                    .checked_add(
                                        commputer_core::token::Amount::from_raw(effective_reward),
                                    )
                                    .unwrap_or(account.total_mined);
                                distributed += effective_reward;
                            }
                        }
                    }
                }

                info!(
                    "Epoch {} complete: {} validators, emitted {:.4} COMME, distributed to {} accounts",
                    epoch,
                    validator_count,
                    distributed as f64 / UNITS_PER_COMME as f64,
                    summaries.len(),
                );

                self.state.emit(distributed);

                // Warn on low remaining supply.
                let remaining_pct = (self.state.remaining_supply() as f64
                    / commputer_core::token::TOTAL_SUPPLY as f64) * 100.0;
                if self.state.is_emergency_access() {
                    warn!("EMERGENCY ACCESS MODE: circulating supply below 1M COMME — any contribution = full access");
                }
                if remaining_pct <= 1.0 {
                    warn!("CRITICAL: Only {:.2}% of supply remaining ({} raw units)",
                        remaining_pct, self.state.remaining_supply());
                } else if remaining_pct <= 5.0 {
                    warn!("WARNING: Only {:.2}% of supply remaining", remaining_pct);
                } else if remaining_pct <= 10.0 {
                    info!("Supply milestone: {:.2}% remaining", remaining_pct);
                }

                // Persist updated account balances
                if let Err(e) = self.state.flush() {
                    warn!("Failed to flush state after epoch: {}", e);
                }
            }
        }

        // Refill grace period for our own validator (1 epoch = 3600s online).
        {
            let our_addr = *self.wallet.address();
            let account = self.state.accounts.get_or_create(our_addr);
            account.cumulative_uptime_secs += 3600;
            account.refill_grace(3600);
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

        self.state.current_epoch = epoch + 1;
        self.epoch_state = EpochState::new(epoch + 1, 0);
        self.epoch_state.record_summary(self_summary);
    }

    fn handle_block_tick(&mut self) {
        // Only produce blocks if we're a registered validator.
        if self.validator.status() != ValidatorStatus::Active {
            return;
        }

        let next_height = self.state.blocks.height() + 1;

        // Don't produce if there's already an active vote at this height.
        if self.consensus.has_active_vote(next_height) || self.consensus.has_height(next_height) {
            return;
        }

        let parent = self
            .state
            .blocks
            .latest()
            .map(|b| b.hash())
            .unwrap_or(BlockHash::GENESIS);

        // Create a new block with pending transactions (capped to block size limit).
        let mut all_txs = std::mem::take(&mut self.pending_txs);
        let txs: Vec<Transaction> = if all_txs.len() > commputer_core::block::MAX_TRANSACTIONS_PER_BLOCK {
            let overflow = all_txs.split_off(commputer_core::block::MAX_TRANSACTIONS_PER_BLOCK);
            self.pending_txs = overflow; // Put excess back in mempool.
            all_txs
        } else {
            all_txs
        };
        let mut block = Block {
            header: BlockHeader {
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
            },
            transactions: txs,
            proof_summaries: vec![],
        };

        // Compute and set merkle roots and state root.
        block.header.tx_root = block.compute_tx_root();
        block.header.proof_root = block.compute_proof_root();
        block.header.state_root = self.state.compute_state_root();

        // Sign the block header with our wallet key.
        commputer_core::signing::sign_block(&mut block, &self.wallet);

        info!("Produced block candidate at height {}", next_height);

        // Broadcast as BlockCandidate on the consensus topic.
        let candidate_msg = ConsensusMessage::BlockCandidate {
            block: block.clone(),
        };
        self.publish_consensus_message(&candidate_msg);

        // Also broadcast on the blocks topic for backward compatibility.
        if let Ok(data) = serde_json::to_vec(&block) {
            let topic = topics::block_topic();
            if let Err(e) = self
                .network
                .swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic, data)
            {
                warn!("Failed to broadcast block: {}", e);
            }
        }

        // Enter our own block as a candidate and start the vote.
        self.consensus.add_candidate(block);

        // Attempt finalization (handles single-candidate fast-path).
        self.consensus.try_finalize_round(next_height);
        self.try_apply_finalized(next_height);
    }

    /// Consensus round tick (500ms): for each active height, publish a query
    /// and attempt to finalize the round from accumulated responses.
    fn handle_consensus_tick(&mut self) {
        let active = self.consensus.active_heights();
        for height in &active {
            // Try to finalize from any responses accumulated so far.
            self.consensus.try_finalize_round(*height);
        }

        // Apply any newly finalized blocks (in height order).
        let mut finalized = self.consensus.finalized_heights();
        finalized.sort();
        for height in finalized {
            self.try_apply_finalized(height);
        }

        // Publish queries for still-active heights.
        let still_active = self.consensus.active_heights();
        for height in still_active {
            if let Some(pref) = self.consensus.query_preference(height) {
                let query = ConsensusMessage::SnowballQuery {
                    height,
                    querier_preference: pref,
                };
                self.publish_consensus_message(&query);
            }
        }
    }

    /// If the consensus manager has a finalized block at `height`, apply it
    /// to the chain state.
    fn try_apply_finalized(&mut self, height: u64) {
        // Only apply if this is the next expected height.
        let expected = self.state.blocks.height() + 1;
        if height != expected {
            return;
        }

        if let Some(block) = self.consensus.take_finalized(height) {
            let hash = block.hash();
            // Record all tx hashes from this block as seen (double-spend prevention).
            for tx in &block.transactions {
                self.seen_tx_hashes.insert(tx.hash());
            }

            // Log block time delta from previous block.
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
                    self.print_status();

                    // Auto-snapshot every 100 blocks.
                    if height % 100 == 0 && height > 0 {
                        let snap_path = std::path::PathBuf::from(
                            format!("snapshot-{}.json", height)
                        );
                        if let Err(e) = self.state.save_snapshot(&snap_path) {
                            warn!("Failed to save snapshot at height {}: {}", height, e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Rejected finalized block {}: {}", hash, e);
                }
            }
        }
    }

    /// Request a block at a specific height from the network.
    pub fn request_block(&mut self, height: u64) {
        let msg = ConsensusMessage::BlockRequest { height };
        self.publish_consensus_message(&msg);
        debug!("Requested block at height {}", height);
    }

    /// Publish a ConsensusMessage on the consensus gossipsub topic.
    fn publish_consensus_message(&mut self, msg: &ConsensusMessage) {
        if let Ok(data) = serde_json::to_vec(msg) {
            let topic = topics::consensus_topic();
            if let Err(e) = self
                .network
                .swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic, data)
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

            // Solve if it's for us
            if challenge.target == *self.wallet.address() {
                let response = self.proof_manager.solve_challenge(challenge);
                self.proof_manager.record_response(response.clone());
                let resp_msg = ProofMessage::Response(response);
                self.publish_proof_message(&resp_msg);
            }
        }

        info!("Proof challenges issued and solved for epoch {}", self.state.current_epoch);
    }

    fn handle_proof_message(&mut self, msg: ProofMessage) {
        match msg {
            ProofMessage::Challenge(challenge) => {
                if challenge.target == *self.wallet.address() {
                    debug!("Received proof challenge for {:?}", challenge.channel);
                    let response = self.proof_manager.solve_challenge(&challenge);
                    self.proof_manager.record_response(response.clone());
                    let resp_msg = ProofMessage::Response(response);
                    self.publish_proof_message(&resp_msg);
                }
            }
            ProofMessage::Response(response) => {
                debug!("Received proof response from {:?}", response.validator);
                self.proof_manager.record_response(response);
            }
        }
    }

    fn publish_proof_message(&mut self, msg: &ProofMessage) {
        if let Ok(data) = serde_json::to_vec(msg) {
            let topic = topics::proof_topic();
            if let Err(e) = self.network.swarm.behaviour_mut().gossipsub.publish(topic, data) {
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
}

/// Extract an IPv4 or IPv6 address from a multiaddr string.
/// Multiaddrs look like: /ip4/192.168.1.10/tcp/9000
fn extract_ip_from_multiaddr(addr: &str) -> Option<String> {
    let parts: Vec<&str> = addr.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if (*part == "ip4" || *part == "ip6") && i + 1 < parts.len() {
            return Some(parts[i + 1].to_string());
        }
    }
    None
}
