use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::time::Instant;

/// Result of executing a WASM job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub result_hash: [u8; 32],
    pub execution_time_ms: u64,
    pub memory_used_bytes: u64,
    pub output: Option<Vec<u8>>,
    pub error: Option<String>,
}

/// Configuration for the WASM executor sandbox.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub max_memory_bytes: u64,
    pub max_cpu_time_ms: u64,
    pub max_output_bytes: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            max_memory_bytes: 256 * 1024 * 1024, // 256 MB
            max_cpu_time_ms: 300_000,             // 5 minutes
            max_output_bytes: 10 * 1024 * 1024,   // 10 MB
        }
    }
}

/// Execute a WASM job in a sandboxed environment.
/// Currently a stub that simulates execution -- real wasmtime integration planned.
pub fn execute_wasm_job(
    wasm_bytes: &[u8],
    input: &[u8],
    _config: &SandboxConfig,
) -> ExecutionResult {
    let start = Instant::now();

    // Validate inputs
    if wasm_bytes.is_empty() {
        return ExecutionResult {
            success: false,
            result_hash: [0u8; 32],
            execution_time_ms: 0,
            memory_used_bytes: 0,
            output: None,
            error: Some("Empty WASM module".into()),
        };
    }

    // Simulate execution: hash the input as "result"
    let mut hasher = Sha256::new();
    hasher.update(wasm_bytes);
    hasher.update(input);
    let result = hasher.finalize();
    let mut result_hash = [0u8; 32];
    result_hash.copy_from_slice(&result);

    let elapsed = start.elapsed();

    ExecutionResult {
        success: true,
        result_hash,
        execution_time_ms: elapsed.as_millis() as u64,
        memory_used_bytes: (wasm_bytes.len() + input.len()) as u64,
        output: Some(result_hash.to_vec()),
        error: None,
    }
}

/// Execute a native job (for trusted L2 workloads).
pub fn execute_native_job(
    command: &str,
    input: &[u8],
    _config: &SandboxConfig,
) -> ExecutionResult {
    // For security, native execution is currently stubbed.
    let mut hasher = Sha256::new();
    hasher.update(command.as_bytes());
    hasher.update(input);
    let result = hasher.finalize();
    let mut result_hash = [0u8; 32];
    result_hash.copy_from_slice(&result);

    ExecutionResult {
        success: true,
        result_hash,
        execution_time_ms: 0,
        memory_used_bytes: input.len() as u64,
        output: Some(result_hash.to_vec()),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execute_wasm_empty_module() {
        let config = SandboxConfig::default();
        let result = execute_wasm_job(&[], b"input", &config);
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_execute_wasm_deterministic() {
        let config = SandboxConfig::default();
        let wasm = b"fake-wasm-module";
        let input = b"some-input";
        let r1 = execute_wasm_job(wasm, input, &config);
        let r2 = execute_wasm_job(wasm, input, &config);
        assert!(r1.success);
        assert_eq!(r1.result_hash, r2.result_hash);
    }

    #[test]
    fn test_execute_wasm_different_inputs() {
        let config = SandboxConfig::default();
        let wasm = b"fake-wasm-module";
        let r1 = execute_wasm_job(wasm, b"input-a", &config);
        let r2 = execute_wasm_job(wasm, b"input-b", &config);
        assert_ne!(r1.result_hash, r2.result_hash);
    }

    #[test]
    fn test_execute_native_deterministic() {
        let config = SandboxConfig::default();
        let r1 = execute_native_job("echo hello", b"data", &config);
        let r2 = execute_native_job("echo hello", b"data", &config);
        assert!(r1.success);
        assert_eq!(r1.result_hash, r2.result_hash);
    }

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert_eq!(config.max_memory_bytes, 256 * 1024 * 1024);
        assert_eq!(config.max_cpu_time_ms, 300_000);
        assert_eq!(config.max_output_bytes, 10 * 1024 * 1024);
    }
}
