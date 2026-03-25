use commputer_core::proof::{EpochProofSummary, ResourceChannel};
use commputer_core::identity::Address;
use std::collections::{HashMap, HashSet};

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
    /// Feature 114: Per-channel difficulty multiplier. Adjusted at epoch end
    /// based on proof pass rates.
    pub difficulty_multiplier: HashMap<ResourceChannel, f64>,
    /// Feature 124: Active validator set for this epoch. Only these validators
    /// can participate and earn rewards. Snapshotted at epoch transition.
    pub active_validators: HashSet<Address>,
}

impl EpochState {
    pub fn new(epoch: u64, start_time: u64) -> Self {
        let mut demand = HashMap::new();
        let mut difficulty_multiplier = HashMap::new();
        for channel in ResourceChannel::ALL {
            demand.insert(channel, 0);
            difficulty_multiplier.insert(channel, 1.0);
        }
        Self {
            epoch,
            start_time,
            summaries: HashMap::new(),
            demand,
            difficulty_multiplier,
            active_validators: HashSet::new(),
        }
    }

    /// Feature 114: Compute next epoch's difficulty multipliers based on
    /// pass rates this epoch. If > 80% of validators pass a channel easily,
    /// increase difficulty by 10%. If < 40% pass, decrease by 10%.
    pub fn compute_next_difficulty(&self) -> HashMap<ResourceChannel, f64> {
        let validator_count = self.summaries.len() as f64;
        if validator_count == 0.0 {
            return self.difficulty_multiplier.clone();
        }

        let mut next = self.difficulty_multiplier.clone();
        for channel in ResourceChannel::ALL {
            let pass_count = self.summaries.values().filter(|s| {
                let score = match channel {
                    ResourceChannel::Processing => s.processing_score,
                    ResourceChannel::Gpu => s.gpu_score,
                    ResourceChannel::Storage => s.storage_score,
                    ResourceChannel::Ram => s.ram_score,
                    ResourceChannel::Bandwidth => s.bandwidth_score,
                };
                score >= 80
            }).count() as f64;

            let pass_rate = pass_count / validator_count;
            let current = self.difficulty_multiplier.get(&channel).copied().unwrap_or(1.0);
            let adjusted = if pass_rate > 0.8 {
                (current * 1.1).min(5.0) // Cap at 5x
            } else if pass_rate < 0.4 {
                (current * 0.9).max(0.2) // Floor at 0.2x
            } else {
                current
            };
            next.insert(channel, adjusted);
        }
        next
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

    /// Feature 124: Snapshot current validators as the active set for this epoch.
    pub fn snapshot_validators(&mut self, validators: HashSet<Address>) {
        self.active_validators = validators;
    }

    /// Feature 124: Check if a validator is in the active set for this epoch.
    pub fn is_active_validator(&self, addr: &Address) -> bool {
        // If active_validators is empty (genesis/bootstrap), allow all
        self.active_validators.is_empty() || self.active_validators.contains(addr)
    }
}

/// Feature 126: Epoch summary event emitted at epoch boundaries.
#[derive(Debug, Clone)]
pub struct EpochSummary {
    pub epoch: u64,
    pub validator_count: u64,
    pub total_emission: u64,
    pub difficulty_adjustments: HashMap<ResourceChannel, f64>,
    pub active_validator_count: usize,
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
