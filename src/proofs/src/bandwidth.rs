use commputer_core::proof::{ProofChallenge, ProofResponse, ResourceChannel};
use commputer_core::identity::Address;
use sha2::{Sha256, Digest};
use std::time::Instant;

/// Bandwidth Proof prover.
///
/// At launch this is a timed data generation + hashing benchmark.
/// The challenge specifies a payload size in KB. The prover generates
/// that many KB of data from the seed, hashes it, and reports timing.
/// The verifier recomputes the hash to confirm correctness.
pub struct BandwidthProver;

impl BandwidthProver {
    /// Solve a bandwidth proof challenge.
    pub fn solve(challenge: &ProofChallenge, validator: Address) -> ProofResponse {
        assert_eq!(challenge.channel, ResourceChannel::Bandwidth);

        let start = Instant::now();

        // Payload layout: [4-byte data_size_kb (little-endian), then 32-byte seed]
        let size_kb = u32::from_le_bytes(
            challenge.payload[..4].try_into().unwrap()
        );
        let seed = &challenge.payload[4..];

        let result = Self::bandwidth_hash(seed, size_kb as usize);

        let elapsed = start.elapsed();

        ProofResponse {
            challenge_id: challenge.challenge_id,
            validator,
            result: result.to_vec(),
            compute_time_ms: elapsed.as_millis() as u64,
            signature: vec![],
        }
    }

    /// Verify a bandwidth proof by recomputing the hash.
    /// Also checks timing — excessively slow responses indicate poor bandwidth.
    pub fn verify(challenge: &ProofChallenge, response: &ProofResponse) -> bool {
        let size_kb = u32::from_le_bytes(
            challenge.payload[..4].try_into().unwrap()
        );
        let seed = &challenge.payload[4..];

        let expected = Self::bandwidth_hash(seed, size_kb as usize);
        if expected[..] != response.result[..] {
            return false;
        }

        // Timing check: processing 1MB of data shouldn't take > 5 seconds.
        let max_ms = (size_kb as u64 / 1024 + 1) * 5000;
        if response.compute_time_ms > max_ms {
            return false; // Too slow — possible throttling
        }

        true
    }

    /// Estimate bandwidth score (0-100) from compute time.
    /// Lower time = higher score.
    pub fn score_from_timing(size_kb: u32, compute_time_ms: u64) -> u32 {
        if compute_time_ms == 0 {
            return 100;
        }
        // Expected: ~1ms per MB for fast hardware.
        let expected_ms = (size_kb as u64 / 1024).max(1);
        let ratio = expected_ms as f64 / compute_time_ms as f64;
        (ratio * 100.0).clamp(0.0, 100.0) as u32
    }

    /// Core computation: generate `size_kb` kilobytes of data from seed, hash it.
    pub fn bandwidth_hash(seed: &[u8], size_kb: usize) -> [u8; 32] {
        let size_bytes = size_kb * 1024;
        let mut outer_hasher = Sha256::new();

        // Stream the generated payload through the hasher in 32-byte blocks
        // to avoid needing to hold it all in memory at once.
        let mut counter = 0u64;
        let mut produced = 0usize;
        while produced < size_bytes {
            let mut block_hasher = Sha256::new();
            block_hasher.update(seed);
            block_hasher.update(b"bw_payload");
            block_hasher.update(counter.to_le_bytes());
            let block = block_hasher.finalize();
            let remaining = size_bytes - produced;
            let to_use = remaining.min(32);
            outer_hasher.update(&block[..to_use]);
            produced += to_use;
            counter += 1;
        }

        let result = outer_hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr() -> Address {
        let mut a = [0u8; 32];
        a[0] = 1;
        Address(a)
    }

    fn make_bw_challenge() -> ProofChallenge {
        // Use 1 KB for tests so it runs fast.
        let mut payload = 1u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&[42u8; 32]);
        ProofChallenge {
            channel: ResourceChannel::Bandwidth,
            challenge_id: [5u8; 32],
            epoch: 0,
            target: test_addr(),
            payload,
            deadline_block: 100,
        }
    }

    #[test]
    fn solve_and_verify_bandwidth() {
        let challenge = make_bw_challenge();
        let response = BandwidthProver::solve(&challenge, test_addr());
        assert!(BandwidthProver::verify(&challenge, &response));
    }

    #[test]
    fn bandwidth_reports_timing() {
        let challenge = make_bw_challenge();
        let response = BandwidthProver::solve(&challenge, test_addr());
        assert!(response.compute_time_ms < 10_000);
    }

    #[test]
    fn bandwidth_result_is_deterministic() {
        let challenge = make_bw_challenge();
        let r1 = BandwidthProver::solve(&challenge, test_addr());
        let r2 = BandwidthProver::solve(&challenge, test_addr());
        assert_eq!(r1.result, r2.result);
    }

    #[test]
    fn wrong_bandwidth_result_fails() {
        let challenge = make_bw_challenge();
        let mut response = BandwidthProver::solve(&challenge, test_addr());
        response.result[0] ^= 0xFF;
        assert!(!BandwidthProver::verify(&challenge, &response));
    }
}
