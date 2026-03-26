//! Item 146: Bandwidth proof bidirectional.
//!
//! Measures both upload AND download between paired validators.
//! Each side generates data, sends it to the peer, and the peer hashes it.
//! Both directions must pass for the proof to be valid.

use commputer_core::identity::Address;
use sha2::{Digest, Sha256};
use serde::{Serialize, Deserialize};

/// Bidirectional bandwidth measurement between two validators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BidirectionalBandwidth {
    /// Unique test identifier.
    pub test_id: [u8; 32],
    /// First validator (initiator).
    pub initiator: Address,
    /// Second validator (responder).
    pub responder: Address,
    /// Data size in KB for each direction.
    pub data_size_kb: u32,
    /// Epoch of the test.
    pub epoch: u64,
}

/// Report from one direction of a bidirectional bandwidth test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalReport {
    /// Which direction: true = upload (initiator -> responder), false = download.
    pub is_upload: bool,
    /// Time in milliseconds for the transfer.
    pub transfer_time_ms: u64,
    /// Hash of the transferred data.
    pub data_hash: [u8; 32],
    /// Computed throughput in KB/s.
    pub throughput_kbps: u64,
}

/// Combined result of a bidirectional bandwidth test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BidirectionalResult {
    pub test_id: [u8; 32],
    pub upload_report: DirectionalReport,
    pub download_report: DirectionalReport,
    /// Combined score (0-100).
    pub combined_score: u32,
    /// Whether both directions passed minimum thresholds.
    pub passed: bool,
}

/// Minimum throughput in KB/s for a direction to be considered passing.
const MIN_THROUGHPUT_KBPS: u64 = 100;
/// Reference throughput for perfect score (KB/s).
const REFERENCE_THROUGHPUT_KBPS: u64 = 10_000;

impl BidirectionalBandwidth {
    /// Create a new bidirectional bandwidth test.
    pub fn new(
        initiator: Address,
        responder: Address,
        data_size_kb: u32,
        epoch: u64,
        seed: &[u8; 32],
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"bidir_bw:");
        hasher.update(seed);
        hasher.update(initiator.0);
        hasher.update(responder.0);
        hasher.update(epoch.to_le_bytes());
        let id = hasher.finalize();
        let mut test_id = [0u8; 32];
        test_id.copy_from_slice(&id);

        Self {
            test_id,
            initiator,
            responder,
            data_size_kb,
            epoch,
        }
    }

    /// Generate the deterministic data payload for a given direction.
    pub fn generate_payload(seed: &[u8; 32], is_upload: bool, size_kb: u32) -> Vec<u8> {
        let size_bytes = size_kb as usize * 1024;
        let mut data = Vec::with_capacity(size_bytes);
        let direction_tag: &[u8] = if is_upload { b"upload" } else { b"download" };

        let mut counter = 0u64;
        while data.len() < size_bytes {
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update(direction_tag);
            hasher.update(counter.to_le_bytes());
            let block = hasher.finalize();
            let remaining = size_bytes - data.len();
            data.extend_from_slice(&block[..remaining.min(32)]);
            counter += 1;
        }
        data
    }

    /// Hash a data payload.
    pub fn hash_payload(data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Create a directional report from transfer measurements.
    pub fn make_report(
        is_upload: bool,
        transfer_time_ms: u64,
        data_hash: [u8; 32],
        data_size_kb: u32,
    ) -> DirectionalReport {
        let throughput_kbps = if transfer_time_ms == 0 {
            data_size_kb as u64 * 1000 // Instantaneous
        } else {
            (data_size_kb as u64 * 1000) / transfer_time_ms
        };

        DirectionalReport {
            is_upload,
            transfer_time_ms,
            data_hash,
            throughput_kbps,
        }
    }

    /// Evaluate a bidirectional test from two directional reports.
    pub fn evaluate(
        &self,
        upload: DirectionalReport,
        download: DirectionalReport,
    ) -> BidirectionalResult {
        let upload_passed = upload.throughput_kbps >= MIN_THROUGHPUT_KBPS;
        let download_passed = download.throughput_kbps >= MIN_THROUGHPUT_KBPS;
        let passed = upload_passed && download_passed;

        // Score: average of both directions, normalized to reference throughput.
        let upload_score = (upload.throughput_kbps as f64 / REFERENCE_THROUGHPUT_KBPS as f64 * 100.0)
            .clamp(0.0, 100.0);
        let download_score = (download.throughput_kbps as f64 / REFERENCE_THROUGHPUT_KBPS as f64 * 100.0)
            .clamp(0.0, 100.0);

        // Use minimum of both directions (bottleneck determines score).
        let combined_score = upload_score.min(download_score) as u32;

        BidirectionalResult {
            test_id: self.test_id,
            upload_report: upload,
            download_report: download,
            combined_score,
            passed,
        }
    }

    /// Verify that both directions used the correct data payloads.
    pub fn verify_payloads(
        &self,
        seed: &[u8; 32],
        upload_hash: &[u8; 32],
        download_hash: &[u8; 32],
    ) -> bool {
        let expected_upload = Self::hash_payload(
            &Self::generate_payload(seed, true, self.data_size_kb),
        );
        let expected_download = Self::hash_payload(
            &Self::generate_payload(seed, false, self.data_size_kb),
        );

        *upload_hash == expected_upload && *download_hash == expected_download
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
    fn item_146_create_bidirectional_test() {
        let seed = [42u8; 32];
        let bw = BidirectionalBandwidth::new(test_addr(1), test_addr(2), 100, 0, &seed);
        assert_eq!(bw.initiator, test_addr(1));
        assert_eq!(bw.responder, test_addr(2));
        assert_eq!(bw.data_size_kb, 100);
    }

    #[test]
    fn item_146_generate_different_payloads_per_direction() {
        let seed = [42u8; 32];
        let upload = BidirectionalBandwidth::generate_payload(&seed, true, 1);
        let download = BidirectionalBandwidth::generate_payload(&seed, false, 1);
        assert_ne!(upload, download);
    }

    #[test]
    fn item_146_evaluate_passing() {
        let seed = [42u8; 32];
        let bw = BidirectionalBandwidth::new(test_addr(1), test_addr(2), 100, 0, &seed);

        let upload = BidirectionalBandwidth::make_report(true, 10, [1u8; 32], 100);
        let download = BidirectionalBandwidth::make_report(false, 10, [2u8; 32], 100);

        let result = bw.evaluate(upload, download);
        assert!(result.passed);
        assert!(result.combined_score > 0);
    }

    #[test]
    fn item_146_evaluate_failing_slow() {
        let seed = [42u8; 32];
        let bw = BidirectionalBandwidth::new(test_addr(1), test_addr(2), 1, 0, &seed);

        // Very slow: 1 KB in 100 seconds = 0.01 KB/s < MIN_THROUGHPUT_KBPS
        let upload = BidirectionalBandwidth::make_report(true, 100_000, [1u8; 32], 1);
        let download = BidirectionalBandwidth::make_report(false, 1, [2u8; 32], 1);

        let result = bw.evaluate(upload, download);
        assert!(!result.passed);
    }

    #[test]
    fn item_146_verify_payloads() {
        let seed = [42u8; 32];
        let bw = BidirectionalBandwidth::new(test_addr(1), test_addr(2), 1, 0, &seed);

        let upload_data = BidirectionalBandwidth::generate_payload(&seed, true, 1);
        let download_data = BidirectionalBandwidth::generate_payload(&seed, false, 1);

        let upload_hash = BidirectionalBandwidth::hash_payload(&upload_data);
        let download_hash = BidirectionalBandwidth::hash_payload(&download_data);

        assert!(bw.verify_payloads(&seed, &upload_hash, &download_hash));
    }

    #[test]
    fn item_146_deterministic_test_id() {
        let seed = [42u8; 32];
        let bw1 = BidirectionalBandwidth::new(test_addr(1), test_addr(2), 100, 0, &seed);
        let bw2 = BidirectionalBandwidth::new(test_addr(1), test_addr(2), 100, 0, &seed);
        assert_eq!(bw1.test_id, bw2.test_id);
    }
}
