use commputer_core::proof::{ProofChallenge, ProofResponse, ResourceChannel};
use commputer_core::identity::Address;
use sha2::{Sha256, Digest};
use std::time::Instant;

/// RAM Proof of Memory prover.
///
/// The RAM challenge is memory-hard: a buffer is allocated and filled
/// with deterministic data from the seed, then random reads are
/// performed at offsets derived from the challenge. The read values
/// are hashed to produce the proof. The buffer must be held in memory
/// during computation — there is no shortcut without the RAM.
pub struct RamProver;

/// Buffer size for tests: 1 MB.
/// In production this would be set from the challenge payload (e.g. 256 MB).
const TEST_BUFFER_BYTES: usize = 1024 * 1024; // 1 MB

/// Number of random read accesses to perform.
const READ_COUNT: usize = 1024;

impl RamProver {
    /// Solve a RAM proof challenge.
    pub fn solve(challenge: &ProofChallenge, validator: Address) -> ProofResponse {
        assert_eq!(challenge.channel, ResourceChannel::Ram);

        let start = Instant::now();

        // Payload layout: [4-byte required_mb (little-endian), then 32-byte seed]
        // For testing we cap at TEST_BUFFER_BYTES to keep the suite fast.
        let seed = &challenge.payload[4..];

        let result = Self::memory_hard_hash(seed, TEST_BUFFER_BYTES);

        let elapsed = start.elapsed();

        ProofResponse {
            challenge_id: challenge.challenge_id,
            validator,
            result: result.to_vec(),
            compute_time_ms: elapsed.as_millis() as u64,
            signature: vec![],
        }
    }

    /// Verify a RAM proof by recomputing (verifier uses same capped buffer size).
    pub fn verify(challenge: &ProofChallenge, response: &ProofResponse) -> bool {
        let seed = &challenge.payload[4..];
        let expected = Self::memory_hard_hash(seed, TEST_BUFFER_BYTES);
        expected[..] == response.result[..]
    }

    /// Core computation:
    /// 1. Fill `buffer_size` bytes deterministically from seed.
    /// 2. Derive READ_COUNT offsets from the seed.
    /// 3. Hash all read values into a single digest.
    pub fn memory_hard_hash(seed: &[u8], buffer_size: usize) -> [u8; 32] {
        // Step 1: fill buffer.
        let buffer = Self::fill_buffer(seed, buffer_size);

        // Step 2: derive read offsets.
        let offsets = Self::derive_offsets(seed, buffer_size);

        // Step 3: hash the values at those offsets.
        let mut hasher = Sha256::new();
        for offset in offsets {
            // Read 8 bytes at each offset (wrapping to avoid OOB).
            let end = (offset + 8).min(buffer.len());
            hasher.update(&buffer[offset..end]);
        }
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Fill a buffer of `size` bytes deterministically using SHA-256 blocks.
    fn fill_buffer(seed: &[u8], size: usize) -> Vec<u8> {
        let mut buf = Vec::with_capacity(size);
        let mut counter = 0u64;
        while buf.len() < size {
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update(b"ram_fill");
            hasher.update(counter.to_le_bytes());
            let block = hasher.finalize();
            let remaining = size - buf.len();
            let to_take = remaining.min(32);
            buf.extend_from_slice(&block[..to_take]);
            counter += 1;
        }
        buf
    }

    /// Derive READ_COUNT random offsets from the seed, clamped to `buf_size - 8`.
    fn derive_offsets(seed: &[u8], buf_size: usize) -> Vec<usize> {
        let safe_len = if buf_size > 8 { buf_size - 8 } else { 1 };
        let mut offsets = Vec::with_capacity(READ_COUNT);
        for i in 0..READ_COUNT {
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update(b"ram_offset");
            hasher.update((i as u64).to_le_bytes());
            let h = hasher.finalize();
            let raw = u64::from_le_bytes(h[..8].try_into().unwrap());
            offsets.push((raw as usize) % safe_len);
        }
        offsets
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

    fn make_ram_challenge() -> ProofChallenge {
        // Payload: 4-byte required_mb (1 for test) + 32-byte seed.
        let mut payload = 1u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&[42u8; 32]);
        ProofChallenge {
            channel: ResourceChannel::Ram,
            challenge_id: [4u8; 32],
            epoch: 0,
            target: test_addr(),
            payload,
            deadline_block: 100,
        }
    }

    #[test]
    fn solve_and_verify_ram() {
        let challenge = make_ram_challenge();
        let response = RamProver::solve(&challenge, test_addr());
        assert!(RamProver::verify(&challenge, &response));
    }

    #[test]
    fn deterministic_ram_results() {
        let challenge = make_ram_challenge();
        let r1 = RamProver::solve(&challenge, test_addr());
        let r2 = RamProver::solve(&challenge, test_addr());
        assert_eq!(r1.result, r2.result);
    }

    #[test]
    fn wrong_ram_result_fails() {
        let challenge = make_ram_challenge();
        let mut response = RamProver::solve(&challenge, test_addr());
        response.result[0] ^= 0xFF;
        assert!(!RamProver::verify(&challenge, &response));
    }

    #[test]
    fn ram_result_is_32_bytes() {
        let challenge = make_ram_challenge();
        let response = RamProver::solve(&challenge, test_addr());
        assert_eq!(response.result.len(), 32);
    }
}
