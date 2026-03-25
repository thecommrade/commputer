use std::time::Duration;
use tokio::time;
use tracing::{info, warn, debug};
use futures::StreamExt;

use commputer_core::block::{Block, BlockHeader, BlockHash};
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

pub struct EventLoop {
    pub state: ChainState,
    pub wallet: Wallet,
    pub network: CommpNetwork,
    pub emission: EmissionSchedule,
    pub epoch_state: EpochState,
    pub validator: ValidatorState,
    pub compliance: ComplianceChecker,
    pub pending_txs: Vec<Transaction>,
}

impl EventLoop {
    pub fn new(
        state: ChainState,
        wallet: Wallet,
        network: CommpNetwork,
    ) -> Self {
        let epoch_state = EpochState::new(0, 0);
        Self {
            state,
            wallet,
            network,
            emission: EmissionSchedule::new(),
            epoch_state,
            validator: ValidatorState::new(),
            compliance: ComplianceChecker::new(),
            pending_txs: Vec::new(),
        }
    }

    pub async fn run(&mut self) {
        let mut epoch_interval = time::interval(Duration::from_secs(3600));
        let mut block_interval = time::interval(Duration::from_secs(2));

        info!("Event loop started. Listening for peers...");

        loop {
            tokio::select! {
                event = self.network.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }
                _ = epoch_interval.tick() => {
                    self.handle_epoch_tick();
                }
                _ = block_interval.tick() => {
                    self.handle_block_tick();
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
                gossipsub::Event::Message { message, .. }
            )) => {
                let topic = message.topic.as_str();
                debug!("Gossipsub message on topic: {}", topic);

                if topic == topics::TOPIC_BLOCKS {
                    if let Ok(block) = serde_json::from_slice::<Block>(&message.data) {
                        self.handle_new_block(block);
                    }
                } else if topic == topics::TOPIC_TRANSACTIONS {
                    if let Ok(tx) = serde_json::from_slice::<Transaction>(&message.data) {
                        self.handle_new_transaction(tx);
                    }
                }
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(
                    "Listening on {}/p2p/{}",
                    address, self.network.local_peer_id
                );
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                info!("Connected to peer: {}", peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                info!("Disconnected from peer: {}", peer_id);
            }
            _ => {}
        }
    }

    fn handle_new_block(&mut self, block: Block) {
        let hash = block.hash();
        let height = block.height();

        if self.state.blocks.contains(&hash) {
            return; // Already have this block.
        }

        match self.state.apply_block(&block) {
            Ok(()) => {
                info!("Applied block {} at height {}", hash, height);
                self.print_status();
            }
            Err(e) => {
                warn!("Rejected block {}: {}", hash, e);
            }
        }
    }

    fn handle_new_transaction(&mut self, tx: Transaction) {
        let hash = tx.hash();
        debug!("Received transaction: {:?}", hash);
        self.pending_txs.push(tx);
    }

    fn handle_epoch_tick(&mut self) {
        let epoch = self.epoch_state.epoch;
        let validator_count = self.epoch_state.validator_count() as u64;

        if validator_count == 0 {
            debug!("Epoch {} tick — no validators", epoch);
            self.epoch_state = EpochState::new(epoch + 1, 0);
            return;
        }

        // Calculate emission for this epoch.
        let epoch_emission = self.emission.per_epoch_emission(validator_count);

        // Don't emit more than remaining supply.
        let remaining = self.state.remaining_supply();
        let actual_emission = epoch_emission.min(remaining);

        if actual_emission > 0 {
            // Demand-weighted allocation across channels.
            let _allocation =
                ChannelAllocation::from_demand(actual_emission, &self.epoch_state.demand);

            info!(
                "Epoch {} complete: {} validators, emitting {} COMME across channels",
                epoch,
                validator_count,
                actual_emission / UNITS_PER_COMME,
            );

            // Record emission.
            self.state.emit(actual_emission);
        }

        // Start new epoch.
        self.state.current_epoch = epoch + 1;
        self.epoch_state = EpochState::new(epoch + 1, 0);
    }

    fn handle_block_tick(&mut self) {
        // Only produce blocks if we're a registered validator.
        if self.validator.status() != ValidatorStatus::Active {
            return;
        }

        let current_height = self.state.blocks.height();
        let parent = self
            .state
            .blocks
            .latest()
            .map(|b| b.hash())
            .unwrap_or(BlockHash::GENESIS);

        // Create a new block with pending transactions.
        let block = Block {
            header: BlockHeader {
                height: current_height + 1,
                parent_hash: parent,
                tx_root: [0u8; 32],  // Simplified for now.
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                producer: *self.wallet.address(),
                epoch: self.state.current_epoch,
                signature: vec![],
            },
            transactions: std::mem::take(&mut self.pending_txs),
            proof_summaries: vec![],
        };

        // Apply and broadcast.
        match self.state.apply_block(&block) {
            Ok(()) => {
                info!("Produced block at height {}", block.height());

                // Broadcast via gossipsub.
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
            }
            Err(e) => {
                warn!("Failed to produce block: {}", e);
                // Return transactions to pending.
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
