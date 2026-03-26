//! Item 142: GPU proof fallback measurement.
//!
//! When no GPU is available, measure CPU performance of the same matrix
//! operation and score proportionally based on actual timing.

use sha2::{Digest, Sha256};
use std::time::Instant;

/// GPU fallback scoring based on actual CPU timing of matrix operations.
pub struct GpuFallbackScorer;

/// Result of a GPU fallback benchmark.
#[derive(Debug, Clone)]
pub struct FallbackBenchmark {
    /// Time in microseconds to perform the matrix multiply.
    pub matrix_mul_us: u64,
    /// Time in microseconds for the SHA-256 hash of the result.
    pub hash_us: u64,
    /// Total elapsed time in microseconds.
    pub total_us: u64,
    /// Score (0-100), proportional to performance. GPU gets 100, CPU is capped at 50
    /// but scaled by how fast the CPU is relative to a baseline.
    pub score: u32,
}

const MATRIX_DIM: usize = 64;

/// Baseline expected time in microseconds for a mid-range CPU.
/// If the CPU is faster than this, score is higher (up to the cap).
const BASELINE_US: u64 = 5000;

/// Maximum score for CPU fallback (item 142 enhancement: was hard-capped at 50,
/// now scales based on timing up to this cap).
const MAX_FALLBACK_SCORE: u32 = 50;

impl GpuFallbackScorer {
    /// Run the matrix multiply benchmark and compute a timing-based score.
    pub fn benchmark(seed: &[u8]) -> FallbackBenchmark {
        // Phase 1: Matrix generation + multiply
        let mat_start = Instant::now();
        let a = Self::generate_matrix(seed, 0);
        let b = Self::generate_matrix(seed, 1);
        let c = Self::multiply(&a, &b);
        let matrix_mul_us = mat_start.elapsed().as_micros() as u64;

        // Phase 2: Hash the result
        let hash_start = Instant::now();
        let mut hasher = Sha256::new();
        for row in &c {
            for &val in row {
                hasher.update(val.to_le_bytes());
            }
        }
        let _ = hasher.finalize();
        let hash_us = hash_start.elapsed().as_micros() as u64;

        let total_us = matrix_mul_us + hash_us;

        // Score: ratio of baseline to actual time, scaled to MAX_FALLBACK_SCORE.
        // Faster CPU -> higher score (up to cap).
        let score = if total_us == 0 {
            MAX_FALLBACK_SCORE
        } else {
            let ratio = BASELINE_US as f64 / total_us as f64;
            (ratio * MAX_FALLBACK_SCORE as f64).clamp(1.0, MAX_FALLBACK_SCORE as f64) as u32
        };

        FallbackBenchmark {
            matrix_mul_us,
            hash_us,
            total_us,
            score,
        }
    }

    /// Score a GPU proof response that used CPU fallback, based on its compute_time_ms.
    pub fn score_from_timing(compute_time_ms: u64) -> u32 {
        if compute_time_ms == 0 {
            return MAX_FALLBACK_SCORE;
        }
        let baseline_ms = BASELINE_US / 1000;
        let ratio = baseline_ms.max(1) as f64 / compute_time_ms as f64;
        (ratio * MAX_FALLBACK_SCORE as f64).clamp(1.0, MAX_FALLBACK_SCORE as f64) as u32
    }

    fn generate_matrix(seed: &[u8], index: u8) -> Vec<Vec<u64>> {
        let mut matrix = vec![vec![0u64; MATRIX_DIM]; MATRIX_DIM];
        for row in 0..MATRIX_DIM {
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update([index, row as u8]);
            let row_seed = hasher.finalize();
            for col in 0..MATRIX_DIM {
                let byte_offset = (col * 8) % 24;
                let val = u64::from_le_bytes(
                    row_seed[byte_offset..byte_offset + 8].try_into().unwrap(),
                );
                matrix[row][col] = val;
            }
        }
        matrix
    }

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

    #[test]
    fn item_142_benchmark_runs() {
        let seed = [42u8; 32];
        let result = GpuFallbackScorer::benchmark(&seed);
        assert!(result.score > 0);
        assert!(result.score <= 50);
        assert!(result.total_us > 0 || result.score == 50);
    }

    #[test]
    fn item_142_score_from_timing() {
        // Very fast: should get max score
        assert_eq!(GpuFallbackScorer::score_from_timing(0), 50);
        // Reasonable time
        let score = GpuFallbackScorer::score_from_timing(5);
        assert!(score > 0);
        assert!(score <= 50);
        // Very slow: should get minimum
        let slow = GpuFallbackScorer::score_from_timing(100_000);
        assert!(slow >= 1);
    }

    #[test]
    fn item_142_benchmark_is_deterministic_score_range() {
        let seed = [99u8; 32];
        let r1 = GpuFallbackScorer::benchmark(&seed);
        let r2 = GpuFallbackScorer::benchmark(&seed);
        // Both scores should be valid (within allowed range).
        // Timing-based scores can vary run-to-run, so we just check validity.
        assert!(r1.score > 0 && r1.score <= 50);
        assert!(r2.score > 0 && r2.score <= 50);
    }
}
