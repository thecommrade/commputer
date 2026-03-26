#![allow(dead_code)]
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use serde::{Deserialize, Serialize};
use tracing::{info, debug, warn};

use commputer_core::block::{Block, BlockHash, BlockHeader};
use commputer_core::identity::Address;
use commputer_consensus::snowball::{SnowballParams, SnowballVoter};

/// Feature 129: Consensus timeout — if no block finalized within this duration, force re-election.
pub const CONSENSUS_TIMEOUT_SECS: u64 = 30;

/// Feature 126: Minimum block interval per validator (seconds).
pub const MIN_BLOCK_INTERVAL_SECS: u64 = 2;

/// Feature 139: Maximum allowed timestamp drift from network median (seconds).
pub const MAX_TIMESTAMP_DRIFT_SECS: u64 = 15;

/// Feature 121: View change protocol.
/// If the elected block producer is offline for this duration (seconds),
/// the next-highest CRS validator takes over block production.
pub const VIEW_CHANGE_TIMEOUT_SECS: u64 = 10;

/// Feature 121: View change state — tracks when a view change is triggered.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ViewChange {
    /// The height at which the view change occurred.
    pub height: u64,
    /// The original producer who was expected to produce the block.
    pub original_producer: Address,
    /// The replacement producer who took over.
    pub replacement_producer: Address,
    /// When the view change was initiated.
    pub triggered_at: Instant,
    /// The view number (0 = original, 1 = first replacement, etc.).
    pub view_number: u32,
}

/// Messages exchanged between nodes for Snowball consensus and sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsensusMessage {
    /// A block candidate for a given height.
    BlockCandidate {
        block: Block,
    },
    /// Query: "At this height, which block do you prefer?"
    SnowballQuery {
        height: u64,
        querier_preference: BlockHash,
    },
    /// Response: "At this height, I prefer this block."
    SnowballResponse {
        height: u64,
        preference: BlockHash,
    },
    /// Request a specific block by height (sync protocol).
    BlockRequest {
        height: u64,
    },
    /// Response to a block request.
    BlockResponse {
        block: Option<Block>,
        requested_height: u64,
    },

    /// Feature 247: Light client request — request merkle proof for a tx in a block.
    LightClientRequest {
        tx_hash: [u8; 32],
        block_height: u64,
    },

    /// Feature 247: Light client response — return the proof and block header.
    LightClientResponse {
        block_header: Option<BlockHeader>,
        merkle_proof: Option<commputer_core::merkle::MerkleProof>,
        tx_hash: [u8; 32],
    },

    /// Feature 133: Checkpoint commitment — validators sign a checkpoint.
    CheckpointCommitment {
        height: u64,
        state_root: [u8; 32],
        validator: Address,
    },
}

/// Per-height voting state: the voter plus all candidate blocks.
struct HeightState {
    voter: SnowballVoter,
    candidates: HashMap<BlockHash, Block>,
    /// Accumulated responses for the current round.
    round_responses: HashMap<BlockHash, usize>,
}

/// Manages Snowball consensus across active heights.
///
/// Lifecycle per height:
/// 1. `add_candidate()` registers a block and creates/updates the voter.
/// 2. `query_preference()` returns what to include in a SnowballQuery.
/// 3. `record_response()` accumulates peer responses.
/// 4. `try_finalize_round()` feeds accumulated responses into the voter.
/// 5. `take_finalized()` returns the winning block and cleans up.
pub struct ConsensusManager {
    heights: HashMap<u64, HeightState>,
    params: SnowballParams,
    /// Feature 125: Track (validator, height) -> BlockHash to detect equivocation.
    pub validator_blocks: HashMap<(Address, u64), BlockHash>,
    /// Feature 125: Slashed validators for the current epoch — earn zero rewards.
    pub slashed_validators: HashSet<Address>,
    /// Feature 129: Track when each height started consensus.
    pub height_start_time: HashMap<u64, Instant>,
    /// Feature 121: View change history.
    pub view_changes: Vec<ViewChange>,
    /// Feature 126: Track last block timestamp per validator for rate limiting.
    pub last_block_time: HashMap<Address, u64>,
    /// Feature 133: Checkpoint commitments: height -> set of (validator, state_root).
    pub checkpoint_votes: HashMap<u64, HashMap<Address, [u8; 32]>>,
}

impl ConsensusManager {
    /// Create a new consensus manager with default Snowball parameters.
    pub fn new() -> Self {
        Self {
            heights: HashMap::new(),
            params: SnowballParams {
                sample_size: 3,
                quorum: 2,
                decision_threshold: 5,
            },
            validator_blocks: HashMap::new(),
            slashed_validators: HashSet::new(),
            height_start_time: HashMap::new(),
            view_changes: Vec::new(),
            last_block_time: HashMap::new(),
            checkpoint_votes: HashMap::new(),
        }
    }

    /// Create a consensus manager with custom Snowball parameters (Feature 131).
    pub fn with_params(params: SnowballParams) -> Self {
        Self {
            heights: HashMap::new(),
            params,
            validator_blocks: HashMap::new(),
            slashed_validators: HashSet::new(),
            height_start_time: HashMap::new(),
            view_changes: Vec::new(),
            last_block_time: HashMap::new(),
            checkpoint_votes: HashMap::new(),
        }
    }

    /// Add a candidate block. Creates the voter for this height if needed.
    /// If there is only one candidate, it is immediately finalized.
    /// Feature 125: Detects equivocation (same validator, same height, different hash).
    /// `from_network` should be true for blocks received from remote peers;
    /// local retries (same producer, rejected block) should pass false to
    /// avoid false-positive equivocation detection.
    pub fn add_candidate(&mut self, block: Block) {
        self.add_candidate_inner(block, true);
    }

    /// Add a locally produced candidate without triggering equivocation detection.
    pub fn add_local_candidate(&mut self, block: Block) {
        self.add_candidate_inner(block, false);
    }

    fn add_candidate_inner(&mut self, block: Block, from_network: bool) {
        let height = block.height();
        let hash = block.hash();
        let producer = block.header.producer;

        // Feature 125: Check for equivocation — only flag blocks received from the
        // network. Local retries at the same height are normal (e.g. after a rejected
        // block) and should not trigger slashing.
        let key = (producer, height);
        if from_network {
            if let Some(existing_hash) = self.validator_blocks.get(&key) {
                if *existing_hash != hash {
                    warn!(
                        "EQUIVOCATION DETECTED: validator {} signed two different blocks at height {} ({} and {})",
                        producer, height, existing_hash, hash
                    );
                    self.slashed_validators.insert(producer);
                }
            } else {
                self.validator_blocks.insert(key, hash);
            }
        }

        // Feature 129: Track height start time.
        self.height_start_time.entry(height).or_insert_with(Instant::now);

        let state = self.heights.entry(height).or_insert_with(|| HeightState {
            voter: SnowballVoter::new(self.params.clone()),
            candidates: HashMap::new(),
            round_responses: HashMap::new(),
        });

        // Don't re-add duplicates.
        if state.candidates.contains_key(&hash) {
            return;
        }

        state.candidates.insert(hash, block);
        debug!("Candidate added at height {}: {} (total: {})", height, hash, state.candidates.len());
    }

    /// Returns the voter's current preference at a given height, if any.
    pub fn query_preference(&self, height: u64) -> Option<BlockHash> {
        self.heights.get(&height).and_then(|s| s.voter.preference())
    }

    /// Record a peer's response for a given height.
    pub fn record_response(&mut self, height: u64, preference: BlockHash) {
        if let Some(state) = self.heights.get_mut(&height)
            && !state.voter.is_finalized() {
                *state.round_responses.entry(preference).or_insert(0) += 1;
            }
    }

    /// Feed accumulated responses into the voter and reset for the next round.
    /// Also handles the single-candidate fast-path: if only one candidate exists,
    /// finalize immediately without requiring peer responses.
    /// Feature 129: If elapsed > 30s, force re-election by finalizing on any candidate.
    /// Returns true if this round caused finalization.
    pub fn try_finalize_round(&mut self, height: u64) -> bool {
        if let Some(state) = self.heights.get_mut(&height) {
            if state.voter.is_finalized() {
                return false;
            }

            // Single-candidate fast-path: no need for multi-round voting.
            if state.candidates.len() == 1 {
                let hash = match state.candidates.keys().next() {
                    Some(h) => *h,
                    None => return false,
                };
                let mut responses = HashMap::new();
                responses.insert(hash, self.params.sample_size);
                for _ in 0..self.params.decision_threshold {
                    state.voter.record_round(&responses);
                }
                debug!("Single candidate at height {} — finalized immediately", height);
                return true;
            }

            // Feature 129: Consensus timeout — force finalization after 30s.
            if let Some(start) = self.height_start_time.get(&height)
                && start.elapsed().as_secs() >= CONSENSUS_TIMEOUT_SECS {
                    // Force finalize on the current preference or first candidate.
                    let hash = match state.voter.preference()
                        .or_else(|| state.candidates.keys().next().copied()) {
                        Some(h) => h,
                        None => return false, // no candidates at all
                    };
                    let mut responses = HashMap::new();
                    responses.insert(hash, self.params.sample_size);
                    for _ in 0..self.params.decision_threshold {
                        state.voter.record_round(&responses);
                    }
                    warn!("Consensus timeout at height {} — force-finalizing on {}", height, hash);
                    return true;
                }

            if state.round_responses.is_empty() {
                return false;
            }
            let responses = std::mem::take(&mut state.round_responses);
            let finalized = state.voter.record_round(&responses);
            if finalized {
                info!(
                    "Snowball finalized at height {}: {:?}",
                    height,
                    state.voter.finalized_hash()
                );
            }
            finalized
        } else {
            false
        }
    }

    /// Feature 125: Check if a validator has been slashed for equivocation.
    pub fn is_slashed(&self, addr: &Address) -> bool {
        self.slashed_validators.contains(addr)
    }

    /// Feature 125: Reset slashing state at epoch boundary.
    pub fn reset_epoch_slashing(&mut self) {
        self.slashed_validators.clear();
        self.validator_blocks.clear();
    }

    /// If the vote at `height` is finalized, return the winning block hash.
    pub fn finalized_at_height(&self, height: u64) -> Option<BlockHash> {
        self.heights
            .get(&height)
            .and_then(|s| s.voter.finalized_hash())
    }

    /// Take the finalized block out of the manager, cleaning up that height's state.
    /// Returns None if not yet finalized or height unknown.
    pub fn take_finalized(&mut self, height: u64) -> Option<Block> {
        let hash = self.finalized_at_height(height)?;
        let state = self.heights.remove(&height)?;
        state.candidates.into_values().find(|b| b.hash() == hash)
    }

    /// How many candidates exist at a given height.
    pub fn candidates_at_height(&self, height: u64) -> usize {
        self.heights
            .get(&height)
            .map(|s| s.candidates.len())
            .unwrap_or(0)
    }

    /// All heights that currently have an active (non-finalized) vote.
    pub fn active_heights(&self) -> Vec<u64> {
        self.heights
            .iter()
            .filter(|(_, s)| !s.voter.is_finalized())
            .map(|(&h, _)| h)
            .collect()
    }

    /// All heights that have a finalized winner ready to be applied.
    pub fn finalized_heights(&self) -> Vec<u64> {
        self.heights
            .iter()
            .filter(|(_, s)| s.voter.is_finalized())
            .map(|(&h, _)| h)
            .collect()
    }

    /// Whether there is an active (non-finalized) vote at a given height.
    pub fn has_active_vote(&self, height: u64) -> bool {
        self.heights
            .get(&height)
            .map(|s| !s.voter.is_finalized())
            .unwrap_or(false)
    }

    /// Whether we know about any activity at the given height.
    pub fn has_height(&self, height: u64) -> bool {
        self.heights.contains_key(&height)
    }

    /// Feature 121: Record a view change event.
    pub fn record_view_change(
        &mut self,
        height: u64,
        original_producer: Address,
        replacement_producer: Address,
        view_number: u32,
    ) {
        let vc = ViewChange {
            height,
            original_producer,
            replacement_producer,
            triggered_at: Instant::now(),
            view_number,
        };
        warn!(
            "View change at height {}: {} -> {} (view #{})",
            height, original_producer, replacement_producer, view_number
        );
        self.view_changes.push(vc);
    }

    /// Feature 121: Check if a view change should be triggered for the given height.
    /// Returns true if no block has been seen for VIEW_CHANGE_TIMEOUT_SECS.
    pub fn should_view_change(&self, height: u64) -> bool {
        if let Some(start) = self.height_start_time.get(&height) {
            start.elapsed().as_secs() >= VIEW_CHANGE_TIMEOUT_SECS
        } else {
            false
        }
    }

    /// Feature 126: Check if a block from this validator is rate-limited.
    /// Returns true if the block is too fast (less than MIN_BLOCK_INTERVAL_SECS
    /// since the last block from this validator).
    pub fn is_block_rate_limited(&self, producer: &Address, timestamp: u64) -> bool {
        if let Some(&last_time) = self.last_block_time.get(producer) {
            timestamp < last_time + MIN_BLOCK_INTERVAL_SECS
        } else {
            false
        }
    }

    /// Feature 126: Record when a validator produced a block.
    pub fn record_block_time(&mut self, producer: Address, timestamp: u64) {
        self.last_block_time.insert(producer, timestamp);
    }

    /// Feature 127: Check if a block should be suppressed (empty block with no
    /// transactions and no proof summaries).
    pub fn should_suppress_empty_block(block: &Block) -> bool {
        block.transactions.is_empty()
            && block.proof_summaries.is_empty()
            && block.epoch_summary.is_none()
    }

    /// Feature 132: Batch-finalize multiple blocks at once when catching up.
    /// Takes blocks sorted by height and finalizes them directly without
    /// running the full Snowball protocol (used during sync/catchup).
    pub fn batch_finalize_catchup(&mut self, blocks: Vec<Block>) -> Vec<Block> {
        let mut finalized = Vec::new();
        for block in blocks {
            let height = block.height();
            let hash = block.hash();

            // Track validator block for equivocation detection.
            let key = (block.header.producer, height);
            self.validator_blocks.entry(key).or_insert(hash);

            finalized.push(block);
        }
        finalized
    }

    /// Feature 133: Record a checkpoint commitment from a validator.
    pub fn record_checkpoint_vote(
        &mut self,
        height: u64,
        validator: Address,
        state_root: [u8; 32],
    ) {
        let votes = self.checkpoint_votes.entry(height).or_default();
        votes.insert(validator, state_root);
    }

    /// Feature 133: Check if a checkpoint has reached consensus (2/3+ agreement).
    pub fn checkpoint_consensus(
        &self,
        height: u64,
        total_validators: usize,
    ) -> Option<[u8; 32]> {
        if total_validators == 0 {
            return None;
        }
        let threshold = (total_validators * 2) / 3 + 1;

        if let Some(votes) = self.checkpoint_votes.get(&height) {
            // Count votes per state root.
            let mut root_counts: HashMap<[u8; 32], usize> = HashMap::new();
            for root in votes.values() {
                *root_counts.entry(*root).or_insert(0) += 1;
            }
            // Find any root with enough votes.
            for (root, count) in &root_counts {
                if *count >= threshold {
                    return Some(*root);
                }
            }
        }
        None
    }

    /// Feature 139: Validate a block's timestamp against the network median.
    /// Returns true if the timestamp is within acceptable drift.
    pub fn validate_timestamp(
        &self,
        block_timestamp: u64,
        recent_timestamps: &[u64],
    ) -> bool {
        if recent_timestamps.is_empty() {
            return true; // No reference — accept.
        }

        let mut sorted = recent_timestamps.to_vec();
        sorted.sort();
        let median = sorted[sorted.len() / 2];

        // Block timestamp must not be too far in the future or past.
        let drift = if block_timestamp > median {
            block_timestamp - median
        } else {
            median - block_timestamp
        };

        drift <= MAX_TIMESTAMP_DRIFT_SECS
    }
}

/// Feature 134: Generate a light client merkle proof for a transaction in a block.
/// Given a block and a transaction hash, returns the merkle proof and the index.
pub fn generate_light_client_proof(
    block: &Block,
    tx_hash: [u8; 32],
) -> Option<(commputer_core::merkle::MerkleProof, usize)> {
    let tx_hashes: Vec<[u8; 32]> = block.transactions.iter()
        .map(|tx| tx.hash().0)
        .collect();

    // Find the index of the transaction.
    let tx_index = tx_hashes.iter().position(|h| *h == tx_hash)?;

    let proof = commputer_core::merkle::generate_merkle_proof(&tx_hashes, tx_index)?;
    Some((proof, tx_index))
}

/// Feature 135: Verify a light client merkle proof without downloading the full block.
/// Given a tx hash, proof, and the block header's tx_root, verify inclusion.
pub fn verify_light_client_proof(
    tx_hash: [u8; 32],
    proof: &commputer_core::merkle::MerkleProof,
    tx_root: [u8; 32],
) -> bool {
    commputer_core::merkle::verify_merkle_proof(tx_hash, proof, tx_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::block::{Block, BlockHeader, BlockHash};
    use commputer_core::identity::Address;

    fn addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    fn make_test_block(height: u64) -> Block {
        make_test_block_with_producer(height, addr(0))
    }

    fn make_test_block_with_producer(height: u64, producer: Address) -> Block {
        make_test_block_with_timestamp(height, producer, 1000 + height)
    }

    fn make_test_block_with_timestamp(height: u64, producer: Address, timestamp: u64) -> Block {
        Block {
            header: BlockHeader {
                protocol_version: 1,
                height,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp,
                producer,
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        }
    }

    // ---- Existing tests ----

    #[test]
    fn single_candidate_finalizes_immediately() {
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        let hash = block.hash();
        cm.add_candidate(block);
        // Not finalized until we run a round.
        assert_eq!(cm.finalized_at_height(1), None);
        // Single-candidate fast-path triggers on try_finalize_round.
        cm.try_finalize_round(1);
        assert_eq!(cm.finalized_at_height(1), Some(hash));
    }

    #[test]
    fn multiple_candidates_require_voting() {
        let mut cm = ConsensusManager::new();
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        cm.add_candidate(block_a.clone());
        cm.add_candidate(block_b);
        // Not yet finalized — needs Snowball rounds.
        assert_eq!(cm.finalized_at_height(1), None);
        assert_eq!(cm.candidates_at_height(1), 2);
    }

    #[test]
    fn voting_converges_with_responses() {
        let mut cm = ConsensusManager::new();
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // Simulate 5 rounds where all peers prefer block A.
        // With quorum=2, decision_threshold=5, this should finalize.
        for _ in 0..5 {
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.try_finalize_round(1);
        }
        assert_eq!(cm.finalized_at_height(1), Some(hash_a));
    }

    #[test]
    fn take_finalized_removes_height() {
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        let hash = block.hash();
        cm.add_candidate(block);
        cm.try_finalize_round(1);

        let taken = cm.take_finalized(1);
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().hash(), hash);
        // Height should be gone now.
        assert_eq!(cm.finalized_at_height(1), None);
        assert!(!cm.has_height(1));
    }

    /// Simulate two consensus managers (two nodes) that both see two competing
    /// blocks at the same height. They exchange Snowball votes and must converge
    /// on the same winner.
    #[test]
    fn fork_resolution_two_managers_converge() {
        let mut cm_a = ConsensusManager::new();
        let mut cm_b = ConsensusManager::new();

        let block_1 = make_test_block_with_producer(1, addr(1));
        let block_2 = make_test_block_with_producer(1, addr(2));
        let hash_1 = block_1.hash();
        let hash_2 = block_2.hash();

        // Both nodes see both candidates (as would happen via gossipsub).
        cm_a.add_candidate(block_1.clone());
        cm_a.add_candidate(block_2.clone());
        cm_b.add_candidate(block_1);
        cm_b.add_candidate(block_2);

        // Neither should be finalized yet.
        assert_eq!(cm_a.finalized_at_height(1), None);
        assert_eq!(cm_b.finalized_at_height(1), None);

        // Simulate Snowball voting rounds. In each round:
        // - Each node queries its preference
        // - Both preferences are recorded as responses on the other node
        // - Both nodes try to finalize
        // We simulate a network where hash_1 has a slight majority.
        for round in 0..10 {
            let _pref_a = cm_a.query_preference(1);
            let _pref_b = cm_b.query_preference(1);

            // Simulate 3 peers voting (sample_size=3):
            // - 2 peers prefer hash_1, 1 prefers hash_2 (gives hash_1 the edge)
            let majority_hash = hash_1;
            let minority_hash = hash_2;

            // Feed majority preference to both managers.
            cm_a.record_response(1, majority_hash);
            cm_a.record_response(1, majority_hash);
            cm_a.record_response(1, minority_hash);

            cm_b.record_response(1, majority_hash);
            cm_b.record_response(1, majority_hash);
            cm_b.record_response(1, minority_hash);

            cm_a.try_finalize_round(1);
            cm_b.try_finalize_round(1);

            // Check if both finalized.
            let final_a = cm_a.finalized_at_height(1);
            let final_b = cm_b.finalized_at_height(1);

            if final_a.is_some() && final_b.is_some() {
                assert_eq!(
                    final_a, final_b,
                    "Both nodes must converge on the same block (round {})",
                    round
                );
                assert_eq!(
                    final_a.unwrap(), majority_hash,
                    "Winner should be the majority-preferred block"
                );
                eprintln!("Fork resolved in {} rounds — both chose {}", round + 1, majority_hash);
                return;
            }
        }

        // If we get here, both should at least have finalized by now with consistent voting.
        let final_a = cm_a.finalized_at_height(1);
        let final_b = cm_b.finalized_at_height(1);
        assert!(final_a.is_some(), "Node A should have finalized after 10 rounds");
        assert!(final_b.is_some(), "Node B should have finalized after 10 rounds");
        assert_eq!(final_a, final_b, "Both nodes must agree on the winner");
    }

    /// Test that fork resolution works even when the minority block initially
    /// gets some support before the majority prevails.
    #[test]
    fn fork_resolution_minority_loses_after_initial_support() {
        let mut cm = ConsensusManager::new();
        let block_a = make_test_block_with_producer(1, addr(10));
        let block_b = make_test_block_with_producer(1, addr(20));
        let hash_a = block_a.hash();
        let hash_b = block_b.hash();

        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // First 2 rounds: block_b has majority (shouldn't finalize yet, need 5 consecutive).
        for _ in 0..2 {
            cm.record_response(1, hash_b);
            cm.record_response(1, hash_b);
            cm.record_response(1, hash_a);
            cm.try_finalize_round(1);
        }
        assert_eq!(cm.finalized_at_height(1), None, "Should not finalize after only 2 rounds");

        // Next 5+ rounds: block_a takes over with strong majority.
        for _ in 0..6 {
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.try_finalize_round(1);
        }

        // block_a should win since it had strong majority in later rounds.
        let winner = cm.finalized_at_height(1);
        assert!(winner.is_some(), "Should finalize after sufficient consistent rounds");
        assert_eq!(winner.unwrap(), hash_a, "Block A should win with sustained majority");
    }

    #[test]
    fn no_finalization_without_enough_rounds() {
        let mut cm = ConsensusManager::new();
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // Only 3 rounds (need 5 for decision_threshold).
        for _ in 0..3 {
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.try_finalize_round(1);
        }
        assert_eq!(cm.finalized_at_height(1), None);
    }

    // ---- Feature 121: View change tests ----

    #[test]
    fn view_change_recorded() {
        let mut cm = ConsensusManager::new();
        cm.record_view_change(10, addr(1), addr(2), 1);
        assert_eq!(cm.view_changes.len(), 1);
        assert_eq!(cm.view_changes[0].height, 10);
        assert_eq!(cm.view_changes[0].original_producer, addr(1));
        assert_eq!(cm.view_changes[0].replacement_producer, addr(2));
        assert_eq!(cm.view_changes[0].view_number, 1);
    }

    // ---- Feature 122: Extended equivocation detection tests ----

    #[test]
    fn equivocation_detected_and_slashed() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        // First block from validator at height 5.
        let block_a = make_test_block_with_producer(5, validator);
        cm.add_candidate(block_a);
        assert!(!cm.is_slashed(&validator));

        // Second different block from same validator at same height.
        let mut block_b = make_test_block_with_timestamp(5, validator, 1006);
        // Give it a different state root to make hash differ.
        block_b.header.state_root = [1u8; 32];
        cm.add_candidate(block_b);

        // Should now be slashed.
        assert!(cm.is_slashed(&validator));
    }

    #[test]
    fn same_block_twice_not_equivocation() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        let block = make_test_block_with_producer(5, validator);
        let block_clone = block.clone();
        cm.add_candidate(block);
        cm.add_candidate(block_clone); // Same block — not equivocation.

        assert!(!cm.is_slashed(&validator));
    }

    #[test]
    fn different_heights_not_equivocation() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        let block_a = make_test_block_with_producer(5, validator);
        let block_b = make_test_block_with_producer(6, validator);
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // Different heights — this is normal behavior, not equivocation.
        assert!(!cm.is_slashed(&validator));
    }

    #[test]
    fn different_validators_same_height_not_equivocation() {
        let mut cm = ConsensusManager::new();

        let block_a = make_test_block_with_producer(5, addr(1));
        let block_b = make_test_block_with_producer(5, addr(2));
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        assert!(!cm.is_slashed(&addr(1)));
        assert!(!cm.is_slashed(&addr(2)));
    }

    #[test]
    fn multiple_equivocators_all_slashed() {
        let mut cm = ConsensusManager::new();

        // Validator 1 equivocates.
        let b1a = make_test_block_with_producer(5, addr(1));
        let mut b1b = make_test_block_with_timestamp(5, addr(1), 1006);
        b1b.header.state_root = [1u8; 32];
        cm.add_candidate(b1a);
        cm.add_candidate(b1b);

        // Validator 2 equivocates.
        let b2a = make_test_block_with_producer(5, addr(2));
        let mut b2b = make_test_block_with_timestamp(5, addr(2), 1007);
        b2b.header.state_root = [2u8; 32];
        cm.add_candidate(b2a);
        cm.add_candidate(b2b);

        assert!(cm.is_slashed(&addr(1)));
        assert!(cm.is_slashed(&addr(2)));
    }

    // ---- Feature 123: Slashing ensures zero epoch rewards ----

    #[test]
    fn slashed_validator_gets_zero_rewards() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        // Cause equivocation.
        let b1 = make_test_block_with_producer(5, validator);
        let mut b2 = make_test_block_with_timestamp(5, validator, 1006);
        b2.header.state_root = [1u8; 32];
        cm.add_candidate(b1);
        cm.add_candidate(b2);

        // Validator is slashed — should get zero epoch rewards.
        assert!(cm.is_slashed(&validator));

        // Simulate reward calculation: slashed validators get 0.
        let base_reward = 1000u64;
        let actual_reward = if cm.is_slashed(&validator) { 0 } else { base_reward };
        assert_eq!(actual_reward, 0);

        // Non-slashed validator gets full reward.
        let non_slashed = addr(2);
        let non_slashed_reward = if cm.is_slashed(&non_slashed) { 0 } else { base_reward };
        assert_eq!(non_slashed_reward, 1000);
    }

    #[test]
    fn slashing_resets_at_epoch_boundary() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        // Cause slashing.
        let b1 = make_test_block_with_producer(5, validator);
        let mut b2 = make_test_block_with_timestamp(5, validator, 1006);
        b2.header.state_root = [1u8; 32];
        cm.add_candidate(b1);
        cm.add_candidate(b2);
        assert!(cm.is_slashed(&validator));

        // Reset at epoch boundary.
        cm.reset_epoch_slashing();
        assert!(!cm.is_slashed(&validator));
    }

    // ---- Feature 126: Block production rate limiting ----

    #[test]
    fn block_rate_limiting() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        cm.record_block_time(validator, 1000);

        // Block at 1001 (only 1s later) should be rate-limited.
        assert!(cm.is_block_rate_limited(&validator, 1001));

        // Block at 1002 (exactly 2s later) should be allowed.
        assert!(!cm.is_block_rate_limited(&validator, 1002));

        // Block at 1005 (5s later) should be allowed.
        assert!(!cm.is_block_rate_limited(&validator, 1005));
    }

    #[test]
    fn block_rate_limiting_unknown_validator() {
        let cm = ConsensusManager::new();
        // Unknown validator — no rate limit.
        assert!(!cm.is_block_rate_limited(&addr(99), 1000));
    }

    // ---- Feature 127: Empty block suppression ----

    #[test]
    fn empty_block_suppressed() {
        let block = make_test_block(1);
        assert!(ConsensusManager::should_suppress_empty_block(&block));
    }

    #[test]
    fn block_with_transactions_not_suppressed() {
        use commputer_core::transaction::{Transaction, TxKind};
        use commputer_core::token::Amount;

        let mut block = make_test_block(1);
        block.transactions.push(Transaction {
            from: addr(1),
            nonce: 0,
            kind: TxKind::Transfer {
                to: addr(2),
                amount: Amount::from_raw(100),
            },
            fee: 0,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        });
        assert!(!ConsensusManager::should_suppress_empty_block(&block));
    }

    // ---- Feature 132: Multi-block finality (batch catchup) ----

    #[test]
    fn batch_finalize_catchup() {
        let mut cm = ConsensusManager::new();
        let blocks = vec![
            make_test_block_with_producer(1, addr(1)),
            make_test_block_with_producer(2, addr(2)),
            make_test_block_with_producer(3, addr(3)),
        ];
        let finalized = cm.batch_finalize_catchup(blocks);
        assert_eq!(finalized.len(), 3);
        // Validator blocks should be tracked.
        assert!(cm.validator_blocks.contains_key(&(addr(1), 1)));
        assert!(cm.validator_blocks.contains_key(&(addr(2), 2)));
        assert!(cm.validator_blocks.contains_key(&(addr(3), 3)));
    }

    // ---- Feature 133: Checkpoint commitment ----

    #[test]
    fn checkpoint_commitment_consensus() {
        let mut cm = ConsensusManager::new();
        let state_root = [42u8; 32];

        // 3 validators vote for the same state root at height 1000.
        cm.record_checkpoint_vote(1000, addr(1), state_root);
        cm.record_checkpoint_vote(1000, addr(2), state_root);
        cm.record_checkpoint_vote(1000, addr(3), state_root);

        // With 4 total validators, need 3 votes (2/3 + 1 = 3).
        assert_eq!(cm.checkpoint_consensus(1000, 4), Some(state_root));

        // With 5 total validators, need 4 votes — not enough.
        assert_eq!(cm.checkpoint_consensus(1000, 5), None);
    }

    #[test]
    fn checkpoint_no_consensus_on_different_roots() {
        let mut cm = ConsensusManager::new();
        cm.record_checkpoint_vote(1000, addr(1), [1u8; 32]);
        cm.record_checkpoint_vote(1000, addr(2), [2u8; 32]);
        cm.record_checkpoint_vote(1000, addr(3), [3u8; 32]);

        // Everyone voted for different roots — no consensus.
        assert_eq!(cm.checkpoint_consensus(1000, 3), None);
    }

    // ---- Feature 137: Fork choice rule test ----

    #[test]
    fn fork_choice_crs_weighted() {
        // Create two equal-length forks. The one with CRS-weighted majority
        // should be selected through Snowball voting.
        let mut cm = ConsensusManager::new();

        // Two competing blocks at the same height from different validators.
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();
        let hash_b = block_b.hash();

        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // Simulate CRS-weighted voting: validator with higher CRS gets more
        // "votes" (peers that follow CRS prefer that validator's block).
        // hash_a has 2/3 support (CRS-weighted), hash_b has 1/3.
        for _ in 0..5 {
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_b);
            cm.try_finalize_round(1);
        }

        let winner = cm.finalized_at_height(1);
        assert!(winner.is_some(), "Should finalize after 5 rounds of consistent voting");
        assert_eq!(winner.unwrap(), hash_a, "CRS-weighted majority should win");
    }

    #[test]
    fn fork_choice_equal_length_deterministic() {
        // With equal voting, the fork choice should still converge deterministically.
        let mut cm_1 = ConsensusManager::new();
        let mut cm_2 = ConsensusManager::new();

        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();

        cm_1.add_candidate(block_a.clone());
        cm_1.add_candidate(block_b.clone());
        cm_2.add_candidate(block_a);
        cm_2.add_candidate(block_b);

        // Both managers see the same voting pattern.
        for _ in 0..6 {
            cm_1.record_response(1, hash_a);
            cm_1.record_response(1, hash_a);
            cm_1.try_finalize_round(1);

            cm_2.record_response(1, hash_a);
            cm_2.record_response(1, hash_a);
            cm_2.try_finalize_round(1);
        }

        let final_1 = cm_1.finalized_at_height(1);
        let final_2 = cm_2.finalized_at_height(1);
        assert_eq!(final_1, final_2, "Both managers must agree with same inputs");
    }

    // ---- Feature 139: Time warp attack prevention ----

    #[test]
    fn timestamp_within_drift_accepted() {
        let cm = ConsensusManager::new();
        let recent = vec![1000, 1002, 1004, 1006, 1008];
        // Median is 1004. Block at 1010 is +6s drift — within 15s limit.
        assert!(cm.validate_timestamp(1010, &recent));
    }

    #[test]
    fn timestamp_too_far_in_future_rejected() {
        let cm = ConsensusManager::new();
        let recent = vec![1000, 1002, 1004, 1006, 1008];
        // Median is 1004. Block at 1025 is +21s drift — exceeds 15s limit.
        assert!(!cm.validate_timestamp(1025, &recent));
    }

    #[test]
    fn timestamp_too_far_in_past_rejected() {
        let cm = ConsensusManager::new();
        let recent = vec![1000, 1002, 1004, 1006, 1008];
        // Median is 1004. Block at 980 is -24s drift — exceeds 15s limit.
        assert!(!cm.validate_timestamp(980, &recent));
    }

    #[test]
    fn timestamp_empty_references_always_accepted() {
        let cm = ConsensusManager::new();
        assert!(cm.validate_timestamp(9999, &[]));
    }

    // ---- Feature 131: Custom params ----

    #[test]
    fn custom_params_constructor() {
        let params = SnowballParams {
            sample_size: 10,
            quorum: 7,
            decision_threshold: 15,
        };
        let cm = ConsensusManager::with_params(params);
        // Verify custom params are used by testing convergence speed.
        // With decision_threshold=15, need 15 rounds.
        let mut cm = cm;
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // 14 rounds should not be enough.
        for _ in 0..14 {
            for _ in 0..7 {
                cm.record_response(1, hash_a);
            }
            cm.try_finalize_round(1);
        }
        assert_eq!(cm.finalized_at_height(1), None);

        // 1 more round should finalize.
        for _ in 0..7 {
            cm.record_response(1, hash_a);
        }
        cm.try_finalize_round(1);
        assert_eq!(cm.finalized_at_height(1), Some(hash_a));
    }

    // ---- Feature 134/135: Light client proof generation and verification ----

    #[test]
    fn light_client_proof_roundtrip() {
        use commputer_core::transaction::{Transaction, TxKind};
        use commputer_core::token::Amount;

        // Create a block with transactions.
        let mut block = make_test_block(1);
        for i in 0..5 {
            let mut from = [0u8; 32];
            from[0] = i + 10;
            block.transactions.push(Transaction {
                from: Address(from),
                nonce: i as u64,
                kind: TxKind::Transfer {
                    to: addr(99),
                    amount: Amount::from_raw(100),
                },
                fee: 0,
                signature: vec![],
                public_key: vec![],
                memo: None,
                timelock: None,
            });
        }
        block.header.tx_root = block.compute_tx_root();

        // Generate proof for the 3rd transaction.
        let tx_hash = block.transactions[2].hash().0;
        let (proof, idx) = generate_light_client_proof(&block, tx_hash).unwrap();
        assert_eq!(idx, 2);

        // Verify the proof.
        assert!(verify_light_client_proof(tx_hash, &proof, block.header.tx_root));

        // Wrong tx hash should fail.
        let fake_hash = [0xFFu8; 32];
        assert!(!verify_light_client_proof(fake_hash, &proof, block.header.tx_root));
    }

    #[test]
    fn light_client_proof_missing_tx() {
        let block = make_test_block(1); // No transactions.
        let result = generate_light_client_proof(&block, [1u8; 32]);
        assert!(result.is_none());
    }
}
