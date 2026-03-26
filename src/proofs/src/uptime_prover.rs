//! Item 156: New proof channel: Uptime.
//!
//! Measures and verifies node uptime. Longer uptime = higher score.
//! Requires signed timestamps at regular intervals as heartbeats.

use commputer_core::identity::Address;
use sha2::{Digest, Sha256};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

/// A signed heartbeat proving the node was online at a given time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UptimeHeartbeat {
    /// The validator sending the heartbeat.
    pub validator: Address,
    /// Unix timestamp (seconds) when the heartbeat was generated.
    pub timestamp: u64,
    /// The block height at the time of the heartbeat.
    pub block_height: u64,
    /// Hash of (validator || timestamp || block_height) for tamper detection.
    pub hash: [u8; 32],
    /// Signature over the hash (placeholder; would use ed25519 in production).
    pub signature: Vec<u8>,
}

/// Uptime proof result for a validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UptimeProofResult {
    pub validator: Address,
    /// Total uptime in seconds during the epoch.
    pub uptime_seconds: u64,
    /// Number of heartbeats received.
    pub heartbeat_count: u64,
    /// Uptime percentage (0-100).
    pub uptime_percent: f64,
    /// Score (0-100) based on uptime.
    pub score: u32,
}

/// Maximum gap between heartbeats before considered offline (seconds).
const MAX_GAP_SECS: u64 = 180;

/// Prover and verifier for the uptime proof channel.
pub struct UptimeProver {
    /// Collected heartbeats per validator.
    heartbeats: HashMap<Address, Vec<UptimeHeartbeat>>,
    /// Epoch start timestamp.
    pub epoch_start_timestamp: u64,
    /// Epoch duration in seconds.
    pub epoch_duration_secs: u64,
}

impl UptimeProver {
    /// Create a new uptime prover.
    pub fn new(epoch_start_timestamp: u64, epoch_duration_secs: u64) -> Self {
        Self {
            heartbeats: HashMap::new(),
            epoch_start_timestamp,
            epoch_duration_secs,
        }
    }

    /// Generate a heartbeat for a validator.
    pub fn generate_heartbeat(
        validator: Address,
        timestamp: u64,
        block_height: u64,
    ) -> UptimeHeartbeat {
        let hash = Self::compute_heartbeat_hash(&validator, timestamp, block_height);
        UptimeHeartbeat {
            validator,
            timestamp,
            block_height,
            hash,
            signature: vec![], // Would be signed in production.
        }
    }

    /// Verify a heartbeat's hash is correct.
    pub fn verify_heartbeat(heartbeat: &UptimeHeartbeat) -> bool {
        let expected = Self::compute_heartbeat_hash(
            &heartbeat.validator,
            heartbeat.timestamp,
            heartbeat.block_height,
        );
        expected == heartbeat.hash
    }

    /// Record a heartbeat.
    pub fn record_heartbeat(&mut self, heartbeat: UptimeHeartbeat) -> bool {
        if !Self::verify_heartbeat(&heartbeat) {
            return false;
        }
        self.heartbeats
            .entry(heartbeat.validator)
            .or_default()
            .push(heartbeat);
        true
    }

    /// Compute uptime proof result for a validator at epoch end.
    pub fn compute_uptime(&self, validator: &Address) -> UptimeProofResult {
        let empty = vec![];
        let beats = self.heartbeats.get(validator).unwrap_or(&empty);

        if beats.is_empty() {
            return UptimeProofResult {
                validator: *validator,
                uptime_seconds: 0,
                heartbeat_count: 0,
                uptime_percent: 0.0,
                score: 0,
            };
        }

        let mut sorted_timestamps: Vec<u64> = beats.iter().map(|b| b.timestamp).collect();
        sorted_timestamps.sort();

        // Calculate uptime: sum of intervals where gap < MAX_GAP_SECS.
        let mut uptime_secs: u64 = 0;
        for window in sorted_timestamps.windows(2) {
            let gap = window[1] - window[0];
            if gap <= MAX_GAP_SECS {
                uptime_secs += gap;
            }
        }

        let uptime_percent = if self.epoch_duration_secs == 0 {
            0.0
        } else {
            (uptime_secs as f64 / self.epoch_duration_secs as f64 * 100.0).clamp(0.0, 100.0)
        };

        let score = uptime_percent.round() as u32;

        UptimeProofResult {
            validator: *validator,
            uptime_seconds: uptime_secs,
            heartbeat_count: beats.len() as u64,
            uptime_percent,
            score,
        }
    }

    /// Compute uptime for all tracked validators.
    pub fn compute_all_uptimes(&self) -> Vec<UptimeProofResult> {
        self.heartbeats.keys().map(|v| self.compute_uptime(v)).collect()
    }

    /// Get the number of tracked validators.
    pub fn tracked_count(&self) -> usize {
        self.heartbeats.len()
    }

    fn compute_heartbeat_hash(validator: &Address, timestamp: u64, block_height: u64) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"uptime_heartbeat:");
        hasher.update(validator.0);
        hasher.update(timestamp.to_le_bytes());
        hasher.update(block_height.to_le_bytes());
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    #[test]
    fn item_156_generate_and_verify_heartbeat() {
        let hb = UptimeProver::generate_heartbeat(test_addr(1), 1000, 42);
        assert!(UptimeProver::verify_heartbeat(&hb));
    }

    #[test]
    fn item_156_tampered_heartbeat_fails() {
        let mut hb = UptimeProver::generate_heartbeat(test_addr(1), 1000, 42);
        hb.timestamp = 2000; // Tamper
        assert!(!UptimeProver::verify_heartbeat(&hb));
    }

    #[test]
    fn item_156_compute_uptime() {
        let mut prover = UptimeProver::new(0, 600); // 10-minute epoch

        // Send heartbeats every 60 seconds for 10 minutes.
        for i in 0..10 {
            let hb = UptimeProver::generate_heartbeat(test_addr(1), i * 60, i as u64);
            prover.record_heartbeat(hb);
        }

        let result = prover.compute_uptime(&test_addr(1));
        assert_eq!(result.heartbeat_count, 10);
        assert!(result.uptime_seconds > 0);
        assert!(result.uptime_percent > 0.0);
        assert!(result.score > 0);
    }

    #[test]
    fn item_156_no_heartbeats_zero_uptime() {
        let prover = UptimeProver::new(0, 600);
        let result = prover.compute_uptime(&test_addr(1));
        assert_eq!(result.score, 0);
        assert_eq!(result.uptime_seconds, 0);
    }

    #[test]
    fn item_156_gap_detection() {
        let mut prover = UptimeProver::new(0, 600);

        // First 3 heartbeats, then a 5-minute gap, then 2 more.
        for i in 0..3 {
            let hb = UptimeProver::generate_heartbeat(test_addr(1), i * 60, i as u64);
            prover.record_heartbeat(hb);
        }
        // Big gap
        let hb = UptimeProver::generate_heartbeat(test_addr(1), 420, 7);
        prover.record_heartbeat(hb);
        let hb = UptimeProver::generate_heartbeat(test_addr(1), 480, 8);
        prover.record_heartbeat(hb);

        let result = prover.compute_uptime(&test_addr(1));
        // Should only count the connected periods, not the 5-min gap.
        assert!(result.uptime_seconds < 600);
        assert!(result.uptime_percent < 100.0);
    }

    #[test]
    fn item_156_compute_all_uptimes() {
        let mut prover = UptimeProver::new(0, 600);

        let hb1 = UptimeProver::generate_heartbeat(test_addr(1), 0, 0);
        let hb2 = UptimeProver::generate_heartbeat(test_addr(2), 0, 0);
        prover.record_heartbeat(hb1);
        prover.record_heartbeat(hb2);

        let results = prover.compute_all_uptimes();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn item_156_reject_invalid_heartbeat() {
        let mut prover = UptimeProver::new(0, 600);
        let mut hb = UptimeProver::generate_heartbeat(test_addr(1), 1000, 42);
        hb.hash[0] ^= 0xFF; // Corrupt
        assert!(!prover.record_heartbeat(hb));
        assert_eq!(prover.tracked_count(), 0);
    }
}
