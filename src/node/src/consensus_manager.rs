use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use tracing::{info, debug};

use commputer_core::block::{Block, BlockHash};
use commputer_consensus::snowball::{SnowballParams, SnowballVoter};

/// Messages exchanged between nodes for Snowball consensus.
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
}

impl ConsensusManager {
    pub fn new() -> Self {
        Self {
            heights: HashMap::new(),
            params: SnowballParams {
                sample_size: 3,
                quorum: 2,
                decision_threshold: 5,
            },
        }
    }

    /// Add a candidate block. Creates the voter for this height if needed.
    /// If there is only one candidate, it is immediately finalized.
    pub fn add_candidate(&mut self, block: Block) {
        let height = block.height();
        let hash = block.hash();

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
    /// Returns true if this round caused finalization.
    pub fn try_finalize_round(&mut self, height: u64) -> bool {
        if let Some(state) = self.heights.get_mut(&height) {
            if state.voter.is_finalized() {
                return false;
            }

            // Single-candidate fast-path: no need for multi-round voting.
            if state.candidates.len() == 1 {
                let hash = *state.candidates.keys().next().unwrap();
                let mut responses = HashMap::new();
                responses.insert(hash, self.params.sample_size);
                for _ in 0..self.params.decision_threshold {
                    state.voter.record_round(&responses);
                }
                debug!("Single candidate at height {} — finalized immediately", height);
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
                height,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1000 + height,
                producer,
                epoch: 0,
                signature: vec![],
            },
            transactions: vec![],
            proof_summaries: vec![],
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
