use commputer_core::proof::{ProofChallenge, ProofResponse, ResourceChannel};
use commputer_core::identity::Address;
use sha2::{Sha256, Digest};
use std::time::Instant;

/// Storage Proof of Retrievability prover.
///
/// The challenge specifies random byte offsets into the validator's
/// stored data. The prover must hash the chunks at those offsets.
/// The verifier checks the result against the known data.
pub struct StorageProver;

/// Number of chunks to challenge per proof.
const CHUNK_COUNT: usize = 16;
/// Size of each challenged chunk in bytes.
const CHUNK_SIZE: usize = 64;

impl StorageProver {
    /// Solve a storage proof challenge.
    /// `data` is the validator's stored data blob (1MB assigned chunk).
    /// Feature 118: Supports both legacy (seed-only) and new (offset+length) payload formats.
    pub fn solve(challenge: &ProofChallenge, data: &[u8], validator: Address) -> ProofResponse {
        assert_eq!(challenge.channel, ResourceChannel::Storage);

        let start = Instant::now();

        let result = if challenge.payload.len() >= 9 + 32 {
            // Feature 118: New format — [0x03, 4-byte offset, 4-byte length, 32-byte seed]
            let offset = u32::from_le_bytes(challenge.payload[1..5].try_into().unwrap()) as usize;
            let length = u32::from_le_bytes(challenge.payload[5..9].try_into().unwrap()) as usize;
            let seed = &challenge.payload[9..];
            Self::hash_range(data, offset, length, seed)
        } else {
            // Legacy format — [0x03, 32-byte seed]
            let seed = &challenge.payload[1..];
            let offsets = Self::derive_offsets(seed, data.len());
            Self::hash_chunks(data, &offsets)
        };

        let elapsed = start.elapsed();

        ProofResponse {
            challenge_id: challenge.challenge_id,
            validator,
            result: result.to_vec(),
            compute_time_ms: elapsed.as_millis() as u64,
            signature: vec![],
        }
    }

    /// Verify a storage proof by recomputing against the real data.
    /// Feature 118: Supports both legacy and new payload formats.
    pub fn verify(challenge: &ProofChallenge, response: &ProofResponse, data: &[u8]) -> bool {
        let expected = if challenge.payload.len() >= 9 + 32 {
            let offset = u32::from_le_bytes(challenge.payload[1..5].try_into().unwrap()) as usize;
            let length = u32::from_le_bytes(challenge.payload[5..9].try_into().unwrap()) as usize;
            let seed = &challenge.payload[9..];
            Self::hash_range(data, offset, length, seed)
        } else {
            let seed = &challenge.payload[1..];
            let offsets = Self::derive_offsets(seed, data.len());
            Self::hash_chunks(data, &offsets)
        };
        expected[..] == response.result[..]
    }

    /// Feature 118: Hash a specific byte range from data, mixed with seed.
    fn hash_range(data: &[u8], offset: usize, length: usize, seed: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(seed);
        let start = offset.min(data.len());
        let end = (offset + length).min(data.len());
        hasher.update(&data[start..end]);
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Generate deterministic test data of the given size, seeded by `seed`.
    pub fn generate_test_data(seed: &[u8; 32], size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        let mut counter = 0u64;
        while data.len() < size {
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update(counter.to_le_bytes());
            let block = hasher.finalize();
            let remaining = size - data.len();
            let to_take = remaining.min(32);
            data.extend_from_slice(&block[..to_take]);
            counter += 1;
        }
        data
    }

    /// Derive CHUNK_COUNT byte offsets from the seed, clamped to `data_len`.
    fn derive_offsets(seed: &[u8], data_len: usize) -> Vec<usize> {
        let safe_len = if data_len > CHUNK_SIZE {
            data_len - CHUNK_SIZE
        } else {
            1
        };

        let mut offsets = Vec::with_capacity(CHUNK_COUNT);
        for i in 0..CHUNK_COUNT {
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update((i as u64).to_le_bytes());
            let h = hasher.finalize();
            let raw = u64::from_le_bytes(h[..8].try_into().unwrap());
            offsets.push((raw as usize) % safe_len);
        }
        offsets
    }

    /// Hash the data chunks at the given offsets into a single 32-byte digest.
    fn hash_chunks(data: &[u8], offsets: &[usize]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for &offset in offsets {
            let end = (offset + CHUNK_SIZE).min(data.len());
            hasher.update(&data[offset..end]);
        }
        let result = hasher.finalize();
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

    fn make_storage_challenge() -> ProofChallenge {
        let mut payload = vec![0x03]; // Storage type marker
        payload.extend_from_slice(&[42u8; 32]); // seed
        ProofChallenge {
            channel: ResourceChannel::Storage,
            challenge_id: [3u8; 32],
            epoch: 0,
            target: test_addr(),
            payload,
            deadline_block: 100,
        }
    }

    #[test]
    fn solve_and_verify_storage() {
        let data = StorageProver::generate_test_data(&[42u8; 32], 4096);
        let challenge = make_storage_challenge();
        let response = StorageProver::solve(&challenge, &data, test_addr());
        assert!(StorageProver::verify(&challenge, &response, &data));
    }

    #[test]
    fn wrong_data_fails_storage() {
        let data = StorageProver::generate_test_data(&[42u8; 32], 4096);
        let wrong = StorageProver::generate_test_data(&[99u8; 32], 4096);
        let challenge = make_storage_challenge();
        let response = StorageProver::solve(&challenge, &wrong, test_addr());
        assert!(!StorageProver::verify(&challenge, &response, &data));
    }

    #[test]
    fn storage_result_is_deterministic() {
        let data = StorageProver::generate_test_data(&[42u8; 32], 4096);
        let challenge = make_storage_challenge();
        let r1 = StorageProver::solve(&challenge, &data, test_addr());
        let r2 = StorageProver::solve(&challenge, &data, test_addr());
        assert_eq!(r1.result, r2.result);
    }

    #[test]
    fn storage_result_is_32_bytes() {
        let data = StorageProver::generate_test_data(&[42u8; 32], 4096);
        let challenge = make_storage_challenge();
        let response = StorageProver::solve(&challenge, &data, test_addr());
        assert_eq!(response.result.len(), 32);
    }
}
