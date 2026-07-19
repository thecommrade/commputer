//! executor_loop.rs — the OFF-THREAD driver of the PoUW executor auto-claim loop
//! (Track-2 Phase A). It turns the PURE, inert `executor_planner` decisions into a live
//! `ClaimJob`/`CompleteJob` stream: coalesce to the newest applied-state view, DA-fetch +
//! re-execute each claimed job on THIS dedicated thread (never the event loop), and emit
//! nonce-free `TxKind`s over a shared sender the PROTECTED event loop drains and signs.
//!
//! WHAT (three concerns):
//!   1. The shared DA seams — `AttestationSource` (bare on-chain `da_root` -> `DaAttestation`)
//!      and `BlobFetcher` (a resolved attestation -> reconstructed `program‖input` blob). Both
//!      are traits so tests inject in-process resolvers/fetchers; the production impls live in
//!      `da_attestation` (`DaBackedAttestationSource` + the generic `BridgeBlobFetcher<T>`), while
//!      `NoAttestationSource` (resolve -> None -> inert Abstain, open-Q15) is the inert default. The
//!      verifier loop RE-USES these two traits (it imports them from here).
//!   2. `ExecutorChainView` — the per-block snapshot the PROTECTED event loop builds from
//!      `ChainState` (open pending jobs, this node's claims + their execution metadata, address,
//!      spendable balance) and sends over a `std::sync::mpsc` channel.
//!   3. `run(..)` — the blocking receive loop. Coalesces backlogged views to the newest, tracks
//!      in-flight Claim/Complete broadcasts + a re-executed-result cache, calls the pure
//!      `plan_executor_actions`, and emits `TxKind::ClaimJob`/`CompleteJob`. WASM re-execution +
//!      the DA fetch run on this thread only.
//!
//! WHERE THIS IS WIRED IN (later, PROTECTED — NOT wired now; this module is inert):
//!   * `main.rs` (PROTECTED): `[executor] enabled=false`; when on, spawn `run` on a dedicated OS
//!     thread with a `da_attestation::BridgeBlobFetcher` + `NoAttestationSource`, the shared actor-tx sender, and a
//!     `std::sync::mpsc::Receiver<ExecutorChainView>`.
//!   * `event_loop.rs` (PROTECTED): each applied block builds an `ExecutorChainView` from
//!     `self.state` and `send`s it; the single `emit_actor_tx` sink drains the `TxKind`s, assigns
//!     the wallet nonce, signs, and admits them via the normal mempool path.
//! FILES NEEDING CHANGES for the live wire-in: `main.rs`, `event_loop.rs`, `config.rs` (all
//! PROTECTED, founder-gated) + `pub mod executor_loop;` already added to `lib.rs`.

// Inert until the PROTECTED wire-in: no in-tree spawner of the loop yet.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use commputer_core::identity::Address;
use commputer_core::transaction::TxKind;
use commputer_da::params::DaAttestation;
use commputer_pouw::wasm::WasmLimits;
use commputer_pouw_onchain::lifecycle::{JobLifecycle, PhaseRec};
use commputer_storage::state::PendingJobRecord;
use tokio::sync::mpsc::UnboundedSender;

use crate::executor_planner::{
    plan_executor_actions, reexecute, split_job_blob, ClaimPhase, ExecutorAction, ExecutorCfg,
    ExecutorSnapshot, MyClaim, OpenJob,
};

// ── Default DA fetch bounds (only affect fetch liveness; the frozen facade degrades to Abstain). ──
/// Retry-window budget (ms/ticks) a single `fetch_blob` gets before it Abstains. Sized against the
/// REAL deadlines it must fit inside: the on-chain commit window is ≈20s (default
/// `PhaseWindows::commit_blocks` = 10 blocks x `genesis.block_time_secs` = 2s;
/// `consensus_params.rs`), and this 30s budget is itself the outer bound the go-live Task B
/// client-side pacing (`da_transport::BridgeTransport::with_min_fetch_interval`, default 150ms)
/// must fit inside: `SAMPLES_PER_VERIFIER` (16) x 150ms = 2.4s of pacing, negligible against both
/// windows. Per-miss cost is asymmetric: a miss against a connected non-holder costs about one RTT
/// (the loop just tries the next provider), while a miss against a dead/unreachable peer costs up
/// to the per-call `fetch_timeout` (`node_config.da.fetch_timeout_ms`, plumbed into
/// `BridgeTransport::with_timeout` in `main.rs`) before the bridge falls back to the unavailable
/// default. NOT changed by go-live Task B — cited here only so the interplay is legible in one
/// place.
pub const DEFAULT_DA_RETRY_WINDOW_TICKS: u64 = 30_000;
/// Max provider attempts per sampled chunk. `find_providers` returns ALL connected peers (no
/// discovery cost); the frozen facade XOR-sorts that list by distance-to-target and tries up to
/// this many before giving up on a sampled chunk. Only the publisher actually HOLDS a chunk today
/// (no re-seeding/replication yet), so a cap smaller than the connected-peer count silently drops
/// the publisher off the tried list once peer count grows past it — hit-rate collapses toward
/// (old cap)/P and `SAMPLES_PER_VERIFIER` (16) sampled chunks per verification starve. Attempts
/// beyond the ACTUAL provider list length are free (the facade loop simply ends when providers run
/// out), so there is no cost to sizing this generously: raised 8 -> 64 (go-live Task B) to cover a
/// real alpha-testnet's full connected-peer set with headroom, comfortably exceeding the 16-sample
/// workload it must not starve.
pub const DEFAULT_DA_MAX_ATTEMPTS_PER_CHUNK: u32 = 64;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Shared DA seams (re-used by verifier_loop).
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Resolve a bare on-chain `da_root` into the full `DaAttestation` (`program_id`, `n_data`,
/// `n_total`, `data_len`, …) a fetch needs. No such distribution channel exists yet (open-Q15),
/// so the production impl is [`NoAttestationSource`] → every resolve is `None` → the loop Abstains
/// (fetches nothing, completes nothing) → jobs refund. Tests inject an in-process resolver.
pub trait AttestationSource {
    fn resolve(&self, da_root: [u8; 32]) -> Option<DaAttestation>;
}

/// The inert production resolver: never resolves, so the loop never fetches/completes (open-Q15).
pub struct NoAttestationSource;

impl AttestationSource for NoAttestationSource {
    fn resolve(&self, _da_root: [u8; 32]) -> Option<DaAttestation> {
        None
    }
}

// Forward through a box so the PROTECTED spawn can hand `Box<dyn AttestationSource + Send>` to
// `run`'s `impl AttestationSource` parameter.
impl<T: AttestationSource + ?Sized> AttestationSource for Box<T> {
    fn resolve(&self, da_root: [u8; 32]) -> Option<DaAttestation> {
        (**self).resolve(da_root)
    }
}

/// Fetch + reconstruct the `program‖input` blob for a resolved attestation via the DA facade.
/// Returns `None` on any unavailability (the caller then retries next tick or Abstains). A trait so
/// tests inject an in-process fetcher instead of a live swarm.
pub trait BlobFetcher {
    fn fetch_blob(&self, att: &DaAttestation) -> Option<Vec<u8>>;
}

impl<T: BlobFetcher + ?Sized> BlobFetcher for Box<T> {
    fn fetch_blob(&self, att: &DaAttestation) -> Option<Vec<u8>> {
        (**self).fetch_blob(att)
    }
}

// The production `BlobFetcher` (and its `AttestationSource` companion) is the generic
// `da_attestation::BridgeBlobFetcher<T>` / `DaBackedAttestationSource<T>`, which supersedes the
// concrete fetcher that once lived here; both re-use the two traits above.

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The per-block view + the loop.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// One of this node's claimed jobs, with the execution metadata the loop needs to re-execute it.
/// (The pure planner's `MyClaim` drops these fields; the loop carries them so a resumed node can
/// re-execute a claim it no longer remembers publishing.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorClaimView {
    pub job_id: [u8; 32],
    pub phase: ClaimPhase,
    /// Last height at which a `CompleteJob` is admissible.
    pub result_by: u64,
    pub program_hash: [u8; 32],
    pub input_hash: [u8; 32],
    /// DA anchor for the `program‖input` blob.
    pub da_root: [u8; 32],
}

/// The applied-state snapshot the PROTECTED event loop builds each block and sends to the loop.
#[derive(Debug, Clone)]
pub struct ExecutorChainView {
    pub height: u64,
    pub open_jobs: Vec<OpenJob>,
    pub my_claims: Vec<ExecutorClaimView>,
    pub my_address: Address,
    /// Spendable balance of the executor wallet.
    pub my_balance: u64,
}

/// Map the on-chain lifecycle phase (record form) to the executor loop's [`ClaimPhase`]. The two
/// enums are 1:1 — a `CompleteJob` is only admissible at `AwaitingResult` (the planner enforces it).
fn record_phase_to_claim(p: PhaseRec) -> ClaimPhase {
    match p {
        PhaseRec::AwaitingResult => ClaimPhase::AwaitingResult,
        PhaseRec::Committing => ClaimPhase::Committing,
        PhaseRec::Revealing => ClaimPhase::Revealing,
        PhaseRec::Settled => ClaimPhase::Settled,
    }
}

/// Build the executor's per-block [`ExecutorChainView`] from the just-applied `ChainState` maps.
/// Called by the PROTECTED event loop each block (with `self.state.pending_jobs`,
/// `self.state.job_lifecycles`, the wallet address + spendable balance) and `send`s the result to
/// [`run`]. PURE over its inputs and DETERMINISTIC: no wall-clock, no rng; `open_jobs` and
/// `my_claims` are sorted by `job_id` so the produced view is byte-stable for a given state.
///
/// `open_jobs` mirrors every entry of `pending_jobs` (the unclaimed jobs). `my_claims` is every
/// lifecycle this node is the executor of (`executor == me`), projected via `JobLifecycle::to_record`
/// (the inner fields are private) — carrying the execution metadata the loop needs to re-execute a
/// claim it may no longer remember publishing. `_epoch` is accepted for call-site symmetry with the
/// verifier tick but is not part of the executor snapshot.
pub fn build_chain_view(
    height: u64,
    _epoch: u64,
    me: Address,
    my_balance: u64,
    pending_jobs: &HashMap<[u8; 32], PendingJobRecord>,
    job_lifecycles: &HashMap<[u8; 32], JobLifecycle>,
) -> ExecutorChainView {
    // Every pending (unclaimed) job → an OpenJob.
    let mut open_jobs: Vec<OpenJob> = pending_jobs
        .iter()
        .map(|(job_id, rec)| OpenJob {
            job_id: *job_id,
            budget: rec.budget,
            program_hash: rec.program_hash,
            input_hash: rec.input_hash,
            da_root: rec.da_root,
            claim_by: rec.claim_by,
        })
        .collect();
    open_jobs.sort_by(|a, b| a.job_id.cmp(&b.job_id));

    // Every lifecycle this node is the executor of → an ExecutorClaimView. `ParticipantId(addr.0)`
    // is the on-chain identity (state.rs), so the record's `executor` bytes equal `me.0`.
    let mut my_claims: Vec<ExecutorClaimView> = job_lifecycles
        .iter()
        .filter_map(|(job_id, lc)| {
            let rec = lc.to_record();
            if rec.executor != me.0 {
                return None;
            }
            Some(ExecutorClaimView {
                job_id: *job_id,
                phase: record_phase_to_claim(rec.phase),
                result_by: rec.deadlines.result_by,
                program_hash: rec.program_hash,
                input_hash: rec.input_hash,
                da_root: rec.da_root,
            })
        })
        .collect();
    my_claims.sort_by(|a, b| a.job_id.cmp(&b.job_id));

    ExecutorChainView {
        height,
        open_jobs,
        my_claims,
        my_address: me,
        my_balance,
    }
}

/// Returned when the shared actor-tx receiver is gone (the event loop dropped it) → the loop exits.
#[derive(Debug)]
struct LoopGone;

/// The stateful driver. Holds the re-executed-result cache + in-flight broadcast tracking across
/// ticks so it never double-claims / double-completes and never re-fetches a computed result.
struct ExecutorLoop<F: BlobFetcher, A: AttestationSource> {
    cfg: ExecutorCfg,
    wasm_limits: WasmLimits,
    fetcher: F,
    atts: A,
    /// job_id -> re-executed result_hash (computed on this thread; injected as `have_result`).
    results: HashMap<[u8; 32], [u8; 32]>,
    /// job_ids we broadcast a `ClaimJob` for, not yet reflected in `my_claims`.
    claimed_broadcast: HashSet<[u8; 32]>,
    /// job_ids we broadcast a `CompleteJob` for, whose claim is still `AwaitingResult`.
    completed_broadcast: HashSet<[u8; 32]>,
}

impl<F: BlobFetcher, A: AttestationSource> ExecutorLoop<F, A> {
    fn new(cfg: ExecutorCfg, wasm_limits: WasmLimits, fetcher: F, atts: A) -> Self {
        Self {
            cfg,
            wasm_limits,
            fetcher,
            atts,
            results: HashMap::new(),
            claimed_broadcast: HashSet::new(),
            completed_broadcast: HashSet::new(),
        }
    }

    /// Process one (already-coalesced-to-newest) view: reconcile in-flight state, do the heavy
    /// DA-fetch + re-execute for completable claims, plan, and emit. Returns `Err(LoopGone)` iff
    /// the actor-tx receiver is gone.
    fn process(
        &mut self,
        view: &ExecutorChainView,
        action_tx: &UnboundedSender<TxKind>,
    ) -> Result<(), LoopGone> {
        let open_ids: HashSet<[u8; 32]> = view.open_jobs.iter().map(|j| j.job_id).collect();
        let claim_ids: HashSet<[u8; 32]> = view.my_claims.iter().map(|c| c.job_id).collect();
        let awaiting_ids: HashSet<[u8; 32]> = view
            .my_claims
            .iter()
            .filter(|c| c.phase == ClaimPhase::AwaitingResult)
            .map(|c| c.job_id)
            .collect();

        // Reconcile against freshly applied state:
        //  * a claim broadcast is "reflected" once the job leaves `open_jobs` into `my_claims`
        //    (or leaves both — claimed by someone else / window closed);
        //  * a complete broadcast is "reflected" once the claim advances past `AwaitingResult`;
        //  * a cached result is stale once its job is no longer among `my_claims`.
        self.claimed_broadcast
            .retain(|jid| open_ids.contains(jid) && !claim_ids.contains(jid));
        self.completed_broadcast.retain(|jid| awaiting_ids.contains(jid));
        self.results.retain(|jid, _| claim_ids.contains(jid));

        // Heavy work (this thread ONLY): for each completable claim we don't yet have a result for,
        // resolve the attestation → fetch the blob → split → re-execute. Any miss just retries next
        // tick (NoAttestationSource makes this a no-op → inert).
        for c in &view.my_claims {
            if c.phase != ClaimPhase::AwaitingResult {
                continue;
            }
            if view.height > c.result_by {
                continue; // window closed; a CompleteJob would be rejected.
            }
            if self.results.contains_key(&c.job_id) {
                continue;
            }
            if self.completed_broadcast.contains(&c.job_id) {
                continue; // already emitted a Complete; awaiting reflection.
            }
            let Some(att) = self.atts.resolve(c.da_root) else {
                continue;
            };
            let Some(blob) = self.fetcher.fetch_blob(&att) else {
                continue; // DA unavailable this tick → retry next block.
            };
            let Some((program, input)) = split_job_blob(&blob) else {
                continue; // malformed envelope → skip.
            };
            // P1: WasmLimits is Clone-not-Copy and `reexecute` takes it by value → clone per claim
            // (value-identical; no determinism effect).
            match reexecute(c.program_hash, c.input_hash, program, input, self.wasm_limits.clone()) {
                Ok(result_hash) => {
                    self.results.insert(c.job_id, result_hash);
                }
                Err(_e) => { /* garbled bytes → linchpin rejects; retry next block */ }
            }
        }

        // Build the pure snapshot, injecting our computed results + in-flight guard.
        let my_claims: Vec<MyClaim> = view
            .my_claims
            .iter()
            .map(|c| MyClaim {
                job_id: c.job_id,
                phase: c.phase,
                result_by: c.result_by,
                have_result: self.results.get(&c.job_id).copied(),
            })
            .collect();
        let mut in_flight: HashSet<[u8; 32]> = self.claimed_broadcast.clone();
        in_flight.extend(self.completed_broadcast.iter().copied());
        let snap = ExecutorSnapshot {
            open_jobs: view.open_jobs.clone(),
            my_claims,
            my_address: view.my_address,
            my_balance: view.my_balance,
            in_flight,
            cfg: self.cfg,
        };

        // P4: emit nonce-free TxKind over the shared sender; the event loop is the sole nonce owner.
        for a in plan_executor_actions(view.height, &snap) {
            let kind = match a {
                ExecutorAction::Claim { job_id } => {
                    self.claimed_broadcast.insert(job_id);
                    TxKind::ClaimJob { job_id }
                }
                ExecutorAction::Complete { job_id, result_hash } => {
                    self.completed_broadcast.insert(job_id);
                    TxKind::CompleteJob { job_id, result_hash }
                }
            };
            if action_tx.send(kind).is_err() {
                return Err(LoopGone); // event loop dropped the receiver → shut down.
            }
        }
        Ok(())
    }
}

/// Blocking receive loop for the executor auto-claim driver — run on a DEDICATED OS thread (WASM
/// re-execution + DA fetch are CPU/latency-heavy and must never touch the event-loop task). It
/// coalesces backlogged snapshots to the newest applied state (so it never acts on a stale
/// `now_height`) and drives [`ExecutorLoop::process`]. Returns when the snapshot channel closes or
/// the actor-tx receiver is gone.
pub fn run(
    cfg: ExecutorCfg,
    wasm_limits: WasmLimits,
    fetcher: impl BlobFetcher,
    atts: impl AttestationSource,
    snapshot_rx: std::sync::mpsc::Receiver<ExecutorChainView>,
    action_tx: UnboundedSender<TxKind>,
) {
    let mut lp = ExecutorLoop::new(cfg, wasm_limits, fetcher, atts);
    while let Ok(view) = snapshot_rx.recv() {
        // Coalesce: work only the newest applied-state view; drop backlog.
        let mut view = view;
        while let Ok(newer) = snapshot_rx.try_recv() {
            view = newer;
        }
        if lp.process(&view, &action_tx).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    use crate::executor_planner::encode_job_blob;

    /// A known-good WASM guest that doubles each input byte (mod 256) — shared with the planner
    /// determinism tests; a program `execute_job`/`reexecute` accepts.
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

    /// A test attestation source that resolves ANY da_root to a dummy attestation (the in-process
    /// fetcher ignores the attestation's fields).
    struct AnyAttestation;
    impl AttestationSource for AnyAttestation {
        fn resolve(&self, da_root: [u8; 32]) -> Option<DaAttestation> {
            Some(DaAttestation {
                program_id: [0u8; 32],
                da_root,
                data_len: 0,
                chunk_size: 65_536,
                n_data: 1,
                n_total: 2,
                params_version: 1,
            })
        }
    }

    /// An in-process fetcher that always returns a fixed envelope (bypasses the swarm).
    struct FixedBlob(Vec<u8>);
    impl BlobFetcher for FixedBlob {
        fn fetch_blob(&self, _att: &DaAttestation) -> Option<Vec<u8>> {
            Some(self.0.clone())
        }
    }

    fn program_input() -> (Vec<u8>, Vec<u8>) {
        (
            wat::parse_str(DOUBLER).expect("guest assembles"),
            vec![1u8, 2, 3, 40, 7],
        )
    }

    fn hashes(program: &[u8], input: &[u8]) -> ([u8; 32], [u8; 32]) {
        (Sha256::digest(program).into(), Sha256::digest(input).into())
    }

    fn cfg() -> ExecutorCfg {
        ExecutorCfg {
            max_concurrent_claims: 4,
            min_balance_reserve: 0,
            executor_bond: 50,
        }
    }

    fn open_job(id: u8, ph: [u8; 32], ih: [u8; 32]) -> OpenJob {
        OpenJob {
            job_id: [id; 32],
            budget: 100,
            program_hash: ph,
            input_hash: ih,
            da_root: [id; 32],
            claim_by: 1_000,
        }
    }

    fn claim_view(id: u8, ph: [u8; 32], ih: [u8; 32]) -> ExecutorClaimView {
        ExecutorClaimView {
            job_id: [id; 32],
            phase: ClaimPhase::AwaitingResult,
            result_by: 1_000,
            program_hash: ph,
            input_hash: ih,
            da_root: [id; 32],
        }
    }

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<TxKind>) -> Vec<TxKind> {
        let mut out = Vec::new();
        while let Ok(k) = rx.try_recv() {
            out.push(k);
        }
        out
    }

    /// The core happy path: an open job → exactly one Claim; then, once claimed, the loop fetches
    /// the blob, re-executes, and emits exactly one Complete carrying the real result_hash.
    #[test]
    fn emits_claim_then_complete_with_real_result() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);
        let expected_rh =
            reexecute(ph, ih, &program, &input, WasmLimits::default()).expect("executes");

        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        let mut lp = ExecutorLoop::new(cfg(), WasmLimits::default(), FixedBlob(blob), AnyAttestation);

        // Tick 1: the job is open → Claim it.
        let view_open = ExecutorChainView {
            height: 10,
            open_jobs: vec![open_job(1, ph, ih)],
            my_claims: vec![],
            my_address: Address([9; 32]),
            my_balance: 10_000,
        };
        lp.process(&view_open, &atx).unwrap();
        let out = drain(&mut arx);
        assert_eq!(out.len(), 1, "one Claim");
        assert!(matches!(out[0], TxKind::ClaimJob { job_id } if job_id == [1; 32]));

        // Tick 2: the claim landed → fetch + re-execute + Complete.
        let view_claimed = ExecutorChainView {
            height: 11,
            open_jobs: vec![],
            my_claims: vec![claim_view(1, ph, ih)],
            my_address: Address([9; 32]),
            my_balance: 9_900,
        };
        lp.process(&view_claimed, &atx).unwrap();
        let out = drain(&mut arx);
        assert_eq!(out.len(), 1, "one Complete");
        assert!(matches!(
            out[0],
            TxKind::CompleteJob { job_id, result_hash }
                if job_id == [1; 32] && result_hash == expected_rh
        ));
    }

    /// Idempotency: re-processing the SAME open view does not re-Claim (in-flight guard), and
    /// re-processing the SAME claimed view does not re-Complete.
    #[test]
    fn does_not_double_claim_or_double_complete() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);

        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        let mut lp = ExecutorLoop::new(cfg(), WasmLimits::default(), FixedBlob(blob), AnyAttestation);

        let view_open = ExecutorChainView {
            height: 10,
            open_jobs: vec![open_job(1, ph, ih)],
            my_claims: vec![],
            my_address: Address([9; 32]),
            my_balance: 10_000,
        };
        lp.process(&view_open, &atx).unwrap();
        assert_eq!(drain(&mut arx).len(), 1, "one Claim on first sight");
        // Same view again — the claim is still in flight (not yet in my_claims) → no re-Claim.
        lp.process(&view_open, &atx).unwrap();
        assert!(drain(&mut arx).is_empty(), "no re-Claim while in flight");

        let view_claimed = ExecutorChainView {
            height: 11,
            open_jobs: vec![],
            my_claims: vec![claim_view(1, ph, ih)],
            my_address: Address([9; 32]),
            my_balance: 9_900,
        };
        lp.process(&view_claimed, &atx).unwrap();
        assert_eq!(drain(&mut arx).len(), 1, "one Complete once claimed");
        // Same claimed view again — Complete still in flight (claim still AwaitingResult) → none.
        lp.process(&view_claimed, &atx).unwrap();
        assert!(drain(&mut arx).is_empty(), "no re-Complete while in flight");
    }

    /// With `NoAttestationSource` the loop is inert: it still Claims (a chain-admissible action) but
    /// can never fetch/re-execute, so it emits NO Complete even for a claimed job.
    #[test]
    fn no_attestation_source_never_completes() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);

        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        let mut lp =
            ExecutorLoop::new(cfg(), WasmLimits::default(), FixedBlob(blob), NoAttestationSource);

        let view_claimed = ExecutorChainView {
            height: 11,
            open_jobs: vec![],
            my_claims: vec![claim_view(1, ph, ih)],
            my_address: Address([9; 32]),
            my_balance: 9_900,
        };
        lp.process(&view_claimed, &atx).unwrap();
        assert!(
            drain(&mut arx).is_empty(),
            "NoAttestationSource → no result → no Complete (inert)"
        );
    }

    /// Coalescing: `run` drains a backlog to the newest view. An old in-window view alone would
    /// Claim; but with a newer view whose claim window has closed queued behind it, only the newest
    /// is processed — so nothing is emitted.
    #[test]
    fn run_coalesces_to_newest_view() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);

        // Baseline: the OLD view alone emits a Claim (non-vacuity).
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let old = ExecutorChainView {
                height: 10,
                open_jobs: vec![open_job(1, ph, ih)], // claim_by 1000, in window
                my_claims: vec![],
                my_address: Address([9; 32]),
                my_balance: 10_000,
            };
            tx.send(old).unwrap();
            drop(tx);
            let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
            run(cfg(), WasmLimits::default(), FixedBlob(blob.clone()), AnyAttestation, rx, atx);
            assert_eq!(drain(&mut arx).len(), 1, "old view alone Claims");
        }

        // Coalesced: OLD (in-window) then NEW (past claim_by) → only NEW processed → no Claim.
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let old = ExecutorChainView {
                height: 10,
                open_jobs: vec![open_job(1, ph, ih)],
                my_claims: vec![],
                my_address: Address([9; 32]),
                my_balance: 10_000,
            };
            let mut past = open_job(1, ph, ih);
            past.claim_by = 20; // closed at the new height
            let new = ExecutorChainView {
                height: 999,
                open_jobs: vec![past],
                my_claims: vec![],
                my_address: Address([9; 32]),
                my_balance: 10_000,
            };
            tx.send(old).unwrap();
            tx.send(new).unwrap();
            drop(tx);
            let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
            run(cfg(), WasmLimits::default(), FixedBlob(blob), AnyAttestation, rx, atx);
            assert!(
                drain(&mut arx).is_empty(),
                "coalesced to the newest (past-window) view → no Claim"
            );
        }
    }

    /// A dropped actor-tx receiver makes `run` exit cleanly (no panic/hang).
    #[test]
    fn run_exits_when_actor_receiver_gone() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(ExecutorChainView {
            height: 10,
            open_jobs: vec![open_job(1, ph, ih)],
            my_claims: vec![],
            my_address: Address([9; 32]),
            my_balance: 10_000,
        })
        .unwrap();
        // keep the snapshot sender alive so `run` would loop, but drop the actor receiver.
        let (atx, arx) = tokio::sync::mpsc::unbounded_channel::<TxKind>();
        drop(arx);
        run(cfg(), WasmLimits::default(), FixedBlob(blob), AnyAttestation, rx, atx);
        // returns (does not hang) because the emit failed → LoopGone.
    }

    // ── build_chain_view (on-chain → snapshot constructor) ──────────────────────────────────────
    use commputer_pouw::params::GameParams;
    use commputer_pouw_onchain::lifecycle::{JobLifecycleRecord, PhaseDeadlinesRec};
    use commputer_pouw_onchain::settlement_resolution::ResolutionParams;
    use std::collections::HashMap as StdHashMap;

    fn pending(job: u8, budget: u64, claim_by: u64) -> PendingJobRecord {
        PendingJobRecord {
            submitter: [0x11; 32],
            budget,
            program_hash: [job.wrapping_add(1); 32],
            input_hash: [job.wrapping_add(2); 32],
            da_root: [job.wrapping_add(3); 32],
            submitted_height: 1,
            claim_by,
        }
    }

    fn lifecycle_rec(job: u8, executor: [u8; 32], phase: PhaseRec) -> JobLifecycle {
        let rec = JobLifecycleRecord {
            job_id: [job; 32],
            program_hash: [job.wrapping_add(1); 32],
            input_hash: [job.wrapping_add(2); 32],
            da_root: [job.wrapping_add(3); 32],
            submitter: [0x11; 32],
            executor,
            executor_bond: 100,
            budget: 200,
            verifier_bond: 20,
            candidates: vec![],
            deadlines: PhaseDeadlinesRec { result_by: 500, commit_by: 600, reveal_by: 700 },
            phase,
            executor_hash: Some([0x99; 32]),
            committee: vec![],
            commitments: vec![],
            reveals: vec![],
            settled: None,
        };
        JobLifecycle::from_record(rec, GameParams::default(), ResolutionParams::default())
    }

    /// An open pending job appears in `open_jobs`; a lifecycle I execute appears in `my_claims`;
    /// a lifecycle someone ELSE executes is excluded. Fields + sort order are checked.
    #[test]
    fn build_chain_view_projects_open_jobs_and_my_claims() {
        let me = Address([0x42; 32]);
        let other = [0x77u8; 32];

        let mut pending_jobs = StdHashMap::new();
        pending_jobs.insert([2u8; 32], pending(2, 150, 900));
        pending_jobs.insert([1u8; 32], pending(1, 100, 800));

        let mut lifecycles = StdHashMap::new();
        lifecycles.insert([5u8; 32], lifecycle_rec(5, me.0, PhaseRec::Committing));
        lifecycles.insert([6u8; 32], lifecycle_rec(6, other, PhaseRec::AwaitingResult));

        let view = build_chain_view(123, 4, me, 10_000, &pending_jobs, &lifecycles);

        assert_eq!(view.height, 123);
        assert_eq!(view.my_address, me);
        assert_eq!(view.my_balance, 10_000);

        // open_jobs: both pending jobs, sorted by job_id, fields mirrored from PendingJobRecord.
        assert_eq!(view.open_jobs.len(), 2);
        assert_eq!(view.open_jobs[0].job_id, [1u8; 32]);
        assert_eq!(view.open_jobs[0].budget, 100);
        assert_eq!(view.open_jobs[0].claim_by, 800);
        assert_eq!(view.open_jobs[0].program_hash, [2u8; 32]); // job 1 → +1
        assert_eq!(view.open_jobs[0].da_root, [4u8; 32]); // job 1 → +3
        assert_eq!(view.open_jobs[1].job_id, [2u8; 32]);
        assert_eq!(view.open_jobs[1].budget, 150);

        // my_claims: ONLY the job I execute, with the mapped phase + result deadline + metadata.
        assert_eq!(view.my_claims.len(), 1, "the other-executor job is excluded");
        let c = &view.my_claims[0];
        assert_eq!(c.job_id, [5u8; 32]);
        assert_eq!(c.phase, ClaimPhase::Committing);
        assert_eq!(c.result_by, 500);
        assert_eq!(c.program_hash, [6u8; 32]); // job 5 → +1
        assert_eq!(c.input_hash, [7u8; 32]); // job 5 → +2
        assert_eq!(c.da_root, [8u8; 32]); // job 5 → +3
    }

    /// The four lifecycle phases map 1:1 to `ClaimPhase`.
    #[test]
    fn build_chain_view_maps_all_phases() {
        let me = Address([0x42; 32]);
        for (rec_phase, want) in [
            (PhaseRec::AwaitingResult, ClaimPhase::AwaitingResult),
            (PhaseRec::Committing, ClaimPhase::Committing),
            (PhaseRec::Revealing, ClaimPhase::Revealing),
            (PhaseRec::Settled, ClaimPhase::Settled),
        ] {
            let mut lifecycles = StdHashMap::new();
            lifecycles.insert([1u8; 32], lifecycle_rec(1, me.0, rec_phase));
            let view = build_chain_view(1, 0, me, 0, &StdHashMap::new(), &lifecycles);
            assert_eq!(view.my_claims.len(), 1);
            assert_eq!(view.my_claims[0].phase, want);
        }
    }

    /// Empty state → empty view (no panics, both vecs empty).
    #[test]
    fn build_chain_view_empty_state_is_empty() {
        let me = Address([0x42; 32]);
        let view = build_chain_view(7, 1, me, 500, &StdHashMap::new(), &StdHashMap::new());
        assert!(view.open_jobs.is_empty());
        assert!(view.my_claims.is_empty());
        assert_eq!(view.height, 7);
        assert_eq!(view.my_balance, 500);
    }

    /// Go-live Task B (const-sanity): the per-chunk provider-attempt ceiling was raised 8 -> 64.
    /// `find_providers` returns ALL connected peers; the frozen facade XOR-sorts that list by
    /// distance-to-target and tries up to this many before giving up on a sampled chunk. Only the
    /// publisher actually HOLDS a chunk today (no re-seeding/replication yet), so a cap that's
    /// smaller than the connected-peer count silently drops the publisher off the tried list past
    /// ~8 peers (hit-rate collapses toward 8/P) — starving `SAMPLES_PER_VERIFIER` (16) sampled
    /// chunks per verification. Attempts beyond the ACTUAL provider list length are free (the
    /// facade loop simply ends when providers run out), so there's no cost to sizing the ceiling
    /// generously; 64 covers a real alpha-testnet's full connected-peer set with headroom and, not
    /// incidentally, comfortably exceeds the 16-sample workload it must not starve.
    #[test]
    fn da_max_attempts_ceiling_is_64_and_exceeds_sample_count() {
        assert_eq!(
            DEFAULT_DA_MAX_ATTEMPTS_PER_CHUNK, 64,
            "ceiling raised 8 -> 64 (go-live Task B)"
        );
        assert!(
            (DEFAULT_DA_MAX_ATTEMPTS_PER_CHUNK as usize) > commputer_da::params::SAMPLES_PER_VERIFIER,
            "ceiling ({}) must exceed the {}-sample verification workload, else even a fully \
             connected mesh can starve a late-sorted holder",
            DEFAULT_DA_MAX_ATTEMPTS_PER_CHUNK,
            commputer_da::params::SAMPLES_PER_VERIFIER,
        );
    }
}
