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

    /// Feature 140: Compute adaptive nerf rate based on network-wide nerfed ratio.
    /// Formula: 8000 + (nerfed_ratio * 2000) bps, capped at 10000.
    /// As more validators are nerfed, the nerf penalty slides toward 100%.
    pub fn compute_adaptive(nerfed_count: u64, total_validators: u64) -> u32 {
        if total_validators == 0 {
            return 8000;
        }
        let nerfed_ratio = nerfed_count as f64 / total_validators as f64;
        let rate = 8000.0 + (nerfed_ratio * 2000.0);
        (rate.round() as u32).min(10000)
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
    /// Feature 138: Same /16 subnet detected.
    SameSubnet16 { peer_address: Address },
    /// Feature 138: Same ASN detected.
    SameAsn { asn: String, peer_address: Address },
    /// Feature 148: Validator running on known datacenter IP range.
    DatacenterIp { provider: String },
    /// Feature 149: Multiple validators behind same IP (VPN/proxy suspected).
    VpnProxy { ip: String, validator_count: usize },
    /// Feature 138: Geographic proximity (same /16 or ASN).
    GeographicProximity { peer_address: Address },
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

/// Feature 146: Compliance summary included in blocks by block producers.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ComplianceSummary {
    /// Number of currently nerfed validators.
    pub nerfed_count: u64,
    /// Current total nerf percentage in basis points.
    pub total_nerf_percentage: u32,
    /// Number of active suspicion flags across all validators.
    pub suspicion_flags: u64,
}

/// Feature 139: Exponential decay multiplier for multi-node operators.
/// First node: 100%, second: 25%, third: 6.25%, fourth: 1.5625%, fifth+: 0%.
pub fn multi_node_multiplier(node_count: u32) -> f64 {
    match node_count {
        0 => 0.0,
        1 => 1.0,
        2 => 0.25,
        3 => 0.0625,
        4 => 0.015625,
        _ => 0.0, // 5+ nodes get zero rewards
    }
}

/// Feature 150: Anti-scale metrics for the /anti-scale RPC endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntiScaleMetrics {
    /// Total number of warehouse-pattern detections.
    pub total_warehouse_detections: u64,
    /// Total rewards nerfed (in raw units).
    pub total_nerfed_rewards: u64,
    /// History of nerf percentage changes: (epoch, bps).
    pub nerf_percentage_history: Vec<(u64, u32)>,
    /// Largest detected clusters: (cluster_size, sample_address_hex).
    pub largest_detected_clusters: Vec<(usize, String)>,
}
