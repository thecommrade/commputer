//! Feature 130: Optimistic block execution.
//! Start executing the next block while the current block is being finalized.
//! If the finalized block matches the optimistic execution, the result is already ready.

use std::collections::HashMap;
use commputer_core::block::BlockHash;

/// Result of an optimistic block execution.
#[derive(Debug, Clone)]
pub struct OptimisticResult {
    /// The block hash this result was computed for.
    pub block_hash: BlockHash,
    /// Height of the block.
    pub height: u64,
    /// State root after applying this block.
    pub state_root: [u8; 32],
    /// Whether this result is still valid (not invalidated by a different finalization).
    pub valid: bool,
}

/// Manages optimistic execution of upcoming blocks.
/// When a block candidate arrives, we can speculatively execute it
/// before consensus finalizes. If the candidate wins, the work is reused.
#[derive(Debug, Default)]
pub struct OptimisticExecutor {
    /// Cached results of optimistic execution, keyed by block hash.
    results: HashMap<BlockHash, OptimisticResult>,
    /// The height we're currently optimistically executing for.
    pub pending_height: Option<u64>,
    /// How many optimistic results hit (matched finalization).
    pub hits: u64,
    /// How many optimistic results missed (finalized block differed).
    pub misses: u64,
}

impl OptimisticExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin optimistic execution for a block candidate.
    /// In production, this would fork the state and execute transactions.
    /// Here we record the intent and return a handle for the result.
    pub fn begin_execution(
        &mut self,
        block_hash: BlockHash,
        height: u64,
        state_root: [u8; 32],
    ) {
        self.pending_height = Some(height);
        self.results.insert(block_hash, OptimisticResult {
            block_hash,
            height,
            state_root,
            valid: true,
        });
    }

    /// Check if we have a valid optimistic result for a given block.
    pub fn get_result(&self, block_hash: &BlockHash) -> Option<&OptimisticResult> {
        self.results.get(block_hash).filter(|r| r.valid)
    }

    /// Called when a block is finalized. If we optimistically executed it, record a hit.
    /// If we executed a different block at that height, record a miss and invalidate.
    pub fn on_finalized(&mut self, finalized_hash: BlockHash, height: u64) {
        let mut had_result = false;
        let mut was_hit = false;

        for (hash, result) in &mut self.results {
            if result.height == height {
                had_result = true;
                if *hash == finalized_hash {
                    was_hit = true;
                } else {
                    result.valid = false;
                }
            }
        }

        if had_result {
            if was_hit {
                self.hits += 1;
            } else {
                self.misses += 1;
            }
        }

        // Clean up all results at or below this height.
        self.results.retain(|_, r| r.height > height);
        if self.pending_height == Some(height) {
            self.pending_height = None;
        }
    }

    /// The hit rate of optimistic execution (0.0 to 1.0).
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Number of pending optimistic results.
    pub fn pending_count(&self) -> usize {
        self.results.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> BlockHash {
        let mut h = [0u8; 32];
        h[0] = n;
        BlockHash(h)
    }

    #[test]
    fn optimistic_hit() {
        let mut exec = OptimisticExecutor::new();
        let h = hash(1);

        exec.begin_execution(h, 10, [0u8; 32]);
        assert!(exec.get_result(&h).is_some());

        // Finalize the same block — hit!
        exec.on_finalized(h, 10);
        assert_eq!(exec.hits, 1);
        assert_eq!(exec.misses, 0);
        assert!((exec.hit_rate() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn optimistic_miss() {
        let mut exec = OptimisticExecutor::new();
        let h1 = hash(1);
        let h2 = hash(2);

        exec.begin_execution(h1, 10, [0u8; 32]);

        // Finalize a different block — miss!
        exec.on_finalized(h2, 10);
        assert_eq!(exec.hits, 0);
        assert_eq!(exec.misses, 1);
        assert!((exec.hit_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cleanup_after_finalization() {
        let mut exec = OptimisticExecutor::new();
        exec.begin_execution(hash(1), 10, [0u8; 32]);
        exec.begin_execution(hash(2), 10, [0u8; 32]);

        exec.on_finalized(hash(1), 10);
        assert_eq!(exec.pending_count(), 0); // All height 10 cleaned up.
    }

    #[test]
    fn results_at_different_heights_preserved() {
        let mut exec = OptimisticExecutor::new();
        exec.begin_execution(hash(1), 10, [0u8; 32]);
        exec.begin_execution(hash(2), 11, [0u8; 32]);

        exec.on_finalized(hash(1), 10);
        // Height 11 result should still be there.
        assert_eq!(exec.pending_count(), 1);
        assert!(exec.get_result(&hash(2)).is_some());
    }
}
