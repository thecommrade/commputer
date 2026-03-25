use std::collections::{HashMap, HashSet};
use std::time::Instant;
use serde::{Deserialize, Serialize};
use tracing::{info, debug, warn};

use commputer_core::block::{Block, BlockHash, BlockHeader};
use commputer_core::identity::Address;
use commputer_consensus::snowball::{SnowballParams, SnowballVoter};

/// Feature 129: Consensus timeout — if no block finalized within this duration, force re-election.
pub const CONSENSUS_TIMEOUT_SECS: u64 = 30;

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
        }
    }

    /// Add a candidate block. Creates the voter for this height if needed.
    /// If there is only one candidate, it is immediately finalized.
    /// Feature 125: Detects equivocation (same validator, same height, different hash).
    pub fn add_candidate(&mut self, block: Block) {
        let height = block.height();
        let hash = block.hash();
        let producer = block.header.producer;

        // Feature 125: Check for equivocation.
        let key = (producer, height);
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
        if let Some(state) = self.heights.get_mut(&height) {
            if !state.voter.is_finalized() {
                *state.round_responses.entry(preference).or_insert(0) += 1;
            }
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
            if let Some(start) = self.height_start_time.get(&height) {
                if start.elapsed().as_secs() >= CONSENSUS_TIMEOUT_SECS {
                    // Force finalize on the current preference or first candidate.
                    let hash = state.voter.preference()
                        .or_else(|| state.candidates.keys().next().copied())
                        .unwrap_or_default();
                    let mut responses = HashMap::new();
                    responses.insert(hash, self.params.sample_size);
                    for _ in 0..self.params.decision_threshold {
                        state.voter.record_round(&responses);
                    }
                    warn!("Consensus timeout at height {} — force-finalizing on {}", height, hash);
                    return true;
                }
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
        Block {
            header: BlockHeader {
                protocol_version: 1,
                height,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1000 + height,
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
            let pref_a = cm_a.query_preference(1);
            let pref_b = cm_b.query_preference(1);

            // Simulate 3 peers voting (sample_size=3):
            // - 2 peers prefer hash_1, 1 prefers hash_2 (gives hash_1 the edge)
            // For the initial rounds before any preference is set, seed the votes.
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
}
