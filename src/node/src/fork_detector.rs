use tracing::warn;

/// Default number of consecutive parent hash mismatches before triggering resync.
pub const DEFAULT_FORK_THRESHOLD: u32 = 3;

/// Tracks consecutive parent hash mismatches during block finalization.
/// After `threshold` consecutive mismatches, signals that the node should
/// wipe its chain and resync from peers.
pub struct ForkDetector {
    consecutive_mismatches: u32,
    threshold: u32,
}

impl ForkDetector {
    pub fn new() -> Self {
        Self {
            consecutive_mismatches: 0,
            threshold: DEFAULT_FORK_THRESHOLD,
        }
    }

    /// Record a parent hash mismatch during block finalization.
    pub fn record_mismatch(&mut self) {
        self.consecutive_mismatches += 1;
        if self.consecutive_mismatches >= self.threshold {
            warn!(
                consecutive = self.consecutive_mismatches,
                threshold = self.threshold,
                "fork_detector: mismatch threshold reached, resync recommended"
            );
        }
    }

    /// Record a successful block application (resets the counter).
    pub fn record_success(&mut self) {
        self.consecutive_mismatches = 0;
    }

    /// Whether the node should wipe and resync from peers.
    pub fn should_resync(&self) -> bool {
        self.consecutive_mismatches >= self.threshold
    }

    /// Current count of consecutive mismatches (for logging).
    pub fn consecutive_mismatches(&self) -> u32 {
        self.consecutive_mismatches
    }

    /// Reset all state (used after a resync completes).
    pub fn reset(&mut self) {
        self.consecutive_mismatches = 0;
    }
}

impl Default for ForkDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_mismatch_no_resync() {
        let mut fd = ForkDetector::new();
        fd.record_mismatch();
        assert!(!fd.should_resync());
    }

    #[test]
    fn threshold_triggers_resync() {
        let mut fd = ForkDetector::new();
        fd.record_mismatch();
        fd.record_mismatch();
        fd.record_mismatch();
        assert!(fd.should_resync());
    }

    #[test]
    fn success_resets_counter() {
        let mut fd = ForkDetector::new();
        fd.record_mismatch();
        fd.record_mismatch();
        fd.record_success();
        assert_eq!(fd.consecutive_mismatches(), 0);
        fd.record_mismatch();
        assert!(!fd.should_resync());
    }

    #[test]
    fn reset_clears_state() {
        let mut fd = ForkDetector::new();
        fd.record_mismatch();
        fd.record_mismatch();
        fd.record_mismatch();
        assert!(fd.should_resync());
        fd.reset();
        assert!(!fd.should_resync());
        assert_eq!(fd.consecutive_mismatches(), 0);
    }
}
