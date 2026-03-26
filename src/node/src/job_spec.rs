#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;

/// JSON schema for compute job specifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpecification {
    pub version: u32,
    pub runtime: RuntimeType,
    pub input_hash: String,
    pub expected_output_format: OutputFormat,
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub storage_mb: u64,
    pub timeout_secs: u64,
    pub wasm_module_hash: Option<String>,
    pub environment: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuntimeType {
    Wasm,
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputFormat {
    Binary,
    Json,
    Text,
}

impl JobSpecification {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported spec version".into());
        }
        if self.cpu_cores == 0 {
            return Err("At least 1 CPU core required".into());
        }
        if self.timeout_secs == 0 || self.timeout_secs > 86400 {
            return Err("Timeout must be 1-86400 secs".into());
        }
        if matches!(self.runtime, RuntimeType::Wasm) && self.wasm_module_hash.is_none() {
            return Err("WASM runtime requires wasm_module_hash".into());
        }
        Ok(())
    }

    pub fn compute_hash(&self) -> [u8; 32] {
        let json = serde_json::to_vec(self).unwrap_or_default();
        let hash = Sha256::digest(&json);
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spec() -> JobSpecification {
        JobSpecification {
            version: 1,
            runtime: RuntimeType::Native,
            input_hash: "abc123".into(),
            expected_output_format: OutputFormat::Json,
            cpu_cores: 2,
            gpu_vram_mb: 0,
            ram_mb: 1024,
            storage_mb: 0,
            timeout_secs: 300,
            wasm_module_hash: None,
            environment: HashMap::new(),
        }
    }

    #[test]
    fn test_validate_ok() {
        assert!(make_spec().validate().is_ok());
    }

    #[test]
    fn test_validate_bad_version() {
        let mut spec = make_spec();
        spec.version = 99;
        assert!(spec.validate().is_err());
    }

    #[test]
    fn test_validate_zero_cores() {
        let mut spec = make_spec();
        spec.cpu_cores = 0;
        assert!(spec.validate().is_err());
    }

    #[test]
    fn test_validate_zero_timeout() {
        let mut spec = make_spec();
        spec.timeout_secs = 0;
        assert!(spec.validate().is_err());
    }

    #[test]
    fn test_validate_wasm_needs_hash() {
        let mut spec = make_spec();
        spec.runtime = RuntimeType::Wasm;
        spec.wasm_module_hash = None;
        assert!(spec.validate().is_err());

        spec.wasm_module_hash = Some("deadbeef".into());
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn test_compute_hash_deterministic() {
        let spec = make_spec();
        let h1 = spec.compute_hash();
        let h2 = spec.compute_hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_compute_hash_changes_with_input() {
        let mut spec = make_spec();
        let h1 = spec.compute_hash();
        spec.input_hash = "different".into();
        let h2 = spec.compute_hash();
        assert_ne!(h1, h2);
    }
}
