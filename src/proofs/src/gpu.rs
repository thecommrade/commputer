use commputer_core::proof::{ProofChallenge, ProofResponse, ResourceChannel};
use commputer_core::identity::Address;
use sha2::{Sha256, Digest};
use std::time::Instant;

/// GPU Proof of Processing prover.
///
/// The GPU challenge is deterministic matrix multiplication.
/// Two 64x64 matrices are generated from the challenge seed,
/// multiplied together, and the result is hashed. Verifiable
/// by recomputing with the same seed.
pub struct GpuProver;

const MATRIX_DIM: usize = 64;

impl GpuProver {
    /// Solve a GPU proof challenge.
    pub fn solve(challenge: &ProofChallenge, validator: Address) -> ProofResponse {
        assert_eq!(challenge.channel, ResourceChannel::Gpu);

        let start = Instant::now();

        // Payload layout: [0x02 type marker, then 32-byte seed]
        let seed = &challenge.payload[1..];

        let result = Self::matrix_hash(seed);

        let elapsed = start.elapsed();

        ProofResponse {
            challenge_id: challenge.challenge_id,
            validator,
            result: result.to_vec(),
            compute_time_ms: elapsed.as_millis() as u64,
            signature: vec![],
        }
    }

    /// Verify a GPU proof by recomputing the matrix multiply + hash.
    pub fn verify(challenge: &ProofChallenge, response: &ProofResponse) -> bool {
        let seed = &challenge.payload[1..];
        let expected = Self::matrix_hash(seed);
        expected[..] == response.result[..]
    }

    /// Core computation: generate two 64x64 matrices from seed, multiply,
    /// then SHA-256 the result.
    pub fn matrix_hash(seed: &[u8]) -> [u8; 32] {
        let a = Self::generate_matrix(seed, 0);
        let b = Self::generate_matrix(seed, 1);
        let c = Self::multiply(&a, &b);

        let mut hasher = Sha256::new();
        for row in &c {
            for &val in row {
                hasher.update(val.to_le_bytes());
            }
        }
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Generate a deterministic MATRIX_DIM x MATRIX_DIM matrix from seed + index.
    fn generate_matrix(seed: &[u8], index: u8) -> Vec<Vec<u64>> {
        let mut matrix = vec![vec![0u64; MATRIX_DIM]; MATRIX_DIM];
        for row in 0..MATRIX_DIM {
            // Derive a row seed by hashing seed + index + row number.
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update([index, row as u8]);
            let row_seed = hasher.finalize();

            for col in 0..MATRIX_DIM {
                // Use 8-byte windows from the row seed, cycling as needed.
                let byte_offset = (col * 8) % 24; // 24 usable bytes in 32-byte hash
                let val = u64::from_le_bytes(
                    row_seed[byte_offset..byte_offset + 8].try_into().unwrap()
                );
                matrix[row][col] = val;
            }
        }
        matrix
    }

    /// Multiply two MATRIX_DIM x MATRIX_DIM matrices using u64 wrapping arithmetic.
    fn multiply(a: &[Vec<u64>], b: &[Vec<u64>]) -> Vec<Vec<u64>> {
        let n = MATRIX_DIM;
        let mut c = vec![vec![0u64; n]; n];
        for i in 0..n {
            for k in 0..n {
                let a_ik = a[i][k];
                if a_ik == 0 {
                    continue;
                }
                for j in 0..n {
                    c[i][j] = c[i][j].wrapping_add(a_ik.wrapping_mul(b[k][j]));
                }
            }
        }
        c
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

    fn make_gpu_challenge() -> ProofChallenge {
        let mut payload = vec![0x02]; // GPU type marker
        payload.extend_from_slice(&[42u8; 32]); // seed
        ProofChallenge {
            channel: ResourceChannel::Gpu,
            challenge_id: [2u8; 32],
            epoch: 0,
            target: test_addr(),
            payload,
            deadline_block: 100,
        }
    }

    #[test]
    fn solve_and_verify_gpu() {
        let challenge = make_gpu_challenge();
        let response = GpuProver::solve(&challenge, test_addr());
        assert!(GpuProver::verify(&challenge, &response));
    }

    #[test]
    fn wrong_gpu_result_fails() {
        let challenge = make_gpu_challenge();
        let mut response = GpuProver::solve(&challenge, test_addr());
        response.result[0] ^= 0xFF;
        assert!(!GpuProver::verify(&challenge, &response));
    }

    #[test]
    fn gpu_result_is_deterministic() {
        let challenge = make_gpu_challenge();
        let r1 = GpuProver::solve(&challenge, test_addr());
        let r2 = GpuProver::solve(&challenge, test_addr());
        assert_eq!(r1.result, r2.result);
    }

    #[test]
    fn gpu_result_is_32_bytes() {
        let challenge = make_gpu_challenge();
        let response = GpuProver::solve(&challenge, test_addr());
        assert_eq!(response.result.len(), 32);
    }
}
