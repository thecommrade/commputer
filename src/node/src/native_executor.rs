#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::time::Instant;

use crate::wasm_executor::ExecutionResult;

/// Configuration for the native executor with cgroup-style resource limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeExecutorConfig {
    /// Maximum memory in megabytes.
    pub max_memory_mb: u64,
    /// Maximum CPU usage as a percentage (1-100).
    pub max_cpu_percent: u8,
    /// Working directory for the native process.
    pub working_dir: String,
    /// Maximum execution time in milliseconds.
    pub max_time_ms: u64,
}

impl Default for NativeExecutorConfig {
    fn default() -> Self {
        Self {
            max_memory_mb: 512,
            max_cpu_percent: 50,
            working_dir: "/tmp/commputer-jobs".into(),
            max_time_ms: 300_000,
        }
    }
}

/// Execute a native job with resource limits.
/// Currently a stub -- real cgroup integration planned for production.
pub fn execute_native(
    config: &NativeExecutorConfig,
    command: &str,
    input: &[u8],
) -> ExecutionResult {
    let start = Instant::now();

    if command.is_empty() {
        return ExecutionResult {
            success: false,
            result_hash: [0u8; 32],
            execution_time_ms: 0,
            memory_used_bytes: 0,
            output: None,
            error: Some("Empty command".into()),
        };
    }

    if config.max_cpu_percent == 0 || config.max_cpu_percent > 100 {
        return ExecutionResult {
            success: false,
            result_hash: [0u8; 32],
            execution_time_ms: 0,
            memory_used_bytes: 0,
            output: None,
            error: Some("Invalid CPU percentage (1-100)".into()),
        };
    }

    // Stub: hash the command + input + config as the result
    let mut hasher = Sha256::new();
    hasher.update(command.as_bytes());
    hasher.update(input);
    hasher.update(config.working_dir.as_bytes());
    let result = hasher.finalize();
    let mut result_hash = [0u8; 32];
    result_hash.copy_from_slice(&result);

    let elapsed = start.elapsed();

    ExecutionResult {
        success: true,
        result_hash,
        execution_time_ms: elapsed.as_millis() as u64,
        memory_used_bytes: input.len() as u64,
        output: Some(result_hash.to_vec()),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execute_native_empty_command() {
        let config = NativeExecutorConfig::default();
        let result = execute_native(&config, "", b"data");
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_execute_native_invalid_cpu() {
        let mut config = NativeExecutorConfig::default();
        config.max_cpu_percent = 0;
        let result = execute_native(&config, "echo", b"data");
        assert!(!result.success);
    }

    #[test]
    fn test_execute_native_success() {
        let config = NativeExecutorConfig::default();
        let result = execute_native(&config, "echo hello", b"input");
        assert!(result.success);
        assert!(result.output.is_some());
    }

    #[test]
    fn test_execute_native_deterministic() {
        let config = NativeExecutorConfig::default();
        let r1 = execute_native(&config, "cmd", b"data");
        let r2 = execute_native(&config, "cmd", b"data");
        assert_eq!(r1.result_hash, r2.result_hash);
    }

    #[test]
    fn test_config_defaults() {
        let config = NativeExecutorConfig::default();
        assert_eq!(config.max_memory_mb, 512);
        assert_eq!(config.max_cpu_percent, 50);
        assert_eq!(config.max_time_ms, 300_000);
    }
}
