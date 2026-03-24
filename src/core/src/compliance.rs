use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use crate::identity::Address;

/// Compliance status for a validator node.
/// The protocol enforces anti-scale rules at this level.
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum ComplianceStatus {
    /// Full rewards. Single compliant node.
    Compliant,
    /// 80%+ reward nerf active. Incidental non-compliance (e.g., hardware upgrade spike).
    /// Returns to Compliant immediately upon resolution.
    NerfedIncidental,
    /// 80%+ reward nerf active. Adversarial gaming detected (warehouse, spoofed nodes).
    /// Returns to Compliant only when scaled back to a single compliant node.
    NerfedAdversarial,
}

/// The current network-wide nerf percentage.
/// Starts at 80%, can only increase, targets 100% long-term.
/// Adjusts automatically based on the count of non-compliant IPs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct NerfRate {
    /// Current nerf percentage in basis points (8000 = 80%, 10000 = 100%).
    pub rate_bps: u32,
}

impl NerfRate {
    /// Starting nerf rate: 80%.
    pub const INITIAL: Self = Self { rate_bps: 8000 };

    /// The nerf rate can only increase. This enforces the protocol rule.
    pub fn increase_to(&mut self, new_rate_bps: u32) {
        if new_rate_bps > self.rate_bps && new_rate_bps <= 10000 {
            self.rate_bps = new_rate_bps;
        }
    }

    /// Calculate the reward multiplier for a nerfed validator.
    /// Returns the fraction of full rewards they receive (0.0 to 1.0).
    pub fn reward_multiplier(&self) -> f64 {
        1.0 - (self.rate_bps as f64 / 10000.0)
    }
}

/// Flags that can trigger a compliance review.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum ComplianceFlag {
    /// Multiple nodes detected from the same network (latency triangulation).
    ColocatedNodes { peer_addresses: Vec<Address> },
    /// Hardware fingerprint matches another node exactly.
    DuplicateFingerprint { matching_address: Address },
    /// Resource capacity exceeds reference node ceiling.
    ExceedsReferenceCeiling { channel: String, reported: u64, ceiling: u64 },
    /// Sudden resource spike (e.g., RAM doubled overnight).
    ResourceSpike { channel: String, before: u64, after: u64 },
    /// Uptime pattern consistent with datacenter (>99.5% uptime, flat resource curve).
    DatacenterPattern,
    /// Challenge-response timing inconsistent with reported hardware.
    TimingAnomaly { expected_ms: u64, actual_ms: u64 },
}

/// Verdict from a compliance review.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ComplianceVerdict {
    pub validator: Address,
    pub epoch: u64,
    pub previous_status: ComplianceStatus,
    pub new_status: ComplianceStatus,
    pub flags: Vec<ComplianceFlag>,
    /// Human-readable explanation for the validator's dashboard.
    pub explanation: String,
    /// What the validator should do to restore compliance.
    pub resolution_steps: Vec<String>,
}
