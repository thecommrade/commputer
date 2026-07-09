//! Pure planner for the PoUW verifier commit/reveal loop (Track 2, Phase 1 — INERT).
//!
//! WHAT IT DOES: decides, deterministically and side-effect-free, which `Commit`/`Reveal` transactions
//! a bonded validator that has been drawn onto a verification committee should emit at a given block
//! height. It is a PURE function over a caller-built snapshot (`VerifierSnapshot`) + the durable
//! [`SaltStore`]; it performs no I/O, no wall-clock reads, and no randomness — so it is safe to call
//! on the event-loop thread and its output is reproducible.
//!
//! WHERE IT IS WIRED IN (later, PROTECTED phase — NOT here): `event_loop.rs` builds a
//! `VerifierSnapshot` each tick from `self.state.job_lifecycles` (via `to_record()`), calls the
//! two-step contract below, and turns each returned [`VerifierAction`] into a signed `Commit`/`Reveal`
//! gossiped from `self.wallet` (mirroring the `auto_register_validator` self-origination template).
//! This module is additive + inert: it builds and unit-tests standalone and changes no running-node
//! behavior.
//!
//! THE TWO-STEP SALT CONTRACT (fund-safety critical — DO NOT collapse into one call):
//! the commitment is `H(result_hash‖salt‖verifier)`; the salt is secret and must be persisted BEFORE
//! the `Commit` is broadcast (a crash in the commit→reveal gap burns the bond). The planner never
//! invents a salt. Instead:
//!   1. Caller calls [`jobs_needing_salt`] → the commit-eligible jobs that have NO persisted salt yet.
//!      For each, the caller generates a fresh random salt and `SaltStore::insert(job_id,
//!      my_result_hash, salt)` — which fsyncs BEFORE returning.
//!   2. Caller calls [`plan_verifier_actions`] → now the salt is present, so it emits the `Commit`
//!      (and any due `Reveal`). A commit-eligible job whose salt is still absent is silently skipped
//!      (the planner NEVER emits an un-persisted Commit).
//! Both calls are idempotent: once the chain records the commit (`already_committed`) the planner stops
//! re-emitting; likewise for reveal (`already_revealed`).
//!
//! FROZEN PRIMITIVE: the commitment value is produced by `commputer_pouw::commit_reveal::make_commitment`
//! (NOT reimplemented). Its real signature is `make_commitment(&ParticipantId, &result_hash, &salt,
//! bond) -> Commitment`; we take its `.commit` field. NOTE: the returned `commit` hash is
//! `H(result_hash‖salt‖verifier)` and is independent of `bond` (bond only sets a struct field), so the
//! emitted commitment is stable regardless of the bond value — we still pass the real `verifier_bond`.
//! The commitment binds to the SALT-STORE's `(result_hash, salt)` (the value the later `Reveal` opens),
//! guaranteeing commit/reveal consistency even if re-execution were re-run between the two steps.

use crate::salt_store::SaltStore;
use commputer_core::identity::Address;
use commputer_pouw::commit_reveal::make_commitment;
use commputer_pouw::ids::ParticipantId;

/// The lifecycle phase, from the verifier's point of view. The caller maps the on-chain
/// `PhaseRec`/`Phase` onto this (`Committing`/`Revealing`; everything else → `Other`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierPhase {
    /// Committee formed; verifiers may `Commit` until `commit_by`.
    Committing,
    /// Commit window closed; verifiers may `Reveal` until `reveal_by`.
    Revealing,
    /// Any other phase (AwaitingResult / Settled) — the planner does nothing.
    Other,
}

/// A transaction the verifier loop should originate. The caller signs + gossips it from its wallet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierAction {
    /// Post the hiding commitment for `job_id`. `commitment` == the frozen
    /// `make_commitment(..).commit` over the salt-store's `(result_hash, salt)` + this verifier.
    Commit { job_id: [u8; 32], commitment: [u8; 32] },
    /// Open a prior commitment for `job_id` with the salt-store's `(result_hash, salt)`.
    Reveal { job_id: [u8; 32], result_hash: [u8; 32], salt: [u8; 32] },
}

/// One committee this node was drawn onto, projected from the on-chain lifecycle record.
#[derive(Debug, Clone, Copy)]
pub struct MyCommittee {
    pub job_id: [u8; 32],
    pub phase: VerifierPhase,
    /// Last height at which a `Commit` is accepted.
    pub commit_by: u64,
    /// Last height at which a `Reveal` is accepted.
    pub reveal_by: u64,
    /// The exact bond `record_commit` requires (`c.bond == verifier_bond`).
    pub verifier_bond: u64,
    /// Whether the chain already recorded THIS node's commit (idempotency guard).
    pub already_committed: bool,
    /// Whether the chain already recorded THIS node's reveal (idempotency guard).
    pub already_revealed: bool,
    /// This node's OWN re-executed result hash (never copied from the on-chain `executor_hash`).
    /// `None` until re-execution completes — the planner will not commit without it.
    pub my_result_hash: Option<[u8; 32]>,
}

/// Loop policy knobs.
#[derive(Debug, Clone, Copy)]
pub struct VerifierCfg {
    /// Balance to keep free (fees / other txs) beyond the escrowed bond before committing.
    pub min_balance_reserve: u64,
}

/// Everything the planner needs, snapshotted by the caller from the just-applied `ChainState`.
#[derive(Debug, Clone)]
pub struct VerifierSnapshot {
    pub my_committees: Vec<MyCommittee>,
    pub my_address: Address,
    pub my_balance: u64,
    pub cfg: VerifierCfg,
}

/// `balance >= bond + reserve`, overflow-safe (a would-be-overflowing requirement is unaffordable).
fn affordable(balance: u64, bond: u64, reserve: u64) -> bool {
    match bond.checked_add(reserve) {
        Some(need) => balance >= need,
        None => false,
    }
}

/// Whether `c` is a live commit candidate IGNORING salt presence: committing phase, still in window,
/// not already committed, we know our own result hash, and we can afford the bond + reserve. A
/// drawn-but-underfunded verifier fails `affordable` here → it is skipped everywhere (never emits an
/// unpayable Commit).
fn commit_eligible(c: &MyCommittee, now_height: u64, balance: u64, reserve: u64) -> bool {
    c.phase == VerifierPhase::Committing
        && now_height <= c.commit_by
        && !c.already_committed
        && c.my_result_hash.is_some()
        && affordable(balance, c.verifier_bond, reserve)
}

/// STEP 1 of the commit contract. Returns the `job_id`s that are commit-eligible but have NO persisted
/// salt yet — the caller must generate + `SaltStore::insert` a salt for each (fsync) BEFORE calling
/// [`plan_verifier_actions`]. Deterministic (sorted, deduped); pure.
pub fn jobs_needing_salt(
    now_height: u64,
    snap: &VerifierSnapshot,
    salts: &SaltStore,
) -> Vec<[u8; 32]> {
    let mut out: Vec<[u8; 32]> = snap
        .my_committees
        .iter()
        .filter(|c| {
            commit_eligible(c, now_height, snap.my_balance, snap.cfg.min_balance_reserve)
        })
        .filter(|c| salts.get(&c.job_id).is_none())
        .map(|c| c.job_id)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// STEP 2 of the commit contract. Returns the `Commit`/`Reveal` actions to emit at `now_height`, in a
/// deterministic order (committees processed sorted by `job_id`). Pure + idempotent:
/// - `Commit` iff commit-eligible (see [`commit_eligible`]) AND a salt is already persisted for the job.
///   The commitment binds to the SALT-STORE's `(result_hash, salt)` via the frozen `make_commitment`.
/// - `Reveal` iff `Revealing`, `now_height <= reveal_by`, we already committed, we have NOT yet
///   revealed, AND we still hold the salt. A LOST salt → NO reveal (abstain + accept forfeiture rather
///   than broadcast a slashable garbage reveal).
pub fn plan_verifier_actions(
    now_height: u64,
    snap: &VerifierSnapshot,
    salts: &SaltStore,
) -> Vec<VerifierAction> {
    let me = ParticipantId(snap.my_address.0);
    let mut committees: Vec<&MyCommittee> = snap.my_committees.iter().collect();
    committees.sort_by(|a, b| a.job_id.cmp(&b.job_id));

    let mut actions = Vec::new();
    for c in committees {
        match c.phase {
            VerifierPhase::Committing => {
                if !commit_eligible(c, now_height, snap.my_balance, snap.cfg.min_balance_reserve) {
                    continue;
                }
                // Salt must have been generated + persisted by STEP 1; if not, skip (never emit an
                // un-persisted Commit — a crash before persistence would burn the bond).
                let Some((stored_hash, salt)) = salts.get(&c.job_id) else { continue };
                // Bind to what we WILL reveal (the stored pair) so commit/reveal always match.
                let commitment = make_commitment(&me, &stored_hash, &salt, c.verifier_bond).commit;
                actions.push(VerifierAction::Commit { job_id: c.job_id, commitment });
            }
            VerifierPhase::Revealing => {
                if c.already_revealed || !c.already_committed || now_height > c.reveal_by {
                    continue;
                }
                let Some((stored_hash, salt)) = salts.get(&c.job_id) else { continue };
                actions.push(VerifierAction::Reveal {
                    job_id: c.job_id,
                    result_hash: stored_hash,
                    salt,
                });
            }
            VerifierPhase::Other => continue,
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_pouw::commit_reveal::reveal_matches;
    use commputer_pouw::job::Reveal;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_store() -> (SaltStore, PathBuf) {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "commputer_vplanner_test_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (SaltStore::open(&dir).unwrap(), dir)
    }

    fn committee(job: u8, phase: VerifierPhase) -> MyCommittee {
        MyCommittee {
            job_id: [job; 32],
            phase,
            commit_by: 100,
            reveal_by: 200,
            verifier_bond: 20,
            already_committed: false,
            already_revealed: false,
            my_result_hash: Some([job.wrapping_add(1); 32]),
        }
    }

    fn snap(c: MyCommittee, balance: u64) -> VerifierSnapshot {
        VerifierSnapshot {
            my_committees: vec![c],
            my_address: Address([0xAB; 32]),
            my_balance: balance,
            cfg: VerifierCfg { min_balance_reserve: 5 },
        }
    }

    #[test]
    fn commits_when_committing_funded_and_salt_present() {
        let (mut store, dir) = scratch_store();
        let c = committee(1, VerifierPhase::Committing);
        let s = snap(c, 1_000);

        // Step 1: the job needs a salt.
        let need = jobs_needing_salt(50, &s, &store);
        assert_eq!(need, vec![[1u8; 32]]);

        // Caller persists (my_result_hash, salt), then step 2 commits.
        let rh = c.my_result_hash.unwrap();
        let salt = [42u8; 32];
        store.insert(c.job_id, rh, salt).unwrap();
        let actions = plan_verifier_actions(50, &s, &store);
        assert_eq!(actions.len(), 1);
        match actions[0] {
            VerifierAction::Commit { job_id, commitment } => {
                assert_eq!(job_id, [1u8; 32]);
                // Commitment equals the frozen primitive over the SAME (rh, salt, verifier).
                let me = ParticipantId([0xAB; 32]);
                let expected = make_commitment(&me, &rh, &salt, 20).commit;
                assert_eq!(commitment, expected);
                // And a Reveal of that (rh, salt) opens it (commit/reveal consistency).
                let opened = Reveal { verifier: me, result_hash: rh, salt };
                let c_struct = make_commitment(&me, &rh, &salt, 20);
                assert!(reveal_matches(&c_struct, &opened));
            }
            other => panic!("expected Commit, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_commit_before_salt_is_persisted() {
        let (store, dir) = scratch_store();
        let c = committee(1, VerifierPhase::Committing);
        let s = snap(c, 1_000);
        // Salt not yet inserted → planner must NOT emit a Commit (would risk an un-persisted commit).
        assert!(plan_verifier_actions(50, &s, &store).is_empty());
        assert_eq!(jobs_needing_salt(50, &s, &store), vec![[1u8; 32]]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_underfunded_verifier() {
        let (mut store, dir) = scratch_store();
        let c = committee(1, VerifierPhase::Committing);
        // balance (24) < bond (20) + reserve (5) = 25 → unaffordable.
        let s = snap(c, 24);
        assert!(jobs_needing_salt(50, &s, &store).is_empty(), "underfunded → not asked to salt");
        // Even if a salt somehow exists, an unpayable Commit is never emitted.
        store.insert(c.job_id, c.my_result_hash.unwrap(), [7u8; 32]).unwrap();
        assert!(plan_verifier_actions(50, &s, &store).is_empty(), "underfunded → no Commit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn just_affordable_commits() {
        let (mut store, dir) = scratch_store();
        let c = committee(1, VerifierPhase::Committing);
        // balance exactly bond + reserve = 25 → affordable.
        let s = snap(c, 25);
        store.insert(c.job_id, c.my_result_hash.unwrap(), [7u8; 32]).unwrap();
        assert_eq!(plan_verifier_actions(50, &s, &store).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_commit_without_own_result_hash() {
        let (store, dir) = scratch_store();
        let mut c = committee(1, VerifierPhase::Committing);
        c.my_result_hash = None; // re-execution not done yet
        let s = snap(c, 1_000);
        assert!(jobs_needing_salt(50, &s, &store).is_empty());
        assert!(plan_verifier_actions(50, &s, &store).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_commit_past_commit_window() {
        let (mut store, dir) = scratch_store();
        let c = committee(1, VerifierPhase::Committing);
        let s = snap(c, 1_000);
        store.insert(c.job_id, c.my_result_hash.unwrap(), [7u8; 32]).unwrap();
        // now_height (101) > commit_by (100).
        assert!(plan_verifier_actions(101, &s, &store).is_empty());
        assert!(jobs_needing_salt(101, &s, &store).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_no_recommit_when_already_committed() {
        let (mut store, dir) = scratch_store();
        let mut c = committee(1, VerifierPhase::Committing);
        c.already_committed = true;
        let s = snap(c, 1_000);
        store.insert(c.job_id, c.my_result_hash.unwrap(), [7u8; 32]).unwrap();
        assert!(plan_verifier_actions(50, &s, &store).is_empty(), "must not re-commit");
        assert!(jobs_needing_salt(50, &s, &store).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reveals_only_after_commit_with_stored_salt() {
        let (mut store, dir) = scratch_store();
        let mut c = committee(1, VerifierPhase::Revealing);
        c.already_committed = true;
        let s = snap(c, 1_000);
        let rh = [55u8; 32];
        let salt = [66u8; 32];
        store.insert(c.job_id, rh, salt).unwrap();
        let actions = plan_verifier_actions(150, &s, &store);
        assert_eq!(
            actions,
            vec![VerifierAction::Reveal { job_id: [1u8; 32], result_hash: rh, salt }]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_reveal_without_prior_commit() {
        let (mut store, dir) = scratch_store();
        let c = committee(1, VerifierPhase::Revealing); // already_committed = false
        let s = snap(c, 1_000);
        store.insert(c.job_id, [55u8; 32], [66u8; 32]).unwrap();
        assert!(plan_verifier_actions(150, &s, &store).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lost_salt_means_no_reveal() {
        let (store, dir) = scratch_store();
        let mut c = committee(1, VerifierPhase::Revealing);
        c.already_committed = true;
        let s = snap(c, 1_000);
        // No salt persisted (simulating a lost/never-persisted salt) → abstain, never garbage-reveal.
        assert!(plan_verifier_actions(150, &s, &store).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_no_rereveal_when_already_revealed() {
        let (mut store, dir) = scratch_store();
        let mut c = committee(1, VerifierPhase::Revealing);
        c.already_committed = true;
        c.already_revealed = true;
        let s = snap(c, 1_000);
        store.insert(c.job_id, [55u8; 32], [66u8; 32]).unwrap();
        assert!(plan_verifier_actions(150, &s, &store).is_empty(), "must not re-reveal");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_reveal_past_reveal_window() {
        let (mut store, dir) = scratch_store();
        let mut c = committee(1, VerifierPhase::Revealing);
        c.already_committed = true;
        let s = snap(c, 1_000);
        store.insert(c.job_id, [55u8; 32], [66u8; 32]).unwrap();
        assert!(plan_verifier_actions(201, &s, &store).is_empty(), "now_height > reveal_by");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Restart safety: commit with a persisted salt, DROP the store, reopen from disk, and confirm the
    /// reveal still fires with the recovered salt — the salt store + planner survive a crash.
    #[test]
    fn reopen_recovers_salt_for_reveal() {
        static N: AtomicU64 = AtomicU64::new(9000);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("commputer_vplanner_reopen_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        let rh = [77u8; 32];
        let salt = [88u8; 32];
        {
            let mut store = SaltStore::open(&dir).unwrap();
            let c = committee(3, VerifierPhase::Committing);
            let s = snap(c, 1_000);
            store.insert(c.job_id, rh, salt).unwrap();
            assert_eq!(plan_verifier_actions(50, &s, &store).len(), 1, "commits pre-restart");
        }
        // Node restarts; store reloaded from disk; job now in Revealing, already committed.
        let store = SaltStore::open(&dir).unwrap();
        let mut c = committee(3, VerifierPhase::Revealing);
        c.already_committed = true;
        let s = snap(c, 1_000);
        assert_eq!(
            plan_verifier_actions(150, &s, &store),
            vec![VerifierAction::Reveal { job_id: [3u8; 32], result_hash: rh, salt }],
            "reveal uses the salt recovered from disk after restart"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deterministic ordering: many committees → actions ordered by ascending job_id regardless of the
    /// input Vec order.
    #[test]
    fn actions_are_ordered_by_job_id() {
        static N: AtomicU64 = AtomicU64::new(5000);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("commputer_vplanner_order_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SaltStore::open(&dir).unwrap();

        let mut cs = Vec::new();
        for job in [7u8, 2, 9, 4] {
            let mut c = committee(job, VerifierPhase::Revealing);
            c.already_committed = true;
            store.insert(c.job_id, [job; 32], [job; 32]).unwrap();
            cs.push(c);
        }
        let s = VerifierSnapshot {
            my_committees: cs,
            my_address: Address([0xAB; 32]),
            my_balance: 10_000,
            cfg: VerifierCfg { min_balance_reserve: 5 },
        };
        let actions = plan_verifier_actions(150, &s, &store);
        let ids: Vec<u8> = actions
            .iter()
            .map(|a| match a {
                VerifierAction::Reveal { job_id, .. } => job_id[0],
                VerifierAction::Commit { job_id, .. } => job_id[0],
            })
            .collect();
        assert_eq!(ids, vec![2, 4, 7, 9], "actions sorted by job_id deterministically");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
