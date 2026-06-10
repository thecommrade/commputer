//! Adversarial agent strategies for the simulation (plan Task 13).
//!
//! WHAT THIS DOES: models the executor and verifier *strategies* the Monte-Carlo
//! tournament (Task 14) pits against each other. Each strategy answers one question —
//! "what `result_hash` do I claim/reveal for this job?" — and that answer is what the
//! [`crate::engine::run_job`] orchestrator consumes through its `ClaimFn` / `RevealFn`
//! closure seams. The engine never knows about strategies; it only sees the hash a
//! strategy produces, so these enums fully describe the adversary set.
//!
//! WHERE THIS IS WIRED IN: `sim/tournament.rs` (Task 14) builds the per-job closures by
//! calling [`Executor::claim`] and [`Verifier::reveal`] here; `sim/main.rs` declares
//! `mod agents;` so the strategies compile and their unit tests run under
//! `cargo test -p commputer-pouw`.
//!
//! SPEC: §4 (threat model) and §10 (sim) of
//! `src/staging/docs/2026-06-10-pouw-verification-game-design.md`.

use commputer_pouw::ids::ParticipantId;

/// A distinct, deterministic "wrong" hash, derived from a true hash so it is reproducible
/// but never equal to it. A lazy executor (skipped the work) and a cheating executor
/// (computed a plausible-but-wrong answer) both surface a wrong hash; they differ in
/// *why* (saved compute vs. intent), which the tournament models via compute-cost, not
/// via the hash. We fold a tag byte in so "lazy" and "cheat" produce *different* wrong
/// hashes — useful for asserting the two strategies are not accidentally identical.
fn wrong_hash(true_hash: &[u8; 32], tag: u8) -> [u8; 32] {
    let mut h = commputer_pouw::ids::hash_parts(&[true_hash, &[tag]]);
    // Guard against the astronomically-unlikely event that H collides with the input.
    if &h == true_hash {
        h[0] ^= 0xFF;
    }
    h
}

/// Executor strategies (spec §4). The executor fixes its claimed `result_hash` in the
/// execute phase, *before* learning whether the job is sampled, so a strategy cannot
/// selectively cheat only on unsampled jobs — exactly the engine's ordering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Executor {
    /// Runs the work and claims the true result. The only EV-positive strategy under a
    /// well-tuned regime.
    Honest,
    /// Skips the work and returns garbage (a wrong hash). Saves compute `C_exec` but is
    /// caught by sampling or a trap.
    Lazy,
    /// Computes a plausible-but-wrong answer (a wrong hash, distinct from the lazy one).
    /// Models a targeted lie rather than a skipped job.
    Cheat,
}

impl Executor {
    /// The `result_hash` this executor *claims*, given the job's true result hash.
    /// Honest ⇒ the true hash; Lazy/Cheat ⇒ a deterministic wrong hash (distinct from
    /// each other). This is the value the engine's `ClaimFn` returns.
    pub fn claim(&self, true_hash: &[u8; 32]) -> [u8; 32] {
        match self {
            Executor::Honest => *true_hash,
            Executor::Lazy => wrong_hash(true_hash, 0x01),
            Executor::Cheat => wrong_hash(true_hash, 0x02),
        }
    }

    /// Whether this strategy actually performs the job's compute. Honest pays the compute
    /// cost; Lazy skips it (that is the saved-cost the slash must dominate); Cheat still
    /// avoids the *honest* computation. The tournament charges `C_exec` only when `true`.
    pub fn does_work(&self) -> bool {
        matches!(self, Executor::Honest)
    }
}

/// Verifier strategies (spec §4). A verifier reveals a `result_hash` for the claim under
/// review; the committee verdict is a quorum over these reveals.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verifier {
    /// Re-executes the job and reveals the true result. Pays verify cost `C_ver`; earns
    /// the verifier slice and survives traps.
    Honest,
    /// Skips the work and echoes the executor's claimed hash (the verifier's dilemma).
    /// Free when the executor is honest, but trap-slashed when the claim is planted-wrong.
    RubberStamp,
    /// Colludes with a *named* executor: rubber-stamps (echoes the claim) when that
    /// executor is the one under review, but verifies honestly for everyone else (so it
    /// is not trivially caught on unrelated jobs/traps).
    Collude(ParticipantId),
}

impl Verifier {
    /// The `result_hash` this verifier *reveals*, given the verifier's id, the job's true
    /// result hash, the executor's claimed hash, and *which executor* produced the claim.
    /// Honest ⇒ the true hash; RubberStamp ⇒ the executor's claim; Collude ⇒ the claim iff
    /// the reviewed executor is the collusion partner, else the true hash. This is the
    /// value the engine's `RevealFn` returns (the engine supplies the first three args;
    /// the tournament binds `executor` since it knows who is under review).
    pub fn reveal(
        &self,
        _verifier: &ParticipantId,
        true_hash: &[u8; 32],
        executor_claim: &[u8; 32],
        executor: &ParticipantId,
    ) -> [u8; 32] {
        match self {
            Verifier::Honest => *true_hash,
            Verifier::RubberStamp => *executor_claim,
            Verifier::Collude(partner) => {
                if partner == executor {
                    *executor_claim
                } else {
                    *true_hash
                }
            }
        }
    }

    /// Whether this strategy actually re-executes (and so pays verify cost `C_ver`).
    /// Honest always works; RubberStamp never does; a colluder skips the work only for its
    /// partner (where it rubber-stamps) and works otherwise.
    pub fn does_work(&self, executor: &ParticipantId) -> bool {
        match self {
            Verifier::Honest => true,
            Verifier::RubberStamp => false,
            Verifier::Collude(partner) => partner != executor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    #[test]
    fn honest_executor_claims_the_true_hash() {
        let truth = [9u8; 32];
        assert_eq!(Executor::Honest.claim(&truth), truth);
        assert!(Executor::Honest.does_work());
    }

    #[test]
    fn lazy_and_cheat_executors_claim_distinct_wrong_hashes() {
        let truth = [9u8; 32];
        let lazy = Executor::Lazy.claim(&truth);
        let cheat = Executor::Cheat.claim(&truth);
        // Both are wrong (not the true hash)...
        assert_ne!(lazy, truth);
        assert_ne!(cheat, truth);
        // ...and distinct from each other (so the two strategies are separable)...
        assert_ne!(lazy, cheat);
        // ...and deterministic (a strategy is reproducible across runs).
        assert_eq!(lazy, Executor::Lazy.claim(&truth));
        assert_eq!(cheat, Executor::Cheat.claim(&truth));
        // Neither does the work (that is the saved compute the slash must dominate).
        assert!(!Executor::Lazy.does_work());
        assert!(!Executor::Cheat.does_work());
    }

    #[test]
    fn honest_verifier_reveals_the_true_hash_ignoring_the_claim() {
        let truth = [9u8; 32];
        let claim = [0xABu8; 32]; // executor lied
        let exec = pid(9);
        assert_eq!(Verifier::Honest.reveal(&pid(1), &truth, &claim, &exec), truth);
        assert!(Verifier::Honest.does_work(&exec));
    }

    #[test]
    fn rubber_stamp_verifier_echoes_the_executor_claim() {
        let truth = [9u8; 32];
        let claim = [0xABu8; 32]; // executor lied; rubber-stamp blindly echoes it
        let exec = pid(9);
        assert_eq!(
            Verifier::RubberStamp.reveal(&pid(1), &truth, &claim, &exec),
            claim
        );
        // A rubber-stamper never does the verify work.
        assert!(!Verifier::RubberStamp.does_work(&exec));
    }

    #[test]
    fn colluder_stamps_its_partner_but_verifies_everyone_else() {
        let truth = [9u8; 32];
        let claim = [0xABu8; 32];
        let partner = pid(9);
        let stranger = pid(8);
        let v = Verifier::Collude(partner);
        // With its partner under review, the colluder rubber-stamps the (wrong) claim...
        assert_eq!(v.reveal(&pid(1), &truth, &claim, &partner), claim);
        assert!(!v.does_work(&partner));
        // ...but for any other executor it reveals the true hash (stays clean on traps).
        assert_eq!(v.reveal(&pid(1), &truth, &claim, &stranger), truth);
        assert!(v.does_work(&stranger));
    }
}
