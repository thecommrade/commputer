//! Feature 129: Consensus metrics tracking.
//! Tracks: blocks produced, blocks orphaned, forks detected, reorgs executed, finality depth.

use std::sync::atomic::{AtomicU64, Ordering};

/// Consensus performance metrics.
/// Uses atomic counters for thread-safe updates.
#[derive(Debug, Default)]
pub struct ConsensusMetrics {
    /// Total blocks produced by this node.
    pub blocks_produced: AtomicU64,
    /// Total blocks orphaned (not included in the canonical chain).
    pub blocks_orphaned: AtomicU64,
    /// Total forks detected (multiple candidates at same height).
    pub forks_detected: AtomicU64,
    /// Total reorgs executed (switching canonical chain tip).
    pub reorgs_executed: AtomicU64,
    /// Current finality depth (how many blocks back is the last finalized).
    pub finality_depth: AtomicU64,
    /// Total consensus rounds executed.
    pub consensus_rounds: AtomicU64,
    /// Total view changes triggered.
    pub view_changes: AtomicU64,
    /// Total equivocations detected.
    pub equivocations_detected: AtomicU64,
    /// Total blocks validated (received from peers).
    pub blocks_validated: AtomicU64,
    /// Total blocks rejected (failed validation).
    pub blocks_rejected: AtomicU64,
}

impl ConsensusMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_block_produced(&self) {
        self.blocks_produced.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_block_orphaned(&self) {
        self.blocks_orphaned.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fork_detected(&self) {
        self.forks_detected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_reorg(&self) {
        self.reorgs_executed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_finality_depth(&self, depth: u64) {
        self.finality_depth.store(depth, Ordering::Relaxed);
    }

    pub fn record_consensus_round(&self) {
        self.consensus_rounds.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_view_change(&self) {
        self.view_changes.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_equivocation(&self) {
        self.equivocations_detected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_block_validated(&self) {
        self.blocks_validated.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_block_rejected(&self) {
        self.blocks_rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a snapshot of all metrics as a struct with plain u64 values.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            blocks_produced: self.blocks_produced.load(Ordering::Relaxed),
            blocks_orphaned: self.blocks_orphaned.load(Ordering::Relaxed),
            forks_detected: self.forks_detected.load(Ordering::Relaxed),
            reorgs_executed: self.reorgs_executed.load(Ordering::Relaxed),
            finality_depth: self.finality_depth.load(Ordering::Relaxed),
            consensus_rounds: self.consensus_rounds.load(Ordering::Relaxed),
            view_changes: self.view_changes.load(Ordering::Relaxed),
            equivocations_detected: self.equivocations_detected.load(Ordering::Relaxed),
            blocks_validated: self.blocks_validated.load(Ordering::Relaxed),
            blocks_rejected: self.blocks_rejected.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time snapshot of consensus metrics.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub blocks_produced: u64,
    pub blocks_orphaned: u64,
    pub forks_detected: u64,
    pub reorgs_executed: u64,
    pub finality_depth: u64,
    pub consensus_rounds: u64,
    pub view_changes: u64,
    pub equivocations_detected: u64,
    pub blocks_validated: u64,
    pub blocks_rejected: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_increment() {
        let m = ConsensusMetrics::new();
        m.record_block_produced();
        m.record_block_produced();
        m.record_fork_detected();
        m.record_equivocation();

        let snap = m.snapshot();
        assert_eq!(snap.blocks_produced, 2);
        assert_eq!(snap.forks_detected, 1);
        assert_eq!(snap.equivocations_detected, 1);
        assert_eq!(snap.blocks_orphaned, 0);
    }

    #[test]
    fn set_finality_depth() {
        let m = ConsensusMetrics::new();
        m.set_finality_depth(42);
        assert_eq!(m.snapshot().finality_depth, 42);
        m.set_finality_depth(100);
        assert_eq!(m.snapshot().finality_depth, 100);
    }

    #[test]
    fn default_is_zero() {
        let snap = ConsensusMetrics::new().snapshot();
        assert_eq!(snap.blocks_produced, 0);
        assert_eq!(snap.blocks_orphaned, 0);
        assert_eq!(snap.forks_detected, 0);
        assert_eq!(snap.reorgs_executed, 0);
        assert_eq!(snap.finality_depth, 0);
    }
}
