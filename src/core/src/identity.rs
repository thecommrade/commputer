use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Sha256, Digest};

/// A 32-byte address derived from the validator's public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Address(pub [u8; 32]);

impl Address {
    pub fn from_public_key(key: &VerifyingKey) -> Self {
        let hash = Sha256::digest(key.as_bytes());
        let mut addr = [0u8; 32];
        addr.copy_from_slice(&hash);
        Self(addr)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "comme:{}", hex::encode(&self.0[..8]))
    }
}

/// Hardware fingerprint reported by a validator node.
/// Used for Sybil detection — identical fingerprints across nodes is a flag.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct HardwareFingerprint {
    pub cpu_model: String,
    pub cpu_cores: u16,
    pub ram_total_mb: u64,
    pub gpu_model: Option<String>,
    pub gpu_vram_mb: Option<u64>,
    pub storage_total_mb: u64,
    pub os_family: String,
}

/// Reported resource capacity of a validator node.
/// Measured relative to the reference node defined by the gold standard.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ResourceCapacity {
    /// CPU score relative to reference node (100 = reference baseline).
    pub cpu_score: u32,
    /// GPU score relative to reference node (100 = reference baseline, 0 = no GPU).
    pub gpu_score: u32,
    /// Available RAM in MB.
    pub ram_available_mb: u64,
    /// Available storage in MB.
    pub storage_available_mb: u64,
    /// Measured bandwidth in kbps.
    pub bandwidth_kbps: u64,
    /// Percentage of resources contributed (1-100).
    pub contribution_percent: u8,
}

/// Full validator identity on the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorIdentity {
    pub address: Address,
    pub public_key: VerifyingKey,
    pub hardware: HardwareFingerprint,
    pub capacity: ResourceCapacity,
    /// When this validator first registered on-chain.
    pub registered_epoch: u64,
    /// Cumulative uptime in seconds since registration.
    pub cumulative_uptime_secs: u64,
}

impl ValidatorIdentity {
    /// Contribution time as a Duration for grace period calculations.
    pub fn contribution_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.cumulative_uptime_secs)
    }

    /// Grace period balance in seconds. Equal to cumulative uptime,
    /// capped at 10 years.
    pub fn grace_period_secs(&self) -> u64 {
        const TEN_YEARS_SECS: u64 = 10 * 365 * 24 * 3600;
        self.cumulative_uptime_secs.min(TEN_YEARS_SECS)
    }
}
