//! Consensus-critical limits + engine identity for the WASM runtime (spec §6).
//! New file; wired via wasm/mod.rs. No existing-file changes.
//! Every constant here is destined for chain consensus params (founder, cycle #3):
//! two nodes disagreeing on ANY of them diverge on every job by design (the
//! fingerprint is folded into every outcome digest).

use sha2::{Digest, Sha256};

pub const ENGINE_ID: &str = "wasmi";
/// MUST match the `=` pin in Cargo.toml. Upgrading the engine is a coordinated
/// protocol change, never a silent bump (spec §2).
pub const ENGINE_VERSION: &str = "1.0.9";
pub const ABI_VERSION: u32 = 1;
pub const VALIDATION_VERSION: u32 = 1;
/// Domain-separation tag for every digest this runtime produces (spec §8).
pub const DOMAIN: &[u8] = b"commputer-pouw-wasm-v1";

/// Hard caps, identical on every node (spec §6). Fuel is the ONLY compute meter
/// (wall-clock is forbidden as a meter — it is non-deterministic and would cause
/// false disputes; see the dead stub src/node/src/wasm_executor.rs for the
/// anti-pattern this replaces).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmLimits {
    pub fuel: u64,
    pub max_memory_bytes: u64,
    pub max_call_depth: usize,
    pub max_stack_height: usize,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            fuel: 100_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
            max_call_depth: 1024,
            max_stack_height: 1 << 20,
            max_input_bytes: 10 * 1024 * 1024,
            max_output_bytes: 10 * 1024 * 1024,
        }
    }
}

impl WasmLimits {
    /// SHA-256 over the full determinism identity: engine id/version, ABI and
    /// validation-policy versions, and every limit, in a fixed serialization.
    pub fn config_fingerprint(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(DOMAIN);
        h.update(ENGINE_ID.as_bytes());
        h.update([0u8]); // separator: ENGINE_ID is not fixed-width
        h.update(ENGINE_VERSION.as_bytes());
        h.update([0u8]);
        h.update(ABI_VERSION.to_le_bytes());
        h.update(VALIDATION_VERSION.to_le_bytes());
        h.update(self.fuel.to_le_bytes());
        h.update(self.max_memory_bytes.to_le_bytes());
        h.update((self.max_call_depth as u64).to_le_bytes());
        h.update((self.max_stack_height as u64).to_le_bytes());
        h.update(self.max_input_bytes.to_le_bytes());
        h.update(self.max_output_bytes.to_le_bytes());
        h.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_and_limit_sensitive() {
        let a = WasmLimits::default();
        let b = WasmLimits::default();
        assert_eq!(a.config_fingerprint(), b.config_fingerprint());

        // EVERY field must perturb the fingerprint — if a field is ever dropped
        // from the hash, drift in it would no longer fail loud (spec §8).
        let base = a.config_fingerprint();
        let perturbations: [fn(&mut WasmLimits); 6] = [
            |l| l.fuel += 1,
            |l| l.max_memory_bytes += 1,
            |l| l.max_call_depth += 1,
            |l| l.max_stack_height += 1,
            |l| l.max_input_bytes += 1,
            |l| l.max_output_bytes += 1,
        ];
        for (i, perturb) in perturbations.iter().enumerate() {
            let mut c = WasmLimits::default();
            perturb(&mut c);
            assert_ne!(base, c.config_fingerprint(), "field #{i} must be fingerprint-sensitive");
        }
    }

    /// Known-answer pin: any change to the preimage serialization (field order,
    /// endianness, separators, constants) shows up here as a loud diff.
    #[test]
    fn default_fingerprint_golden_vector() {
        let hex: String = WasmLimits::default()
            .config_fingerprint()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        // Computed from the spec'd serialization on 2026-06-11 (wasmi 1.0.9 identity).
        assert_eq!(hex, "82230c7e1d031a9ff16a056510af9a3ed5e49aa11f04fa778777589f8e120cce");
    }

    #[test]
    fn defaults_are_the_spec_constants() {
        let l = WasmLimits::default();
        assert_eq!(l.fuel, 100_000_000);
        assert_eq!(l.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(l.max_call_depth, 1024);
        assert_eq!(l.max_stack_height, 1 << 20);
        assert_eq!(l.max_input_bytes, 10 * 1024 * 1024);
        assert_eq!(l.max_output_bytes, 10 * 1024 * 1024);
        assert_eq!(ENGINE_ID, "wasmi");
        assert_eq!(ENGINE_VERSION, "1.0.9");
    }
}
