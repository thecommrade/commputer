//! Item 154: Hardware benchmark on startup.
//!
//! Runs a standard benchmark suite when the node starts. Results are
//! cached and used for proof difficulty calibration.

use sha2::{Digest, Sha256};
use std::time::Instant;

/// Results of the hardware benchmark suite.
#[derive(Debug, Clone)]
pub struct HardwareBenchmark {
    /// CPU: SHA-256 hashes per second.
    pub cpu_hashes_per_sec: u64,
    /// Memory: MB/s for sequential read.
    pub memory_bandwidth_mbps: u64,
    /// Memory: estimated latency in nanoseconds.
    pub memory_latency_ns: u64,
    /// Storage: estimated sequential read speed in MB/s (simulated).
    pub storage_read_mbps: u64,
    /// Total benchmark time in milliseconds.
    pub benchmark_time_ms: u64,
    /// Composite performance score (0-1000).
    pub composite_score: u32,
}

/// Number of SHA-256 iterations for CPU benchmark.
const CPU_BENCH_ITERATIONS: u32 = 100_000;
/// Buffer size for memory benchmark (4 MB).
const MEM_BENCH_SIZE: usize = 4 * 1024 * 1024;
/// Number of random reads for memory latency benchmark.
const MEM_LATENCY_READS: usize = 1024;

impl HardwareBenchmark {
    /// Run the full benchmark suite.
    pub fn run() -> Self {
        let total_start = Instant::now();

        let cpu_hashes_per_sec = Self::bench_cpu();
        let (memory_bandwidth_mbps, memory_latency_ns) = Self::bench_memory();
        let storage_read_mbps = Self::bench_storage_sim();

        let benchmark_time_ms = total_start.elapsed().as_millis() as u64;

        let composite_score = Self::compute_composite(
            cpu_hashes_per_sec,
            memory_bandwidth_mbps,
            memory_latency_ns,
            storage_read_mbps,
        );

        Self {
            cpu_hashes_per_sec,
            memory_bandwidth_mbps,
            memory_latency_ns,
            storage_read_mbps,
            benchmark_time_ms,
            composite_score,
        }
    }

    /// CPU benchmark: iterative SHA-256 hashing.
    fn bench_cpu() -> u64 {
        let seed = [42u8; 32];
        let start = Instant::now();

        let mut current = Sha256::digest(seed);
        for _ in 1..CPU_BENCH_ITERATIONS {
            current = Sha256::digest(current);
        }

        // Prevent optimization.
        std::hint::black_box(&current);

        let elapsed_ms = start.elapsed().as_millis() as u64;
        if elapsed_ms == 0 {
            CPU_BENCH_ITERATIONS as u64 * 1000
        } else {
            (CPU_BENCH_ITERATIONS as u64 * 1000) / elapsed_ms
        }
    }

    /// Memory benchmark: fill and read buffer.
    fn bench_memory() -> (u64, u64) {
        // Bandwidth: sequential fill + read
        let start = Instant::now();
        let mut buffer = vec![0u8; MEM_BENCH_SIZE];
        for (i, byte) in buffer.iter_mut().enumerate() {
            *byte = (i & 0xFF) as u8;
        }

        // Read back to prevent optimization.
        let mut sum: u64 = 0;
        for &byte in &buffer {
            sum = sum.wrapping_add(byte as u64);
        }
        std::hint::black_box(sum);

        let elapsed_us = start.elapsed().as_micros() as u64;
        let bandwidth_mbps = if elapsed_us == 0 {
            10_000
        } else {
            (MEM_BENCH_SIZE as u64 * 1_000_000) / (elapsed_us * 1024 * 1024)
        };

        // Latency: random reads
        let latency_start = Instant::now();
        let mut hasher = Sha256::new();
        for i in 0..MEM_LATENCY_READS {
            let offset = (i * 4099) % (MEM_BENCH_SIZE - 8); // Prime stride
            hasher.update(&buffer[offset..offset + 8]);
        }
        std::hint::black_box(hasher.finalize());

        let latency_elapsed_ns = latency_start.elapsed().as_nanos() as u64;
        let avg_latency_ns = latency_elapsed_ns / MEM_LATENCY_READS as u64;

        (bandwidth_mbps, avg_latency_ns)
    }

    /// Storage benchmark: simulated sequential read (actually memory).
    fn bench_storage_sim() -> u64 {
        let size = 1024 * 1024; // 1 MB
        let start = Instant::now();

        let mut hasher = Sha256::new();
        let mut counter = 0u64;
        let mut produced = 0usize;
        while produced < size {
            let mut block_hasher = Sha256::new();
            block_hasher.update(b"storage_bench");
            block_hasher.update(counter.to_le_bytes());
            let block = block_hasher.finalize();
            hasher.update(&block);
            produced += 32;
            counter += 1;
        }
        std::hint::black_box(hasher.finalize());

        let elapsed_us = start.elapsed().as_micros() as u64;
        if elapsed_us == 0 {
            5_000
        } else {
            (size as u64 * 1_000_000) / (elapsed_us * 1024 * 1024)
        }
    }

    /// Compute a composite score from individual benchmarks.
    fn compute_composite(
        cpu_hps: u64,
        mem_bw: u64,
        mem_lat: u64,
        storage: u64,
    ) -> u32 {
        // Weighted scoring:
        // CPU: 40%, Memory bandwidth: 20%, Memory latency: 20%, Storage: 20%
        let cpu_score = (cpu_hps as f64 / 1_000_000.0 * 400.0).clamp(0.0, 400.0);
        let mem_bw_score = (mem_bw as f64 / 10_000.0 * 200.0).clamp(0.0, 200.0);
        let mem_lat_score = if mem_lat == 0 {
            200.0
        } else {
            (100.0 / mem_lat as f64 * 200.0).clamp(0.0, 200.0)
        };
        let storage_score = (storage as f64 / 1_000.0 * 200.0).clamp(0.0, 200.0);

        (cpu_score + mem_bw_score + mem_lat_score + storage_score).round() as u32
    }

    /// Get suggested difficulty multipliers based on benchmark results.
    pub fn suggested_difficulties(&self) -> std::collections::HashMap<&'static str, f64> {
        let mut map = std::collections::HashMap::new();

        // Scale difficulties relative to baseline performance.
        let cpu_factor = (self.cpu_hashes_per_sec as f64 / 500_000.0).clamp(0.1, 10.0);
        let mem_factor = (self.memory_bandwidth_mbps as f64 / 5_000.0).clamp(0.1, 10.0);
        let storage_factor = (self.storage_read_mbps as f64 / 500.0).clamp(0.1, 10.0);

        map.insert("Processing", cpu_factor);
        map.insert("Gpu", (cpu_factor * 0.8).clamp(0.1, 10.0)); // Slightly less if no GPU
        map.insert("Ram", mem_factor);
        map.insert("Storage", storage_factor);
        map.insert("Bandwidth", 1.0); // Network-dependent, not locally measurable

        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_154_benchmark_runs() {
        let bench = HardwareBenchmark::run();
        assert!(bench.cpu_hashes_per_sec > 0);
        assert!(bench.benchmark_time_ms > 0 || bench.composite_score > 0);
    }

    #[test]
    fn item_154_composite_score_nonzero() {
        let bench = HardwareBenchmark::run();
        assert!(bench.composite_score > 0, "composite score should be > 0");
    }

    #[test]
    fn item_154_suggested_difficulties() {
        let bench = HardwareBenchmark::run();
        let diffs = bench.suggested_difficulties();
        assert!(diffs.contains_key("Processing"));
        assert!(diffs.contains_key("Gpu"));
        assert!(diffs.contains_key("Ram"));
        assert!(diffs.contains_key("Storage"));
        assert!(diffs.contains_key("Bandwidth"));

        for (_, &v) in &diffs {
            assert!(v >= 0.1 && v <= 10.0);
        }
    }

    #[test]
    fn item_154_composite_score_bounded() {
        // Directly test compute_composite with extreme values.
        let score = HardwareBenchmark::compute_composite(10_000_000, 100_000, 1, 100_000);
        assert!(score <= 1000);
    }
}
