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
        }
    }

    /// Ban a peer and disconnect them.
    fn ban_peer(&mut self, peer_id: libp2p::PeerId, reason: &str) {
        if self.banned_peers.insert(peer_id) {
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

        info!("Event loop started. Listening for peers...");

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
            }
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
                info!("Connected to peer: {} at {}", peer_id, addr_str);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                // Clean up peer tracking.
                self.peer_ips.remove(&peer_id);
                if let Some(validator_addr) = self.peer_validators.remove(&peer_id) {
                    self.compliance.deregister_node(&validator_addr);
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
        }
    }

    /// Validate a block received from a peer. Returns false and bans the peer
    /// if the block has bad merkle roots or invalid transaction signatures.
    fn validate_block_from_peer(&mut self, block: &Block, source: libp2p::PeerId) -> bool {
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

                // Persist updated account balances
                if let Err(e) = self.state.flush() {
                    warn!("Failed to flush state after epoch: {}", e);
                }
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

        // Create a new block with pending transactions.
        let txs = std::mem::take(&mut self.pending_txs);
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

        // Compute and set merkle roots.
        block.header.tx_root = block.compute_tx_root();
        block.header.proof_root = block.compute_proof_root();

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
            match self.state.apply_block_validated(&block) {
                Ok(()) => {
                    info!("Finalized and applied block {} at height {}", hash, height);
                    self.print_status();
                }
                Err(e) => {
                    warn!("Rejected finalized block {}: {}", hash, e);
                }
            }
        }
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
