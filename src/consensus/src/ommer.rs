//! Feature 128: Uncle/ommer block tracking.
//! Tracks valid blocks that weren't selected as the canonical chain block.
//! Credits partial rewards to producers of ommer blocks.

use std::collections::HashMap;
use commputer_core::block::BlockHash;
use commputer_core::identity::Address;

/// Fraction of the full block reward credited to ommer block producers.
/// 1/8 of the canonical block reward (similar to Ethereum's uncle reward).
pub const OMMER_REWARD_FRACTION_NUMERATOR: u64 = 1;
pub const OMMER_REWARD_FRACTION_DENOMINATOR: u64 = 8;

/// Maximum number of ommers to track per height.
pub const MAX_OMMERS_PER_HEIGHT: usize = 4;

/// Maximum height difference for an ommer to be eligible for reward.
/// An ommer at height H can only be referenced by a canonical block at height H+1..H+6.
pub const MAX_OMMER_DEPTH: u64 = 6;

/// A recorded ommer (uncle) block.
#[derive(Debug, Clone)]
pub struct OmmerRecord {
    pub hash: BlockHash,
    pub height: u64,
    pub producer: Address,
    pub timestamp: u64,
    /// Whether a partial reward has been credited.
    pub reward_credited: bool,
}

/// Tracks ommer blocks across heights.
#[derive(Debug, Default)]
pub struct OmmerTracker {
    /// Ommers keyed by height.
    ommers: HashMap<u64, Vec<OmmerRecord>>,
    /// Total ommers recorded (for metrics).
    pub total_ommers: u64,
}

impl OmmerTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a valid block that lost the consensus vote (became an ommer).
    /// `canonical_hash` is the hash that won at this height.
    pub fn record_ommer(
        &mut self,
        block_hash: BlockHash,
        height: u64,
        producer: Address,
        timestamp: u64,
        canonical_hash: &BlockHash,
    ) {
        // Don't record the canonical block as an ommer.
        if block_hash == *canonical_hash {
            return;
        }

        let records = self.ommers.entry(height).or_default();

        // Don't double-record.
        if records.iter().any(|r| r.hash == block_hash) {
            return;
        }

        // Cap ommers per height.
        if records.len() >= MAX_OMMERS_PER_HEIGHT {
            return;
        }

        records.push(OmmerRecord {
            hash: block_hash,
            height,
            producer,
            timestamp,
            reward_credited: false,
        });
        self.total_ommers += 1;
    }

    /// Get all ommers at a given height.
    pub fn ommers_at_height(&self, height: u64) -> &[OmmerRecord] {
        self.ommers.get(&height).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Calculate partial rewards for ommer producers at a given height.
    /// Returns a list of (producer, reward_amount) pairs.
    pub fn calculate_ommer_rewards(
        &self,
        height: u64,
        canonical_block_reward: u64,
    ) -> Vec<(Address, u64)> {
        let ommer_reward = (canonical_block_reward * OMMER_REWARD_FRACTION_NUMERATOR)
            / OMMER_REWARD_FRACTION_DENOMINATOR;

        self.ommers_at_height(height)
            .iter()
            .filter(|r| !r.reward_credited)
            .map(|r| (r.producer, ommer_reward))
            .collect()
    }

    /// Mark ommer rewards as credited at a given height.
    pub fn mark_rewards_credited(&mut self, height: u64) {
        if let Some(records) = self.ommers.get_mut(&height) {
            for record in records {
                record.reward_credited = true;
            }
        }
    }

    /// Prune ommers older than `min_height` to save memory.
    pub fn prune_before(&mut self, min_height: u64) {
        self.ommers.retain(|&h, _| h >= min_height);
    }

    /// Count of all tracked ommer records.
    pub fn len(&self) -> usize {
        self.ommers.values().map(|v| v.len()).sum()
    }

    /// Whether the tracker is empty.
    pub fn is_empty(&self) -> bool {
        self.ommers.is_empty()
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
    fn record_and_retrieve_ommer() {
        let mut tracker = OmmerTracker::new();
        let canonical = hash(1);

        tracker.record_ommer(hash(2), 10, addr(1), 1000, &canonical);

        let ommers = tracker.ommers_at_height(10);
        assert_eq!(ommers.len(), 1);
        assert_eq!(ommers[0].hash, hash(2));
        assert_eq!(ommers[0].producer, addr(1));
    }

    #[test]
    fn canonical_block_not_recorded_as_ommer() {
        let mut tracker = OmmerTracker::new();
        let canonical = hash(1);

        tracker.record_ommer(hash(1), 10, addr(1), 1000, &canonical);
        assert!(tracker.ommers_at_height(10).is_empty());
    }

    #[test]
    fn ommer_reward_calculation() {
        let mut tracker = OmmerTracker::new();
        let canonical = hash(1);
        let block_reward = 1000;

        tracker.record_ommer(hash(2), 10, addr(1), 1000, &canonical);
        tracker.record_ommer(hash(3), 10, addr(2), 1001, &canonical);

        let rewards = tracker.calculate_ommer_rewards(10, block_reward);
        assert_eq!(rewards.len(), 2);
        // Each ommer gets 1/8 of the canonical reward.
        assert_eq!(rewards[0].1, 125); // 1000 / 8
        assert_eq!(rewards[1].1, 125);
    }

    #[test]
    fn mark_rewards_credited() {
        let mut tracker = OmmerTracker::new();
        let canonical = hash(1);

        tracker.record_ommer(hash(2), 10, addr(1), 1000, &canonical);
        tracker.mark_rewards_credited(10);

        // After marking, no more uncredited rewards.
        let rewards = tracker.calculate_ommer_rewards(10, 1000);
        assert!(rewards.is_empty());
    }

    #[test]
    fn max_ommers_per_height_respected() {
        let mut tracker = OmmerTracker::new();
        let canonical = hash(0);

        for i in 1..=10 {
            tracker.record_ommer(hash(i), 10, addr(i), 1000, &canonical);
        }

        // Only MAX_OMMERS_PER_HEIGHT should be recorded.
        assert_eq!(tracker.ommers_at_height(10).len(), MAX_OMMERS_PER_HEIGHT);
    }

    #[test]
    fn prune_old_ommers() {
        let mut tracker = OmmerTracker::new();
        let canonical = hash(0);

        tracker.record_ommer(hash(1), 10, addr(1), 1000, &canonical);
        tracker.record_ommer(hash(2), 20, addr(2), 2000, &canonical);
        tracker.record_ommer(hash(3), 30, addr(3), 3000, &canonical);

        tracker.prune_before(20);
        assert!(tracker.ommers_at_height(10).is_empty());
        assert_eq!(tracker.ommers_at_height(20).len(), 1);
        assert_eq!(tracker.ommers_at_height(30).len(), 1);
    }

    #[test]
    fn no_duplicate_ommers() {
        let mut tracker = OmmerTracker::new();
        let canonical = hash(0);

        tracker.record_ommer(hash(1), 10, addr(1), 1000, &canonical);
        tracker.record_ommer(hash(1), 10, addr(1), 1000, &canonical);

        assert_eq!(tracker.ommers_at_height(10).len(), 1);
    }
}
