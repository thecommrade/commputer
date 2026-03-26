//! Feature 131: Consensus parameter tuning.
//! Makes Snowball k (sample_size), alpha (quorum), beta (decision_threshold)
//! configurable via genesis configuration. Documents optimal parameter values.

use crate::snowball::SnowballParams;

/// Consensus configuration — all tuneable consensus parameters.
/// Can be loaded from genesis.json or set programmatically.
#[derive(Debug, Clone)]
pub struct ConsensusConfig {
    /// Snowball sample size (k): number of peers polled each round.
    /// Production default: 20. Must be >= 1.
    pub sample_size: usize,

    /// Snowball quorum threshold (alpha): number of agreeing peers required.
    /// Must be > k/2 for liveness. Production default: 14 (70% of k=20).
    pub quorum: usize,

    /// Snowball decision threshold (beta): consecutive rounds of agreement needed.
    /// Higher = more secure but slower finality. Production default: 20.
    pub decision_threshold: u32,

    /// Maximum block production rate: minimum seconds between blocks per validator.
    /// Default: 2 seconds.
    pub min_block_interval_secs: u64,

    /// Consensus timeout: force re-election after this many seconds.
    /// Default: 30 seconds.
    pub consensus_timeout_secs: u64,

    /// View change timeout: if elected producer is offline for this many seconds,
    /// allow the next validator to produce. Default: 10 seconds.
    pub view_change_timeout_secs: u64,

    /// Finality depth: blocks deeper than this from the tip are considered final.
    /// Default: 100.
    pub finality_depth: u64,

    /// Maximum timestamp drift from network median (seconds).
    /// Blocks with timestamps outside this range are rejected.
    /// Default: 15 seconds.
    pub max_timestamp_drift_secs: u64,

    /// Checkpoint interval: every N blocks, validators sign a checkpoint.
    /// Default: 1000.
    pub checkpoint_interval: u64,
}

impl ConsensusConfig {
    /// Production-ready configuration for mainnet.
    pub fn production() -> Self {
        Self {
            sample_size: 20,
            quorum: 14,
            decision_threshold: 20,
            min_block_interval_secs: 2,
            consensus_timeout_secs: 30,
            view_change_timeout_secs: 10,
            finality_depth: 100,
            max_timestamp_drift_secs: 15,
            checkpoint_interval: 1000,
        }
    }

    /// Testing configuration with faster finality.
    pub fn testing() -> Self {
        Self {
            sample_size: 3,
            quorum: 2,
            decision_threshold: 5,
            min_block_interval_secs: 1,
            consensus_timeout_secs: 10,
            view_change_timeout_secs: 5,
            finality_depth: 10,
            max_timestamp_drift_secs: 60,
            checkpoint_interval: 100,
        }
    }

    /// Convert to SnowballParams.
    pub fn to_snowball_params(&self) -> SnowballParams {
        SnowballParams {
            sample_size: self.sample_size,
            quorum: self.quorum,
            decision_threshold: self.decision_threshold,
        }
    }

    /// Validate the configuration. Returns an error message if invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.sample_size == 0 {
            return Err("sample_size must be >= 1".into());
        }
        if self.quorum == 0 {
            return Err("quorum must be >= 1".into());
        }
        if self.quorum > self.sample_size {
            return Err(format!(
                "quorum ({}) must be <= sample_size ({})",
                self.quorum, self.sample_size
            ));
        }
        // Quorum must be > k/2 for liveness.
        if self.quorum <= self.sample_size / 2 {
            return Err(format!(
                "quorum ({}) must be > sample_size/2 ({}) for liveness",
                self.quorum,
                self.sample_size / 2
            ));
        }
        if self.decision_threshold == 0 {
            return Err("decision_threshold must be >= 1".into());
        }
        if self.min_block_interval_secs == 0 {
            return Err("min_block_interval_secs must be >= 1".into());
        }
        if self.finality_depth == 0 {
            return Err("finality_depth must be >= 1".into());
        }
        Ok(())
    }
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self::production()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_config_valid() {
        let config = ConsensusConfig::production();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn testing_config_valid() {
        let config = ConsensusConfig::testing();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn invalid_quorum_too_large() {
        let mut config = ConsensusConfig::testing();
        config.quorum = 100;
        assert!(config.validate().is_err());
    }

    #[test]
    fn invalid_quorum_too_small() {
        let mut config = ConsensusConfig::testing();
        config.sample_size = 10;
        config.quorum = 5; // Not > k/2
        assert!(config.validate().is_err());
    }

    #[test]
    fn zero_sample_size_invalid() {
        let mut config = ConsensusConfig::testing();
        config.sample_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn to_snowball_params() {
        let config = ConsensusConfig::production();
        let params = config.to_snowball_params();
        assert_eq!(params.sample_size, 20);
        assert_eq!(params.quorum, 14);
        assert_eq!(params.decision_threshold, 20);
    }

    #[test]
    fn default_is_production() {
        let default = ConsensusConfig::default();
        let prod = ConsensusConfig::production();
        assert_eq!(default.sample_size, prod.sample_size);
        assert_eq!(default.quorum, prod.quorum);
        assert_eq!(default.decision_threshold, prod.decision_threshold);
    }
}
