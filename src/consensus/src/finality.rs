//! Feature 124: Finality gadget — blocks with 2/3+ validator weight are finalized.
//! Feature 132: Multi-block finality — batch-finalize blocks when catching up.
//! Feature 138: Long-range attack prevention — reject blocks deeper than finality depth.

use std::collections::{HashMap, HashSet};
use commputer_core::block::BlockHash;
use commputer_core::identity::Address;

/// Default finality depth: blocks finalized deeper than this cannot be reverted.
pub const DEFAULT_FINALITY_DEPTH: u64 = 100;

/// A vote from a validator confirming a particular block hash at a height.
#[derive(Debug, Clone)]
pub struct FinalityVote {
    pub validator: Address,
    pub block_hash: BlockHash,
    pub height: u64,
    /// Weight of this validator (e.g., composite resource score).
    pub weight: u64,
}

/// Tracks finality state: which blocks have been confirmed by 2/3+ of
/// validator weight, and prevents reorgs past the finality boundary.
#[derive(Debug)]
pub struct FinalityGadget {
    /// The highest finalized block height.
    pub finalized_height: u64,
    /// The hash of the highest finalized block.
    pub finalized_hash: Option<BlockHash>,
    /// Votes per height: height -> (block_hash -> set of (validator, weight)).
    votes: HashMap<u64, HashMap<BlockHash, Vec<(Address, u64)>>>,
    /// Total validator weight in the current epoch.
    pub total_weight: u64,
    /// Finality depth — blocks older than (tip - depth) are considered final.
    pub finality_depth: u64,
    /// All finalized block hashes (for quick lookup).
    finalized_blocks: HashSet<BlockHash>,
}

impl FinalityGadget {
    /// Create a new finality gadget.
    pub fn new() -> Self {
        Self {
            finalized_height: 0,
            finalized_hash: None,
            votes: HashMap::new(),
            total_weight: 0,
            finality_depth: DEFAULT_FINALITY_DEPTH,
            finalized_blocks: HashSet::new(),
        }
    }

    /// Set the total validator weight for the current epoch.
    pub fn set_total_weight(&mut self, weight: u64) {
        self.total_weight = weight;
    }

    /// Record a finality vote from a validator.
    /// Returns true if this vote caused the block to become finalized.
    pub fn record_vote(&mut self, vote: FinalityVote) -> bool {
        let height = vote.height;
        let hash = vote.block_hash;

        // Don't accept votes for already-finalized heights.
        if height <= self.finalized_height {
            return false;
        }

        let height_votes = self.votes.entry(height).or_default();
        let hash_votes = height_votes.entry(hash).or_default();

        // Don't double-count votes from the same validator.
        if hash_votes.iter().any(|(v, _)| *v == vote.validator) {
            return false;
        }

        hash_votes.push((vote.validator, vote.weight));

        // Check if this block has 2/3+ weight.
        self.check_finality(height, hash)
    }

    /// Check if a block at height with given hash has reached 2/3+ finality.
    fn check_finality(&mut self, height: u64, hash: BlockHash) -> bool {
        if self.total_weight == 0 {
            return false;
        }

        let threshold = (self.total_weight * 2) / 3 + 1;

        if let Some(height_votes) = self.votes.get(&height) {
            if let Some(hash_votes) = height_votes.get(&hash) {
                let vote_weight: u64 = hash_votes.iter().map(|(_, w)| w).sum();
                if vote_weight >= threshold {
                    self.finalize(height, hash);
                    return true;
                }
            }
        }
        false
    }

    /// Mark a block as finalized and clean up older vote state.
    fn finalize(&mut self, height: u64, hash: BlockHash) {
        self.finalized_height = height;
        self.finalized_hash = Some(hash);
        self.finalized_blocks.insert(hash);

        // Clean up votes for heights at or below the new finality.
        self.votes.retain(|&h, _| h > height);
    }

    /// Feature 132: Batch-finalize multiple blocks at once (used when catching up).
    /// Takes a list of (height, hash) pairs sorted by height and finalizes them all.
    pub fn batch_finalize(&mut self, blocks: &[(u64, BlockHash)]) -> usize {
        let mut count = 0;
        for &(height, hash) in blocks {
            if height > self.finalized_height {
                self.finalize(height, hash);
                count += 1;
            }
        }
        count
    }

    /// Feature 138: Check if a block is attempting a long-range attack.
    /// Returns true if the block's height is below the finality boundary and
    /// the block hash is not already known as finalized.
    pub fn is_long_range_attack(&self, block_height: u64, block_hash: &BlockHash, tip_height: u64) -> bool {
        // If the block targets a height that is deeper than our finality depth
        // from the current tip, and it's not a known finalized block, reject it.
        if tip_height > self.finality_depth
            && block_height < tip_height - self.finality_depth
            && !self.finalized_blocks.contains(block_hash)
        {
            return true;
        }
        false
    }

    /// Check if a block hash has been finalized.
    pub fn is_finalized(&self, hash: &BlockHash) -> bool {
        self.finalized_blocks.contains(hash)
    }

    /// Check if a reorg would violate finality.
    /// Returns true if the proposed fork point is at or below the finalized height.
    pub fn would_violate_finality(&self, fork_point_height: u64) -> bool {
        fork_point_height <= self.finalized_height
    }
}

impl Default for FinalityGadget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    fn hash(n: u8) -> BlockHash {
        let mut h = [0u8; 32];
        h[0] = n;
        BlockHash(h)
    }

    #[test]
    fn finality_requires_two_thirds() {
        let mut fg = FinalityGadget::new();
        fg.set_total_weight(300);

        let block_hash = hash(1);
        let height = 10;

        // 200 weight = 2/3 of 300 = 200, but threshold is 201 (2/3 + 1).
        assert!(!fg.record_vote(FinalityVote {
            validator: addr(1),
            block_hash,
            height,
            weight: 100,
        }));
        assert!(!fg.record_vote(FinalityVote {
            validator: addr(2),
            block_hash,
            height,
            weight: 100,
        }));
        assert_eq!(fg.finalized_height, 0);

        // Adding 101 more weight should finalize (total 301 > 201 threshold).
        assert!(fg.record_vote(FinalityVote {
            validator: addr(3),
            block_hash,
            height,
            weight: 101,
        }));
        assert_eq!(fg.finalized_height, 10);
        assert_eq!(fg.finalized_hash, Some(block_hash));
    }

    #[test]
    fn duplicate_votes_ignored() {
        let mut fg = FinalityGadget::new();
        fg.set_total_weight(100);

        let block_hash = hash(1);

        assert!(!fg.record_vote(FinalityVote {
            validator: addr(1),
            block_hash,
            height: 5,
            weight: 50,
        }));
        // Same validator votes again — should be ignored.
        assert!(!fg.record_vote(FinalityVote {
            validator: addr(1),
            block_hash,
            height: 5,
            weight: 50,
        }));

        // Need a different validator to push past threshold.
        assert!(fg.record_vote(FinalityVote {
            validator: addr(2),
            block_hash,
            height: 5,
            weight: 50,
        }));
    }

    #[test]
    fn no_reorg_past_finality() {
        let mut fg = FinalityGadget::new();
        fg.set_total_weight(100);

        // Finalize at height 50.
        fg.record_vote(FinalityVote {
            validator: addr(1),
            block_hash: hash(1),
            height: 50,
            weight: 100,
        });
        assert_eq!(fg.finalized_height, 50);

        // A fork at height 30 would violate finality.
        assert!(fg.would_violate_finality(30));
        assert!(fg.would_violate_finality(50));
        // A fork at height 51 is fine.
        assert!(!fg.would_violate_finality(51));
    }

    #[test]
    fn batch_finalize_multiple_blocks() {
        let mut fg = FinalityGadget::new();
        let blocks = vec![
            (10, hash(1)),
            (20, hash(2)),
            (30, hash(3)),
        ];
        let count = fg.batch_finalize(&blocks);
        assert_eq!(count, 3);
        assert_eq!(fg.finalized_height, 30);
        assert!(fg.is_finalized(&hash(1)));
        assert!(fg.is_finalized(&hash(2)));
        assert!(fg.is_finalized(&hash(3)));
    }

    #[test]
    fn long_range_attack_detection() {
        let mut fg = FinalityGadget::new();
        fg.finality_depth = 100;
        fg.batch_finalize(&[(50, hash(1))]);

        // Tip is at 200. Block at height 50 with unknown hash = attack.
        assert!(fg.is_long_range_attack(50, &hash(99), 200));

        // Known finalized hash at height 50 is not an attack.
        assert!(!fg.is_long_range_attack(50, &hash(1), 200));

        // Block at height 150 with tip 200 is within finality depth.
        assert!(!fg.is_long_range_attack(150, &hash(99), 200));
    }

    #[test]
    fn votes_for_finalized_heights_rejected() {
        let mut fg = FinalityGadget::new();
        fg.set_total_weight(100);
        fg.record_vote(FinalityVote {
            validator: addr(1),
            block_hash: hash(1),
            height: 10,
            weight: 100,
        });
        assert_eq!(fg.finalized_height, 10);

        // Vote for height 10 again should be rejected.
        assert!(!fg.record_vote(FinalityVote {
            validator: addr(2),
            block_hash: hash(2),
            height: 10,
            weight: 100,
        }));
    }

    #[test]
    fn zero_weight_never_finalizes() {
        let mut fg = FinalityGadget::new();
        // total_weight = 0 by default
        assert!(!fg.record_vote(FinalityVote {
            validator: addr(1),
            block_hash: hash(1),
            height: 1,
            weight: 100,
        }));
        assert_eq!(fg.finalized_height, 0);
    }
}
