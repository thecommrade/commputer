use commputer_core::proof::{ProofChallenge, ProofResponse, ResourceChannel};
use commputer_core::identity::Address;
use sha2::{Sha256, Digest};
use std::time::Instant;

/// CPU Proof of Processing prover.
///
/// The CPU challenge is an iterative SHA-256 hashing puzzle.
/// The validator must perform N rounds of hashing, where N is
/// encoded in the challenge payload. The result is deterministic
/// given the input, so verifiers can spot-check by recomputing.
pub struct CpuProver;

impl CpuProver {
    /// Solve a CPU proof challenge.
    /// Returns the proof response with the computed result and timing.
    pub fn solve(challenge: &ProofChallenge, validator: Address) -> ProofResponse {
        assert_eq!(challenge.channel, ResourceChannel::Processing);

        let start = Instant::now();

        // Parse iterations from payload (first 4 bytes).
        let iterations = u32::from_le_bytes(
            challenge.payload[..4].try_into().unwrap()
        );
        let seed = &challenge.payload[4..];

        // Iterative hashing: hash(hash(hash(... seed ...)))
        let result = Self::iterative_hash(seed, iterations);

        let elapsed = start.elapsed();

        ProofResponse {
            challenge_id: challenge.challenge_id,
            validator,
            result: result.to_vec(),
            compute_time_ms: elapsed.as_millis() as u64,
            signature: vec![], // Filled by the signing layer.
        }
    }

    /// Core computation: iterative SHA-256 hashing.
    /// This is the "work" that proves CPU capability.
    pub fn iterative_hash(seed: &[u8], iterations: u32) -> [u8; 32] {
        let mut current = Sha256::digest(seed);
        for _ in 1..iterations {
            current = Sha256::digest(current);
        }
        let mut result = [0u8; 32];
        result.copy_from_slice(&current);
        result
    }

    /// Verify a CPU proof result by recomputing.
    /// Verifiers can do a full recompute (expensive) or spot-check
    /// by computing to a random intermediate point.
    pub fn verify_full(challenge: &ProofChallenge, response: &ProofResponse) -> bool {
        let iterations = u32::from_le_bytes(
            challenge.payload[..4].try_into().unwrap()
        );
        let seed = &challenge.payload[4..];

        let expected = Self::iterative_hash(seed, iterations);
        expected[..] == response.result[..]
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

    fn make_cpu_challenge(iterations: u32) -> ProofChallenge {
        let mut payload = iterations.to_le_bytes().to_vec();
        payload.extend_from_slice(&[42u8; 32]); // Seed
        ProofChallenge {
            channel: ResourceChannel::Processing,
            challenge_id: [1u8; 32],
            epoch: 0,
            target: test_addr(1),
            payload,
            deadline_block: 100,
        }
    }

    #[test]
    fn solve_and_verify() {
        let challenge = make_cpu_challenge(100);
        let response = CpuProver::solve(&challenge, test_addr(1));

        assert!(CpuProver::verify_full(&challenge, &response));
        assert_eq!(response.result.len(), 32);
    }

    #[test]
    fn deterministic_results() {
        let challenge = make_cpu_challenge(100);
        let r1 = CpuProver::solve(&challenge, test_addr(1));
        let r2 = CpuProver::solve(&challenge, test_addr(1));

        assert_eq!(r1.result, r2.result);
    }

    #[test]
    fn wrong_result_fails_verification() {
        let challenge = make_cpu_challenge(100);
        let mut response = CpuProver::solve(&challenge, test_addr(1));
        response.result[0] ^= 0xFF; // Corrupt the result.

        assert!(!CpuProver::verify_full(&challenge, &response));
    }

    #[test]
    fn reports_timing() {
        let challenge = make_cpu_challenge(1000);
        let response = CpuProver::solve(&challenge, test_addr(1));
        // Should have a non-zero compute time.
        // (May be 0 on very fast machines with only 1000 iterations.)
        assert!(response.compute_time_ms < 10_000); // Sanity: under 10s
    }
}
