//! executor_planner.rs — the PURE, INERT decision core of the PoUW executor auto-claim loop
//! (Track-2 Phase 1). Given a snapshot of the node's view of open jobs + its own claims, decide
//! which `ClaimJob` / `CompleteJob` transactions to emit this tick. Plus the re-execution shim the
//! loop drives on a blocking thread, and the `program‖input` DA-blob envelope codec.
//!
//! WHAT (three concerns, no async, no I/O, no wall-clock, no rng):
//!   1. `plan_executor_actions` — a deterministic, idempotent function `(now_height, snapshot)
//!      -> Vec<ExecutorAction>`. It mirrors the live on-chain admission gates (`apply_claim_job`
//!      state.rs:1843, `post_result` lifecycle.rs:494) so the loop only ever broadcasts txs the
//!      chain will accept, and never re-emits an action already reflected in state.
//!   2. `reexecute` — a thin sync shim over `commputer::pouw_executor::execute_job` (the frozen,
//!      fuel-metered, deterministic kernel). The loop calls this inside `spawn_blocking`; it is
//!      CPU-only and re-executes with the WasmLimits it is HANDED (consensus-anchored, never
//!      `default()` — see the plan's determinism-slash risk). `ExecError` is re-exported.
//!   3. `encode_job_blob` / `split_job_blob` — the single-envelope `[program_len:u32 LE][program]
//!      [input]` DA-blob format (founder decision Q1: one blob = program‖input). The publisher
//!      encodes; the executor/verifier fetch ONE blob and split it before re-executing.
//!
//! WHERE THIS IS WIRED IN (later, PROTECTED phases — NOT wired now, this module is inert):
//!   * `event_loop.rs` (PROTECTED): a new `executor_tick_interval` `select!` arm builds an
//!     `ExecutorSnapshot` from `self.state` (pending_jobs → open_jobs, job_lifecycles → my_claims,
//!     self.wallet.address() → my_address) and calls `plan_executor_actions`; each returned action
//!     is built+signed+gossiped via the existing tx-emit pattern (event_loop.rs:369). Re-execution
//!     runs on `spawn_blocking(reexecute)` with `result_hash` returned over a dedicated mpsc arm.
//!   * `main.rs` / `config.rs` (PROTECTED): construct the loop, `[executor] enabled=false` default,
//!     concurrency cap + min-balance reserve knobs → `ExecutorCfg`.
//! FILES NEEDING CHANGES for the live wire-in: `event_loop.rs`, `main.rs`, `config.rs` (all
//! PROTECTED, founder-gated) + `pub mod executor_planner;` already added to `lib.rs`.

// Inert until the PROTECTED wire-in: the planner + shim have no in-tree callers yet.
#![allow(dead_code)]

use std::collections::HashSet;

use commputer_core::identity::Address;
use commputer_pouw::wasm::WasmLimits;

// Re-export the kernel's error so the loop (and its tests) name a single `ExecError`.
pub use crate::pouw_executor::ExecError;

/// A transaction the executor loop should emit this tick. Job-id-addressed so the planner stays
/// idempotent and the caller can dedupe/track in-flight by `job_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutorAction {
    /// Claim an open pending job (→ on-chain `ClaimJob{job_id}`; opens the lifecycle at
    /// `AwaitingResult`, escrows `max(budget, executor_bond)`).
    Claim { job_id: [u8; 32] },
    /// Deliver the executed result for a job we already claimed (→ on-chain
    /// `CompleteJob{job_id, result_hash}`; the committee is drawn in the block tail).
    Complete { job_id: [u8; 32], result_hash: [u8; 32] },
}

impl ExecutorAction {
    /// The job this action targets — the deterministic sort/dedup key.
    fn job_id(&self) -> [u8; 32] {
        match self {
            ExecutorAction::Claim { job_id } => *job_id,
            ExecutorAction::Complete { job_id, .. } => *job_id,
        }
    }

    /// Tie-break rank when two actions share a job_id (they never do in a well-formed snapshot —
    /// a job is either open OR claimed by us, not both — but keeps the ordering total & stable).
    fn variant_rank(&self) -> u8 {
        match self {
            ExecutorAction::Complete { .. } => 0,
            ExecutorAction::Claim { .. } => 1,
        }
    }
}

/// An unclaimed job visible in `pending_jobs`. Mirrors `PendingJobRecord` (state.rs:3254) minus the
/// fields the executor cannot act on from on-chain data alone (resources/max_duration are dropped
/// there too — see plan open-Q 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenJob {
    pub job_id: [u8; 32],
    /// Escrowed pot == budget; the claim must fund `max(budget, executor_bond)`.
    pub budget: u64,
    pub program_hash: [u8; 32],
    pub input_hash: [u8; 32],
    /// DA anchor for the `program‖input` blob (founder Q1).
    pub da_root: [u8; 32],
    /// Last height at which a `ClaimJob` is admissible (state.rs:1873).
    pub claim_by: u64,
}

/// The executor's own view of a job it has already claimed (derived from `job_lifecycles`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MyClaim {
    pub job_id: [u8; 32],
    pub phase: ClaimPhase,
    /// Last height at which a `CompleteJob` is admissible (deadlines.result_by).
    pub result_by: u64,
    /// `Some(result_hash)` once the loop has re-executed the job off-thread; `None` while the
    /// re-execution is still pending (or the bytes are not yet available).
    pub have_result: Option<[u8; 32]>,
}

/// Lifecycle phase, mirroring the on-chain `PhaseRec` (lifecycle.rs:111). `CompleteJob` is only
/// admissible at `AwaitingResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimPhase {
    AwaitingResult,
    Committing,
    Revealing,
    Settled,
}

/// Operator policy knobs (from `[executor]` config, sourced at construction — PROTECTED wire-in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutorCfg {
    /// Max jobs worked concurrently (claimed-but-not-settled + in-flight). Caps capital lock-up.
    pub max_concurrent_claims: usize,
    /// Balance never spent by auto-claim — a floor kept for fees/liveness.
    pub min_balance_reserve: u64,
    /// The executor bond floor (== `game_params.executor_bond`); escrow is `max(budget, this)`.
    pub executor_bond: u64,
}

/// Everything the planner needs, snapshotted from `ChainState` + the loop's own in-flight tracking.
/// Built by the PROTECTED tick; the planner never touches live state.
#[derive(Debug, Clone)]
pub struct ExecutorSnapshot {
    pub open_jobs: Vec<OpenJob>,
    pub my_claims: Vec<MyClaim>,
    pub my_address: Address,
    /// Spendable balance of the executor wallet.
    pub my_balance: u64,
    /// job_ids for which the loop already has an OUTSTANDING action (a broadcast Claim not yet
    /// reflected in `my_claims`, or a broadcast Complete not yet applied). The idempotency guard.
    pub in_flight: HashSet<[u8; 32]>,
    pub cfg: ExecutorCfg,
}

/// Decide the executor's actions for the tick at `now_height`. PURE + deterministic + idempotent:
/// the same snapshot always yields the same actions, and an action already reflected in
/// `my_claims`/`in_flight` is never re-emitted.
///
/// Rules (mirror the live on-chain gates so the loop never broadcasts a doomed tx):
///   * COMPLETE a claim iff `phase == AwaitingResult`, `now_height <= result_by`, `have_result`
///     is `Some`, and it is not already `in_flight` (don't re-complete).
///   * CLAIM an open job iff it is not `in_flight`, not already among `my_claims`, `now_height <=
///     claim_by`, and it is affordable — the wallet can fund `max(budget, executor_bond)` while
///     still keeping `min_balance_reserve` — subject to `max_concurrent_claims`.
/// Claims are affordability-accounted CUMULATIVELY across the tick (a running available balance is
/// debited per planned claim) so a tick can never plan claims whose combined escrow exceeds the
/// wallet. Selection is in `job_id` order; the returned vec is sorted by `job_id`.
pub fn plan_executor_actions(now_height: u64, snap: &ExecutorSnapshot) -> Vec<ExecutorAction> {
    let mut actions: Vec<ExecutorAction> = Vec::new();

    // ---- COMPLETE: resume claims whose result we have and whose window is still open. ----
    for c in &snap.my_claims {
        if c.phase != ClaimPhase::AwaitingResult {
            continue; // committee already drawn / settled — nothing for the executor to do.
        }
        if now_height > c.result_by {
            continue; // past the deadline; `post_result` would reject (→ Timeout on settle).
        }
        if snap.in_flight.contains(&c.job_id) {
            continue; // a CompleteJob is already outstanding — don't re-complete.
        }
        if let Some(result_hash) = c.have_result {
            actions.push(ExecutorAction::Complete { job_id: c.job_id, result_hash });
        }
    }

    // ---- CLAIM: pick affordable open jobs up to the remaining concurrency budget. ----
    // Concurrency load = distinct jobs we are already working (non-Settled claims ∪ in-flight).
    let claimed_ids: HashSet<[u8; 32]> = snap.my_claims.iter().map(|c| c.job_id).collect();
    let mut active: HashSet<[u8; 32]> = snap.in_flight.clone();
    for c in &snap.my_claims {
        if c.phase != ClaimPhase::Settled {
            active.insert(c.job_id);
        }
    }
    let mut remaining = snap.cfg.max_concurrent_claims.saturating_sub(active.len());

    // Running available balance so cumulative escrow across this tick stays within the wallet.
    let mut available = snap.my_balance;

    // Deterministic selection order: by job_id.
    let mut candidates: Vec<&OpenJob> = snap.open_jobs.iter().collect();
    candidates.sort_by(|a, b| a.job_id.cmp(&b.job_id));

    for job in candidates {
        if remaining == 0 {
            break;
        }
        if snap.in_flight.contains(&job.job_id) {
            continue; // a ClaimJob is already outstanding for this job.
        }
        if claimed_ids.contains(&job.job_id) {
            continue; // already claimed by us (defensive — such a job shouldn't be "open").
        }
        if now_height > job.claim_by {
            continue; // claim window closed; `apply_claim_job` would reject (state.rs:1873).
        }
        let escrow = job.budget.max(snap.cfg.executor_bond);
        // Affordable iff we can escrow AND still keep the reserve, without underflow.
        let needed = match escrow.checked_add(snap.cfg.min_balance_reserve) {
            Some(n) => n,
            None => continue, // pathological config/budget; treat as unaffordable.
        };
        if available < needed {
            continue; // unaffordable this tick (also guards against draining below reserve).
        }
        actions.push(ExecutorAction::Claim { job_id: job.job_id });
        available -= escrow; // debit only the escrowed amount; the reserve is a floor, not a spend.
        remaining -= 1;
    }

    // Total, stable ordering by (job_id, variant) — job_ids are unique across the two action kinds
    // in a well-formed snapshot, so this is a total order the caller can rely on.
    actions.sort_by(|a, b| {
        a.job_id()
            .cmp(&b.job_id())
            .then_with(|| a.variant_rank().cmp(&b.variant_rank()))
    });
    actions
}

/// Thin, SYNC, CPU-only shim over the frozen deterministic kernel `pouw_executor::execute_job`.
/// The loop runs this inside `spawn_blocking` (WASM is CPU-heavy — MUST NOT run on the event-loop
/// thread). Re-execution uses the passed `limits` VERBATIM (consensus-anchored — never `default()`;
/// divergent limits would slash an honest executor). Enforces the linchpin `sha256(program_bytes)
/// == program_hash` inside the kernel.
pub fn reexecute(
    program_hash: [u8; 32],
    input_hash: [u8; 32],
    program_bytes: &[u8],
    input: &[u8],
    limits: WasmLimits,
) -> Result<[u8; 32], ExecError> {
    crate::pouw_executor::execute_job(program_hash, input_hash, program_bytes, input, limits)
}

/// Encode a job's `program‖input` into ONE DA blob (founder Q1). Format:
/// `[program_len: u32 LE][program bytes][input bytes]`. The publisher calls this before chunking.
pub fn encode_job_blob(program: &[u8], input: &[u8]) -> Vec<u8> {
    debug_assert!(
        program.len() <= u32::MAX as usize,
        "program length must fit in the u32 length prefix"
    );
    let mut out = Vec::with_capacity(4 + program.len() + input.len());
    out.extend_from_slice(&(program.len() as u32).to_le_bytes());
    out.extend_from_slice(program);
    out.extend_from_slice(input);
    out
}

/// Decode a `program‖input` DA blob produced by [`encode_job_blob`]. Returns `(program, input)`
/// slices, or `None` if the blob is truncated (< 4-byte prefix) or the declared program length
/// overruns the buffer. Zero-copy: the returned slices borrow `blob`.
pub fn split_job_blob(blob: &[u8]) -> Option<(&[u8], &[u8])> {
    if blob.len() < 4 {
        return None;
    }
    let program_len = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    let rest = &blob[4..];
    if program_len > rest.len() {
        return None; // declared length overruns the blob → malformed.
    }
    Some((&rest[..program_len], &rest[program_len..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn cfg(max_concurrent: usize, reserve: u64, bond: u64) -> ExecutorCfg {
        ExecutorCfg {
            max_concurrent_claims: max_concurrent,
            min_balance_reserve: reserve,
            executor_bond: bond,
        }
    }

    fn open(job_id: u8, budget: u64, claim_by: u64) -> OpenJob {
        OpenJob {
            job_id: [job_id; 32],
            budget,
            program_hash: [0; 32],
            input_hash: [0; 32],
            da_root: [0; 32],
            claim_by,
        }
    }

    fn snap(open_jobs: Vec<OpenJob>, my_claims: Vec<MyClaim>, balance: u64, cfg: ExecutorCfg) -> ExecutorSnapshot {
        ExecutorSnapshot {
            open_jobs,
            my_claims,
            my_address: Address([9; 32]),
            my_balance: balance,
            in_flight: HashSet::new(),
            cfg,
        }
    }

    /// An affordable, in-window, un-claimed open job → exactly one `Claim`.
    #[test]
    fn claims_an_affordable_open_job() {
        let s = snap(vec![open(1, 100, 50)], vec![], 1_000, cfg(4, 10, 50));
        let acts = plan_executor_actions(10, &s);
        assert_eq!(acts, vec![ExecutorAction::Claim { job_id: [1; 32] }]);
    }

    /// A job already in `in_flight` is NOT re-claimed (idempotency).
    #[test]
    fn does_not_reclaim_in_flight() {
        let mut s = snap(vec![open(1, 100, 50)], vec![], 1_000, cfg(4, 10, 50));
        s.in_flight.insert([1; 32]);
        assert!(plan_executor_actions(10, &s).is_empty());
    }

    /// A job already among `my_claims` is not re-claimed (double-claim guard, state.rs:1853).
    #[test]
    fn does_not_reclaim_already_claimed() {
        let claim = MyClaim { job_id: [1; 32], phase: ClaimPhase::AwaitingResult, result_by: 100, have_result: None };
        let s = snap(vec![open(1, 100, 50)], vec![claim], 1_000, cfg(4, 10, 50));
        // No Claim (already claimed); no Complete (have_result is None).
        assert!(plan_executor_actions(10, &s).is_empty());
    }

    /// Balance below `max(budget,bond) + reserve` → the job is NOT claimed (unaffordable).
    #[test]
    fn does_not_claim_unaffordable() {
        // budget 100, bond 50 → escrow 100; reserve 10 → need 110; balance 105 < 110.
        let s = snap(vec![open(1, 100, 50)], vec![], 105, cfg(4, 10, 50));
        assert!(plan_executor_actions(10, &s).is_empty());

        // budget 20, bond 50 → escrow uses the bond floor (50); reserve 10 → need 60; balance 59.
        let s2 = snap(vec![open(1, 20, 50)], vec![], 59, cfg(4, 10, 50));
        assert!(plan_executor_actions(10, &s2).is_empty());
        // one more coin makes it affordable.
        let s3 = snap(vec![open(1, 20, 50)], vec![], 60, cfg(4, 10, 50));
        assert_eq!(plan_executor_actions(10, &s3), vec![ExecutorAction::Claim { job_id: [1; 32] }]);
    }

    /// Past the claim window → not claimed (`now_height > claim_by`).
    #[test]
    fn does_not_claim_past_window() {
        let s = snap(vec![open(1, 100, 50)], vec![], 1_000, cfg(4, 10, 50));
        assert!(plan_executor_actions(51, &s).is_empty()); // 51 > claim_by 50
        // exactly at the deadline is still admissible.
        assert_eq!(plan_executor_actions(50, &s), vec![ExecutorAction::Claim { job_id: [1; 32] }]);
    }

    /// Complete only when `have_result` is Some, phase is AwaitingResult, and window is open.
    #[test]
    fn completes_only_with_result_in_phase_and_window() {
        let rh = [7u8; 32];
        // have_result None → no Complete.
        let c_none = MyClaim { job_id: [1; 32], phase: ClaimPhase::AwaitingResult, result_by: 100, have_result: None };
        assert!(plan_executor_actions(10, &snap(vec![], vec![c_none], 0, cfg(4, 10, 50))).is_empty());

        // have_result Some, in phase, in window → Complete.
        let c_ok = MyClaim { job_id: [1; 32], phase: ClaimPhase::AwaitingResult, result_by: 100, have_result: Some(rh) };
        assert_eq!(
            plan_executor_actions(10, &snap(vec![], vec![c_ok.clone()], 0, cfg(4, 10, 50))),
            vec![ExecutorAction::Complete { job_id: [1; 32], result_hash: rh }]
        );

        // wrong phase → no Complete (committee already drawn).
        let c_phase = MyClaim { job_id: [1; 32], phase: ClaimPhase::Committing, result_by: 100, have_result: Some(rh) };
        assert!(plan_executor_actions(10, &snap(vec![], vec![c_phase], 0, cfg(4, 10, 50))).is_empty());

        // past result_by → no Complete.
        assert!(plan_executor_actions(101, &snap(vec![], vec![c_ok.clone()], 0, cfg(4, 10, 50))).is_empty());

        // already in-flight → don't re-complete.
        let mut s = snap(vec![], vec![c_ok], 0, cfg(4, 10, 50));
        s.in_flight.insert([1; 32]);
        assert!(plan_executor_actions(10, &s).is_empty());
    }

    /// `max_concurrent_claims` caps new claims, counting existing non-settled claims + in-flight.
    #[test]
    fn respects_max_concurrent() {
        // Three affordable open jobs but concurrency cap 2 → only the 2 lowest job_ids claimed.
        let jobs = vec![open(3, 100, 50), open(1, 100, 50), open(2, 100, 50)];
        let s = snap(jobs, vec![], 10_000, cfg(2, 0, 50));
        let acts = plan_executor_actions(10, &s);
        assert_eq!(
            acts,
            vec![
                ExecutorAction::Claim { job_id: [1; 32] },
                ExecutorAction::Claim { job_id: [2; 32] },
            ]
        );

        // One non-settled claim already occupies a slot → cap 2 leaves room for exactly 1 more.
        let existing = MyClaim { job_id: [9; 32], phase: ClaimPhase::AwaitingResult, result_by: 100, have_result: None };
        let jobs = vec![open(1, 100, 50), open(2, 100, 50)];
        let s = snap(jobs, vec![existing], 10_000, cfg(2, 0, 50));
        let claims: Vec<_> = plan_executor_actions(10, &s)
            .into_iter()
            .filter(|a| matches!(a, ExecutorAction::Claim { .. }))
            .collect();
        assert_eq!(claims, vec![ExecutorAction::Claim { job_id: [1; 32] }]);
    }

    /// Cumulative affordability: two jobs each affordable alone, but the wallet can only fund one.
    #[test]
    fn cumulative_affordability_across_tick() {
        // escrow 100 each, reserve 0, balance 150 → first claim ok (avail→50), second needs 100 > 50.
        let jobs = vec![open(1, 100, 50), open(2, 100, 50)];
        let s = snap(jobs, vec![], 150, cfg(5, 0, 50));
        assert_eq!(plan_executor_actions(10, &s), vec![ExecutorAction::Claim { job_id: [1; 32] }]);
    }

    /// A settled claim does NOT occupy a concurrency slot.
    #[test]
    fn settled_claim_frees_concurrency() {
        let settled = MyClaim { job_id: [9; 32], phase: ClaimPhase::Settled, result_by: 100, have_result: None };
        let s = snap(vec![open(1, 100, 50)], vec![settled], 10_000, cfg(1, 0, 50));
        // cap is 1 and the only existing claim is Settled → still room to claim job 1.
        assert_eq!(plan_executor_actions(10, &s), vec![ExecutorAction::Claim { job_id: [1; 32] }]);
    }

    /// The `program‖input` envelope round-trips, including empty program / empty input edge cases.
    #[test]
    fn encode_split_round_trips() {
        for (p, i) in [
            (&b"program-bytes"[..], &b"input-bytes"[..]),
            (&b""[..], &b"only-input"[..]),
            (&b"only-program"[..], &b""[..]),
            (&b""[..], &b""[..]),
        ] {
            let blob = encode_job_blob(p, i);
            let (dp, di) = split_job_blob(&blob).expect("well-formed blob splits");
            assert_eq!(dp, p);
            assert_eq!(di, i);
        }
    }

    /// Malformed blobs are rejected (not panicked on): too short, and an over-long declared prefix.
    #[test]
    fn split_rejects_malformed() {
        assert!(split_job_blob(&[]).is_none());
        assert!(split_job_blob(&[0, 1, 2]).is_none()); // < 4-byte prefix
        // prefix claims 100-byte program but only 1 byte follows.
        let mut bad = 100u32.to_le_bytes().to_vec();
        bad.push(0xAB);
        assert!(split_job_blob(&bad).is_none());
    }

    /// The re-execution shim reproduces `execute_job` on a known program (a doubler), and refuses
    /// program bytes that don't hash to `program_hash` (the linchpin propagates through the shim).
    #[test]
    fn reexecute_matches_execute_job() {
        const DOUBLER: &str = r#"(module
            (memory (export "memory") 1 1)
            (global $next (mut i32) (i32.const 1024))
            (func $alloc (export "alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $next))
                (global.set $next (i32.add (global.get $next) (local.get $len)))
                (local.get $ptr))
            (func (export "run") (param $ptr i32) (param $len i32) (result i64)
                (local $out i32) (local $i i32)
                (local.set $out (call $alloc (local.get $len)))
                (block $done (loop $loop
                    (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
                    (i32.store8
                        (i32.add (local.get $out) (local.get $i))
                        (i32.mul (i32.const 2)
                            (i32.load8_u (i32.add (local.get $ptr) (local.get $i)))))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $loop)))
                (i64.or
                    (i64.shl (i64.extend_i32_u (local.get $out)) (i64.const 32))
                    (i64.extend_i32_u (local.get $len))))
        )"#;
        let wasm = wat::parse_str(DOUBLER).expect("guest assembles");
        let program_hash: [u8; 32] = Sha256::digest(&wasm).into();
        let input = vec![1u8, 2, 3, 40];
        let input_hash: [u8; 32] = Sha256::digest(&input).into();

        let via_shim = reexecute(program_hash, input_hash, &wasm, &input, WasmLimits::default())
            .expect("valid program executes");
        let via_kernel =
            crate::pouw_executor::execute_job(program_hash, input_hash, &wasm, &input, WasmLimits::default())
                .expect("valid program executes");
        assert_eq!(via_shim, via_kernel, "shim must reproduce the kernel result_hash byte-for-byte");

        // Linchpin propagates: wrong bytes → ProgramHashMismatch, not execution.
        let wrong = b"\x00asm\x01\x00\x00\x00 not the program".to_vec();
        assert!(matches!(
            reexecute(program_hash, input_hash, &wrong, &input, WasmLimits::default()),
            Err(ExecError::ProgramHashMismatch { .. })
        ));
    }
}
