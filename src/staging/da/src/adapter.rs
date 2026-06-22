//! Integration glue: `resolve_and_populate` — given a `DataAvailability` driver, an
//! attestation, and the verifier's identity, returns `true` if data is `Available` (and
//! calls the caller-supplied `insert` sink with the bytes) or `false` on `Abstain`.
//!
//! # §7.1 wiring contract (VERBATIM from spec §7.1)
//!
//! **Pre-commit resolution.** A selected verifier runs `verify_available` FIRST. It only
//! commits + escrows its bond on `Available`. An `Abstain` verifier **never escrows** —
//! so `reveals.len() == committee.len()` holds for the *participating* set and no escrow
//! is ever stranded.
//!
//! **Effective committee + quorum.** The verification game runs on the **effective
//! committee** (the verifiers that obtained the data and committed); `quorum` is computed
//! over that effective size. If availability shrinks the effective committee below the
//! quorum threshold (or to zero), that is exactly the existing `Verdict::NoQuorum` →
//! existing escalation trigger — no new enum, no new verdict.
//!
//! **Where this lands.** Wiring pre-commit availability into committee formation + the
//! quorum denominator interacts with bond-escrow timing in `engine.rs` (a
//! verification-game file). That is **founder-owned integration**, NOT an agent edit.
//! Layers 0–4 deliver the standalone DA crate; this adapter returns `Available|Abstain`
//! and documents this exact wiring contract for the founder.
//!
//! # Crate coupling
//!
//! The `commputer-pouw` crate is NOT a dependency of `commputer-da`. The adapter
//! therefore takes a generic `insert: FnMut(&[u8])` sink so an `Available` result can
//! feed an external `ProgramStore` (or any store that accepts a byte slice) WITHOUT
//! coupling the two crates. The founder wires `|bytes| program_store.insert(job_id, bytes)`
//! at the engine layer.
//!
//! # Where to wire in
//!
//! - Call site: the engine's committee-selection loop, BEFORE `engine::commit_verifier`.
//! - Existing file requiring changes: `src/staging/pouw/src/engine.rs` — founder-owned.
//! - No changes to `oracle.rs`, `settlement.rs`, `verdict.rs`, or any protected file.

use crate::facade::{AvailabilityOutcome, DataAvailability};
use crate::params::DaAttestation;
use crate::transport::{Clock, DaTransport};

/// Resolve data availability and, on `Available`, call `insert` with the reconstructed
/// bytes.
///
/// Returns `true` iff outcome is `Available` (the verifier should commit + escrow its
/// bond); returns `false` on `Abstain` (the verifier does not commit, does not escrow,
/// is absent from the effective committee).
///
/// The `insert` sink is called **exactly once** on success with a slice of the
/// reconstructed + sha256-rebound bytes. It is never called on `Abstain`.
///
/// # Example (pseudocode — founder wiring in engine.rs)
/// ```ignore
/// let participating = resolve_and_populate(
///     &da_driver, &attestation, job_id, epoch, verifier_id,
///     |bytes| program_store.insert(job_id, Arc::from(bytes)),
/// );
/// if participating {
///     engine.commit_verifier(verifier_id, bond);
/// }
/// // Abstaining verifiers are simply absent; the effective committee shrinks.
/// ```
pub fn resolve_and_populate<T, C, F>(
    da: &DataAvailability<'_, T, C>,
    att: &DaAttestation,
    job_id: [u8; 32],
    epoch: u64,
    verifier_id: [u8; 32],
    mut insert: F,
) -> bool
where
    T: DaTransport,
    C: Clock,
    F: FnMut(&[u8]),
{
    match da.verify_available(att, job_id, epoch, verifier_id) {
        AvailabilityOutcome::Available(bytes) => {
            insert(&bytes);
            true
        }
        AvailabilityOutcome::Abstain => false,
    }
}
