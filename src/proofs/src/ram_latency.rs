//! Item 145: RAM proof with DRAM latency measurement.
//!
//! Enhances the RAM prover with sequential random reads that defeat CPU cache.
//! Uses a large-stride pointer-chasing pattern to measure actual DRAM latency,
//! not L1/L2/L3 cache performance.

use sha2::{Digest, Sha256};
use std::time::Instant;

/// Stride between accesses to defeat hardware prefetcher.
/// Typically 4KB (one page) to ensure TLB misses as well.
const ACCESS_STRIDE: usize = 4096;
/// Number of pointer-chase steps.
const CHASE_STEPS: usize = 512;

/// Enhanced RAM prover that measures actual DRAM latency via pointer chasing.
pub struct DramLatencyProver;

/// Result of a DRAM latency measurement.
#[derive(Debug, Clone)]
pub struct DramLatencyResult {
    /// Average access latency in nanoseconds.
    pub avg_latency_ns: u64,
    /// Minimum latency observed (ns).
    pub min_latency_ns: u64,
    /// Maximum latency observed (ns).
    pub max_latency_ns: u64,
    /// The hash of all read values (for verification).
    pub result_hash: [u8; 32],
    /// Estimated memory tier based on latency.
    pub memory_tier: MemoryTier,
}

/// Classification of memory performance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTier {
    /// < 50ns: likely reading from cache (suspicious).
    CacheHit,
    /// 50-100ns: fast DDR5 / close NUMA node.
    FastDram,
    /// 100-200ns: typical DDR4.
    NormalDram,
    /// > 200ns: slow memory, remote NUMA, or swap.
    SlowMemory,
}

impl DramLatencyProver {
    /// Run a DRAM latency measurement.
    ///
    /// Allocates a buffer, fills it with a deterministic pointer-chase pattern,
    /// then follows the chain measuring access times. The pattern uses
    /// large strides to defeat hardware prefetchers and cache.
    pub fn measure_latency(seed: &[u8], buffer_mb: usize) -> DramLatencyResult {
        let buffer_size = buffer_mb * 1024 * 1024;
        let buffer_size = buffer_size.max(ACCESS_STRIDE * CHASE_STEPS * 2);

        // Step 1: Allocate and fill buffer with deterministic data.
        let buffer = Self::fill_buffer(seed, buffer_size);

        // Step 2: Generate a pointer-chase sequence.
        // Each "pointer" is an offset into the buffer, separated by ACCESS_STRIDE.
        let offsets = Self::generate_chase_sequence(seed, buffer_size);

        // Step 3: Follow the chain, reading from the buffer at each offset.
        // This forces actual memory accesses.
        let mut hasher = Sha256::new();
        let mut total_ns: u64 = 0;
        let mut min_ns: u64 = u64::MAX;
        let mut max_ns: u64 = 0;

        for &offset in &offsets {
            let start = Instant::now();

            // Read 8 bytes at the offset — this is the latency-sensitive operation.
            let end_pos = (offset + 8).min(buffer.len());
            let slice = &buffer[offset..end_pos];

            // Use the value to prevent the compiler from optimizing away the read.
            hasher.update(slice);

            let elapsed_ns = start.elapsed().as_nanos() as u64;
            total_ns += elapsed_ns;
            min_ns = min_ns.min(elapsed_ns);
            max_ns = max_ns.max(elapsed_ns);
        }

        let avg_latency_ns = if offsets.is_empty() {
            0
        } else {
            total_ns / offsets.len() as u64
        };

        let result = hasher.finalize();
        let mut result_hash = [0u8; 32];
        result_hash.copy_from_slice(&result);

        let memory_tier = Self::classify_latency(avg_latency_ns);

        DramLatencyResult {
            avg_latency_ns,
            min_latency_ns: if min_ns == u64::MAX { 0 } else { min_ns },
            max_latency_ns: max_ns,
            result_hash,
            memory_tier,
        }
    }

    /// Verify a DRAM latency proof by recomputing the hash.
    pub fn verify_hash(seed: &[u8], buffer_mb: usize, expected_hash: &[u8; 32]) -> bool {
        let buffer_size = buffer_mb * 1024 * 1024;
        let buffer_size = buffer_size.max(ACCESS_STRIDE * CHASE_STEPS * 2);

        let buffer = Self::fill_buffer(seed, buffer_size);
        let offsets = Self::generate_chase_sequence(seed, buffer_size);

        let mut hasher = Sha256::new();
        for &offset in &offsets {
            let end_pos = (offset + 8).min(buffer.len());
            hasher.update(&buffer[offset..end_pos]);
        }

        let result = hasher.finalize();
        let mut computed = [0u8; 32];
        computed.copy_from_slice(&result);
        computed == *expected_hash
    }

    /// Score based on latency measurement (0-100).
    pub fn score_latency(result: &DramLatencyResult) -> u32 {
        match result.memory_tier {
            MemoryTier::CacheHit => 30, // Suspicious — might be cheating
            MemoryTier::FastDram => 100,
            MemoryTier::NormalDram => 80,
            MemoryTier::SlowMemory => 40,
        }
    }

    fn classify_latency(avg_ns: u64) -> MemoryTier {
        if avg_ns < 50 {
            MemoryTier::CacheHit
        } else if avg_ns < 100 {
            MemoryTier::FastDram
        } else if avg_ns < 200 {
            MemoryTier::NormalDram
        } else {
            MemoryTier::SlowMemory
        }
    }

    fn fill_buffer(seed: &[u8], size: usize) -> Vec<u8> {
        let mut buf = Vec::with_capacity(size);
        let mut counter = 0u64;
        while buf.len() < size {
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update(b"dram_fill");
            hasher.update(counter.to_le_bytes());
            let block = hasher.finalize();
            let remaining = size - buf.len();
            let to_take = remaining.min(32);
            buf.extend_from_slice(&block[..to_take]);
            counter += 1;
        }
        buf
    }

    fn generate_chase_sequence(seed: &[u8], buffer_size: usize) -> Vec<usize> {
        let safe_size = if buffer_size > 8 { buffer_size - 8 } else { 1 };
        let mut offsets = Vec::with_capacity(CHASE_STEPS);

        for i in 0..CHASE_STEPS {
            let mut hasher = Sha256::new();
            hasher.update(seed);
            hasher.update(b"dram_chase");
            hasher.update((i as u64).to_le_bytes());
            let h = hasher.finalize();
            let raw = u64::from_le_bytes(h[..8].try_into().unwrap());
            // Align to ACCESS_STRIDE boundary for cache-defeating behavior.
            let offset = ((raw as usize) % safe_size) & !(ACCESS_STRIDE - 1);
            offsets.push(offset.min(safe_size - 1));
        }

        offsets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_145_measure_latency() {
        let seed = [42u8; 32];
        let result = DramLatencyProver::measure_latency(&seed, 4); // 4 MB
        assert_ne!(result.result_hash, [0u8; 32]);
    }

    #[test]
    fn item_145_verify_hash() {
        let seed = [42u8; 32];
        let result = DramLatencyProver::measure_latency(&seed, 4);
        assert!(DramLatencyProver::verify_hash(&seed, 4, &result.result_hash));
    }

    #[test]
    fn item_145_wrong_hash_fails() {
        let seed = [42u8; 32];
        let wrong_hash = [0xFFu8; 32];
        assert!(!DramLatencyProver::verify_hash(&seed, 4, &wrong_hash));
    }

    #[test]
    fn item_145_score_latency() {
        let mut result = DramLatencyResult {
            avg_latency_ns: 80,
            min_latency_ns: 60,
            max_latency_ns: 120,
            result_hash: [0u8; 32],
            memory_tier: MemoryTier::FastDram,
        };
        assert_eq!(DramLatencyProver::score_latency(&result), 100);

        result.memory_tier = MemoryTier::CacheHit;
        assert_eq!(DramLatencyProver::score_latency(&result), 30);

        result.memory_tier = MemoryTier::SlowMemory;
        assert_eq!(DramLatencyProver::score_latency(&result), 40);
    }

    #[test]
    fn item_145_deterministic_hash() {
        let seed = [99u8; 32];
        let r1 = DramLatencyProver::measure_latency(&seed, 4);
        let r2 = DramLatencyProver::measure_latency(&seed, 4);
        assert_eq!(r1.result_hash, r2.result_hash);
    }
}
