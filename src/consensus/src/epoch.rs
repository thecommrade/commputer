use commputer_core::proof::{EpochProofSummary, ResourceChannel};
use commputer_core::identity::Address;
use std::collections::HashMap;

/// Duration of one epoch in seconds.
/// Epochs are the time window over which proof scores are aggregated
/// and emission is distributed.
pub const EPOCH_DURATION_SECS: u64 = 3600; // 1 hour

/// State of the current epoch.
#[derive(Debug, Clone)]
pub struct EpochState {
    /// Current epoch number.
    pub epoch: u64,
    /// Start timestamp of this epoch (unix secs).
    pub start_time: u64,
    /// Accumulated proof summaries for this epoch, keyed by validator.
    pub summaries: HashMap<Address, EpochProofSummary>,
    /// Total network resource demand per channel this epoch.
    /// Used for demand-weighted emission allocation.
    pub demand: HashMap<ResourceChannel, u64>,
}

impl EpochState {
    pub fn new(epoch: u64, start_time: u64) -> Self {
        let mut demand = HashMap::new();
        for channel in ResourceChannel::ALL {
            demand.insert(channel, 0);
        }
        Self {
            epoch,
            start_time,
            summaries: HashMap::new(),
            demand,
        }
    }

    /// Record a proof summary for a validator.
    pub fn record_summary(&mut self, summary: EpochProofSummary) {
        self.summaries.insert(summary.validator, summary);
    }

    /// Record demand for a resource channel (from burst compute jobs, flagship needs, etc.).
    pub fn record_demand(&mut self, channel: ResourceChannel, amount: u64) {
        *self.demand.entry(channel).or_insert(0) += amount;
    }

    /// Total number of active validators this epoch.
    pub fn validator_count(&self) -> usize {
        self.summaries.len()
    }

    /// Whether this epoch has ended based on current time.
    pub fn is_expired(&self, current_time: u64) -> bool {
        current_time >= self.start_time + EPOCH_DURATION_SECS
    }
}

/// An epoch that has been finalized — immutable record of one hour of network activity.
#[derive(Debug, Clone)]
pub struct Epoch {
    pub number: u64,
    pub start_time: u64,
    pub end_time: u64,
    pub validator_count: u64,
    pub summaries: Vec<EpochProofSummary>,
    pub demand: HashMap<ResourceChannel, u64>,
    /// Total $COMME emitted this epoch (in raw units).
    pub total_emission: u64,
    /// Emission breakdown per channel.
    pub channel_emission: HashMap<ResourceChannel, u64>,
}
