use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use crate::identity::Address;

/// The five resource channels in Commputer's multi-dimensional PoW.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum ResourceChannel {
    /// Proof of Processing — CPU compute verification.
    Processing,
    /// Proof of GPU — matrix operations / ML micro-benchmarks.
    Gpu,
    /// Proof of Storage — data retrievability challenges.
    Storage,
    /// Proof of RAM — memory-hard challenges.
    Ram,
    /// Proof of Bandwidth — timed data transfer.
    Bandwidth,
}

impl ResourceChannel {
    /// All five resource channels.
    pub const ALL: [Self; 5] = [
        Self::Processing,
        Self::Gpu,
        Self::Storage,
        Self::Ram,
        Self::Bandwidth,
    ];

    /// Minimum emission floor percentage (basis points, 10000 = 100%).
    pub fn emission_floor_bps(&self) -> u32 {
        match self {
            Self::Processing => 1000, // 10%
            Self::Gpu => 1000,        // 10%
            Self::Storage => 1000,    // 10%
            Self::Ram => 500,         // 5%
            Self::Bandwidth => 500,   // 5%
        }
    }
}

/// A challenge issued by the network to a validator for a specific resource channel.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ProofChallenge {
    /// Which resource this challenge targets.
    pub channel: ResourceChannel,
    /// Unique challenge identifier.
    pub challenge_id: [u8; 32],
    /// The epoch this challenge was issued in.
    pub epoch: u64,
    /// The validator being challenged.
    pub target: Address,
    /// Challenge-specific payload (varies by channel).
    pub payload: Vec<u8>,
    /// Deadline (block height) by which response must arrive.
    pub deadline_block: u64,
}

/// A validator's response to a proof challenge.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ProofResponse {
    /// The challenge being responded to.
    pub challenge_id: [u8; 32],
    /// The responding validator.
    pub validator: Address,
    /// Response payload (varies by channel).
    pub result: Vec<u8>,
    /// Time taken to compute response (self-reported, verified by timing).
    pub compute_time_ms: u64,
    /// Signature over (challenge_id || result).
    pub signature: Vec<u8>,
}

/// Result of verifying a proof response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofVerdict {
    /// Proof is valid — validator demonstrated the claimed resource.
    Valid,
    /// Proof is invalid — wrong answer, too slow, or signature mismatch.
    Invalid,
    /// Proof timed out — no response before deadline.
    TimedOut,
    /// Proof is suspicious — correct but timing suggests resource mismatch.
    Suspicious,
}

/// Aggregated proof results for a validator across all channels in an epoch.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct EpochProofSummary {
    pub validator: Address,
    pub epoch: u64,
    pub processing_score: u32,
    pub gpu_score: u32,
    pub storage_score: u32,
    pub ram_score: u32,
    pub bandwidth_score: u32,
    /// Diversity bonus (0-100). Higher if contributing across multiple channels.
    pub diversity_bonus: u8,
}

impl EpochProofSummary {
    /// Count how many proof channels this validator contributed to (score > 0).
    pub fn active_channel_count(&self) -> usize {
        let channels = [
            self.processing_score,
            self.gpu_score,
            self.storage_score,
            self.ram_score,
            self.bandwidth_score,
        ];
        channels.iter().filter(|&&s| s > 0).count()
    }

    /// Composite Resource Score using sub-linear R^0.7 formula per channel,
    /// with DIVERSITY_MULTIPLIER applied based on active channel count.
    /// Each channel score is capped at the gold-standard reference,
    /// then raised to the power 0.7 and summed.
    /// The diversity multiplier from token.rs rewards well-rounded nodes
    /// (up to 5% bonus for all 5 channels).
    pub fn composite_score(&self) -> u64 {
        let raw_channels = [
            self.processing_score,
            self.gpu_score,
            self.storage_score,
            self.ram_score,
            self.bandwidth_score,
        ];

        // Cap each channel at the gold-standard reference score.
        let channels: Vec<f64> = raw_channels
            .iter()
            .enumerate()
            .map(|(i, &s)| crate::token::cap_at_reference(i, s) as f64)
            .collect();

        // Sub-linear: R^0.7 per channel. A score of 100 -> 100^0.7 ≈ 25.1
        // This means doubling resources gives less than double the score.
        let base: f64 = channels.iter()
            .map(|&r| if r > 0.0 { r.powf(0.7) } else { 0.0 })
            .sum();

        // Apply DIVERSITY_MULTIPLIER based on active channel count.
        // Values are percentages: [100, 100, 101, 102, 103, 105].
        let channel_count = self.active_channel_count().min(5);
        let multiplier = crate::token::DIVERSITY_MULTIPLIER[channel_count];

        // Scale to integer. Multiply by 100 for precision, then apply multiplier.
        (base * 100.0 * multiplier as f64 / 100.0).round() as u64
    }
}
