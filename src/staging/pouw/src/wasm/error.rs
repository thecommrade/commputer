//! Deterministic error model + the canonical outcome digest fold (spec §8).
//! New file; wired via wasm/mod.rs. No existing-file changes.

use crate::wasm::limits::{WasmLimits, DOMAIN};
use sha2::{Digest, Sha256};

/// Every way an execution can fail. All variants are deterministic given
/// identical (program, input, limits, engine version) — EXCEPT
/// `ProgramUnavailable`, which depends on local store state; safe this cycle
/// only because tests/sim populate every node's store (spec §8/§10.2).
/// Payload strings are for local logs/tests ONLY — they never reach the digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    ProgramUnavailable,
    HashMismatch,
    /// Failed the determinism gate (validation.rs) or wasmi translation.
    Rejected(String),
    OutOfFuel,
    /// Any other runtime trap (unreachable, OOB, div0, stack/recursion cap...).
    Trapped(String),
    /// The guest violated the ABI contract (bad alloc ptr, oversized/OOB output).
    AbiViolation(String),
}

/// The rich result of `WasmOracle::execute` — NOT on the trait. The future
/// cost-coupling cycle reads `fuel_consumed` from here (spec §3/§10.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutcome {
    pub result: Result<Vec<u8>, ExecError>,
    /// budget − remaining. 0 when execution never started. NOTE: on an
    /// out-of-fuel trap wasmi leaves a small remainder, so this is generally
    /// < budget even for OutOfFuel; the consensus property is cross-node
    /// EQUALITY of this number, not any particular value.
    pub fuel_consumed: u64,
}

impl ExecOutcome {
    /// The consensus-facing 32-byte value (what `ExecutionOracle::run` returns).
    pub fn outcome_digest(&self, limits: &WasmLimits) -> [u8; 32] {
        match &self.result {
            Ok(out) => ok_digest(limits, out),
            Err(_) => error_digest(limits),
        }
    }
}

/// sha256(DOMAIN ‖ fingerprint ‖ 0x00 ‖ output)
pub fn ok_digest(limits: &WasmLimits, output: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(limits.config_fingerprint());
    h.update([0x00]);
    h.update(output);
    h.finalize().into()
}

/// sha256(DOMAIN ‖ fingerprint ‖ 0x01) — ONE sentinel for every error kind,
/// so "which trap" cannot be a covert channel (spec §8).
pub fn error_digest(limits: &WasmLimits) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(limits.config_fingerprint());
    h.update([0x01]);
    h.finalize().into()
}

/// Map a wasmi runtime error deterministically. Replicates the arms of wasmi's
/// pub(crate) `Error::is_out_of_fuel` (wasmi-1.0.9 src/error.rs:131-141).
/// `ErrorKind::ResumableOutOfFuel` is intentionally omitted: it is doc(hidden)
/// and can only arise from resumable calls, which this oracle never uses.
pub fn classify_wasmi_error(e: &wasmi::Error) -> ExecError {
    use wasmi::errors::{ErrorKind, FuelError, MemoryError, TableError};
    use wasmi::TrapCode;
    let out_of_fuel = matches!(
        e.kind(),
        ErrorKind::TrapCode(TrapCode::OutOfFuel)
            | ErrorKind::Memory(MemoryError::OutOfFuel { .. })
            | ErrorKind::Table(TableError::OutOfFuel { .. })
            | ErrorKind::Fuel(FuelError::OutOfFuel { .. })
    );
    if out_of_fuel {
        ExecError::OutOfFuel
    } else {
        ExecError::Trapped(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm::limits::WasmLimits;

    #[test]
    fn digests_are_deterministic_and_domain_separated() {
        let l = WasmLimits::default();
        assert_eq!(ok_digest(&l, b"out"), ok_digest(&l, b"out"));
        assert_ne!(ok_digest(&l, b"out"), ok_digest(&l, b"other"));
        assert_eq!(error_digest(&l), error_digest(&l));
        // The OK digest of ANY output can never equal the error sentinel
        // (0x00 vs 0x01 discriminant byte in the preimage).
        assert_ne!(ok_digest(&l, b""), error_digest(&l));
    }

    #[test]
    fn config_drift_diverges_every_digest() {
        let a = WasmLimits::default();
        let mut b = WasmLimits::default();
        b.fuel += 1; // a mis-configured node
        assert_ne!(ok_digest(&a, b"x"), ok_digest(&b, b"x"), "drift must fail loud (spec §8)");
        assert_ne!(error_digest(&a), error_digest(&b));
    }

    #[test]
    fn every_error_kind_folds_to_the_same_sentinel() {
        let l = WasmLimits::default();
        let kinds = [
            ExecError::ProgramUnavailable,
            ExecError::HashMismatch,
            ExecError::Rejected("x".into()),
            ExecError::OutOfFuel,
            ExecError::Trapped("y".into()),
            ExecError::AbiViolation("z".into()),
        ];
        for k in kinds {
            let o = ExecOutcome { result: Err(k), fuel_consumed: 0 };
            assert_eq!(o.outcome_digest(&l), error_digest(&l),
                "which-error must be indistinguishable in the digest (no covert channel, spec §8)");
        }
    }
}
