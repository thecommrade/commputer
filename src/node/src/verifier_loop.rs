//! verifier_loop.rs — the OFF-THREAD commit/reveal driver of the PoUW verifier (Track-2 Phase A).
//! Turns the PURE, inert `verifier_planner` decisions into a live `Commit`/`Reveal` stream for a
//! bonded validator drawn onto a verification committee: coalesce to the newest applied-state tick,
//! recover-or-re-execute this node's own result_hash on THIS dedicated thread, durably persist the
//! commit salt BEFORE committing, and emit nonce-free `TxKind`s over the shared actor-tx sender.
//!
//! FUND-SAFETY (the two-step salt contract, `salt_store` + `verifier_planner`): the commitment is
//! `H(result_hash‖salt‖verifier)`; the salt MUST be fsynced to disk before the `Commit` broadcasts,
//! else a crash in the commit→reveal gap burns the bond. This loop honors that:
//!   * P3 — if `SaltStore::insert` returns `Err` (fsync failed), it clears the phantom in-memory
//!     entry (`remove`) so the planner cannot see a salt that isn't on disk, and does NOT commit.
//!   * P14 — on restart the in-memory result cache is empty; a persisted salt repopulates it so a
//!     resumed node can still commit/reveal within the window.
//! Salt = `rand::random()` — a node-local CSPRNG draw; the ONLY randomness in either actor loop and
//! NEVER a consensus input.
//!
//! WHERE THIS IS WIRED IN (later, PROTECTED — NOT wired now; inert): `main.rs` spawns
//! `run_verifier_loop` on a dedicated OS thread with a `BridgeBlobFetcher` + `NoAttestationSource`
//! (open-Q15 → Abstain → no commit), a node-local `SaltStore`, and the shared actor-tx sender;
//! `event_loop.rs` builds a `VerifierTick` from `job_lifecycles` each block and sends it. The DA
//! seams (`AttestationSource`/`BlobFetcher`) are RE-USED from `executor_loop`.
//! FILES NEEDING CHANGES for the live wire-in: `main.rs`, `event_loop.rs`, `config.rs` (all
//! PROTECTED, founder-gated) + `pub mod verifier_loop;` already added to `lib.rs`.

// Inert until the PROTECTED wire-in: no in-tree spawner of the loop yet.
#![allow(dead_code)]

use std::collections::HashMap;

use commputer_core::identity::Address;
use commputer_core::token::Amount;
use commputer_core::transaction::TxKind;
use commputer_pouw::wasm::WasmLimits;
use tokio::sync::mpsc::UnboundedSender;

use crate::executor_loop::{AttestationSource, BlobFetcher};
use crate::executor_planner::{reexecute, split_job_blob};
use crate::salt_store::SaltStore;
use crate::verifier_planner::{
    jobs_needing_salt, plan_verifier_actions, MyCommittee, VerifierAction, VerifierCfg,
    VerifierPhase, VerifierSnapshot,
};

/// Blocks between re-emits of the SAME (job, kind) actor tx — bridges the gap between broadcast and
/// on-chain reflection so we don't spam `Commit`/`Reveal` every block while one is in flight. Once
/// the chain records it (`already_committed`/`already_revealed`) the pure planner stops emitting.
const REEMIT_COOLDOWN_BLOCKS: u64 = 5;

/// Which of the two verifier actions a cooldown entry tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EmitKind {
    Commit,
    Reveal,
}

/// One committee this node was drawn onto, with the execution metadata the loop needs to re-execute
/// the job (the pure planner's `MyCommittee` drops these; the loop carries them + a `settled` flag
/// for salt GC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierCommitteeView {
    pub job_id: [u8; 32],
    pub phase: VerifierPhase,
    pub commit_by: u64,
    pub reveal_by: u64,
    pub verifier_bond: u64,
    pub already_committed: bool,
    pub already_revealed: bool,
    pub program_hash: [u8; 32],
    pub input_hash: [u8; 32],
    /// DA anchor for the `program‖input` blob.
    pub da_root: [u8; 32],
    /// The job has reached a terminal on-chain state → the salt secret is spent (GC it).
    pub settled: bool,
}

/// The applied-state snapshot the PROTECTED event loop builds each block and sends to the loop.
#[derive(Debug, Clone)]
pub struct VerifierTick {
    pub now_height: u64,
    pub committees: Vec<VerifierCommitteeView>,
    pub my_address: Address,
    pub my_balance: u64,
}

/// Returned when the shared actor-tx receiver is gone (the event loop dropped it) → the loop exits.
#[derive(Debug)]
struct LoopGone;

/// Stateful driver: the re-executed-result cache + the per-(job,kind) re-emit cooldown persist
/// across ticks.
struct VerifierLoop<F: BlobFetcher, A: AttestationSource> {
    cfg: VerifierCfg,
    wasm_limits: WasmLimits,
    fetcher: F,
    atts: A,
    /// job_id -> this node's OWN re-executed result_hash (never copied from an on-chain hash).
    results: HashMap<[u8; 32], [u8; 32]>,
    /// (job_id, kind) -> height we last emitted, for the re-emit cooldown.
    last_emit: HashMap<([u8; 32], EmitKind), u64>,
}

impl<F: BlobFetcher, A: AttestationSource> VerifierLoop<F, A> {
    fn new(cfg: VerifierCfg, wasm_limits: WasmLimits, fetcher: F, atts: A) -> Self {
        Self {
            cfg,
            wasm_limits,
            fetcher,
            atts,
            results: HashMap::new(),
            last_emit: HashMap::new(),
        }
    }

    /// Process one (already-coalesced-to-newest) tick. Returns `Err(LoopGone)` iff the actor-tx
    /// receiver is gone.
    fn process(
        &mut self,
        tick: &VerifierTick,
        salts: &mut SaltStore,
        actor_tx_tx: &UnboundedSender<TxKind>,
    ) -> Result<(), LoopGone> {
        let now = tick.now_height;

        // (1) Ensure we hold our OWN result_hash per committee.
        for cv in &tick.committees {
            if self.results.contains_key(&cv.job_id) {
                continue;
            }
            // P14 (restart-liveness): a persisted salt carries the (result_hash, salt) we committed
            // to; recover the result so a resumed node can still commit/reveal in-window.
            if let Some((stored_rh, _salt)) = salts.get(&cv.job_id) {
                self.results.insert(cv.job_id, stored_rh);
                continue;
            }
            // Only re-execute when we might still commit (Committing phase). A Revealing job with no
            // recovered result was never committed by us → nothing to do.
            if cv.phase != VerifierPhase::Committing {
                continue;
            }
            // Heavy work (this thread ONLY): resolve → fetch → split → re-execute.
            let Some(att) = self.atts.resolve(cv.da_root) else {
                continue;
            };
            let Some(blob) = self.fetcher.fetch_blob(&att) else {
                continue; // DA unavailable → retry next tick (NoAttestationSource → inert).
            };
            let Some((program, input)) = split_job_blob(&blob) else {
                continue;
            };
            // Re-execute with the consensus-anchored limits (clone: WasmLimits is Clone-not-Copy).
            if let Ok(rh) =
                reexecute(cv.program_hash, cv.input_hash, program, input, self.wasm_limits.clone())
            {
                self.results.insert(cv.job_id, rh);
            }
        }

        // Build the pure snapshot, injecting our own result hashes.
        let bond_by_job: HashMap<[u8; 32], u64> = tick
            .committees
            .iter()
            .map(|c| (c.job_id, c.verifier_bond))
            .collect();
        let my_committees: Vec<MyCommittee> = tick
            .committees
            .iter()
            .map(|cv| MyCommittee {
                job_id: cv.job_id,
                phase: cv.phase,
                commit_by: cv.commit_by,
                reveal_by: cv.reveal_by,
                verifier_bond: cv.verifier_bond,
                already_committed: cv.already_committed,
                already_revealed: cv.already_revealed,
                my_result_hash: self.results.get(&cv.job_id).copied(),
            })
            .collect();
        let snap = VerifierSnapshot {
            my_committees,
            my_address: tick.my_address,
            my_balance: tick.my_balance,
            cfg: self.cfg,
        };

        // (2) Salt generation for commit-eligible jobs that lack a durable salt (STEP 1 of the
        // contract). P3: never let the planner see a salt that isn't fsynced to disk.
        for job_id in jobs_needing_salt(now, &snap, salts) {
            let Some(result_hash) = self.results.get(&job_id).copied() else {
                continue; // no result → nothing to bind a salt to.
            };
            let salt: [u8; 32] = rand::random(); // node-local CSPRNG; the sole randomness.
            if let Err(e) = salts.insert(job_id, result_hash, salt) {
                // insert() set the in-memory entry BEFORE persist() failed — remove it so the
                // planner cannot commit a salt that isn't on disk (else a crash burns the bond).
                let _ = salts.remove(&job_id);
                tracing::error!(
                    "verifier: salt fsync failed for job {} — NOT committing: {}",
                    hex8(&job_id),
                    e
                );
            }
        }

        // (3) Plan + emit (STEP 2), with the per-(job,kind) re-emit cooldown.
        for action in plan_verifier_actions(now, &snap, salts) {
            let (job_id, ekind, kind) = match action {
                VerifierAction::Commit { job_id, commitment } => {
                    let bond = Amount::from_raw(bond_by_job.get(&job_id).copied().unwrap_or(0));
                    (
                        job_id,
                        EmitKind::Commit,
                        TxKind::Commit { job_id, commit: commitment, bond },
                    )
                }
                VerifierAction::Reveal { job_id, result_hash, salt } => (
                    job_id,
                    EmitKind::Reveal,
                    TxKind::Reveal { job_id, result_hash, salt },
                ),
            };
            let key = (job_id, ekind);
            if let Some(&last) = self.last_emit.get(&key) {
                if now.saturating_sub(last) < REEMIT_COOLDOWN_BLOCKS {
                    continue; // still cooling down; the planner will re-offer next tick.
                }
            }
            if actor_tx_tx.send(kind).is_err() {
                return Err(LoopGone); // event loop dropped the receiver → shut down.
            }
            self.last_emit.insert(key, now);
        }

        // (4) GC the salt secret + caches for settled jobs (the secret is spent).
        for cv in &tick.committees {
            if cv.settled {
                let _ = salts.remove(&cv.job_id);
                self.results.remove(&cv.job_id);
                self.last_emit.remove(&(cv.job_id, EmitKind::Commit));
                self.last_emit.remove(&(cv.job_id, EmitKind::Reveal));
            }
        }
        Ok(())
    }
}

/// First 4 bytes of a 32-byte id, hex — a short log tag.
fn hex8(b: &[u8; 32]) -> String {
    hex::encode(&b[..4])
}

/// Blocking receive loop for the verifier commit/reveal driver — run on a DEDICATED OS thread (WASM
/// re-execution + DA fetch are CPU/latency-heavy and must never touch the event-loop task). Coalesces
/// backlogged ticks to the newest applied state (P7) so it never acts on a stale `now_height`, then
/// drives [`VerifierLoop::process`]. Returns when the tick channel closes or the actor-tx receiver
/// is gone. `salts` is a node-local, fsync-before-broadcast store the caller owns.
pub fn run_verifier_loop(
    snapshot_rx: std::sync::mpsc::Receiver<VerifierTick>,
    actor_tx_tx: UnboundedSender<TxKind>,
    fetcher: impl BlobFetcher,
    atts: impl AttestationSource,
    salts: &mut SaltStore,
    wasm_limits: WasmLimits,
    cfg: VerifierCfg,
) {
    let mut lp = VerifierLoop::new(cfg, wasm_limits, fetcher, atts);
    while let Ok(tick) = snapshot_rx.recv() {
        // P7: work only the newest applied-state tick; drop backlog.
        let mut tick = tick;
        while let Ok(newer) = snapshot_rx.try_recv() {
            tick = newer;
        }
        if lp.process(&tick, salts, &actor_tx_tx).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_pouw::commit_reveal::make_commitment;
    use commputer_pouw::ids::ParticipantId;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::executor_loop::NoAttestationSource;
    use crate::executor_planner::encode_job_blob;

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

    struct AnyAttestation;
    impl AttestationSource for AnyAttestation {
        fn resolve(&self, da_root: [u8; 32]) -> Option<commputer_da::params::DaAttestation> {
            Some(commputer_da::params::DaAttestation {
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

    struct FixedBlob(Vec<u8>);
    impl BlobFetcher for FixedBlob {
        fn fetch_blob(&self, _att: &commputer_da::params::DaAttestation) -> Option<Vec<u8>> {
            Some(self.0.clone())
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "commputer_vloop_{}_{}_{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn program_input() -> (Vec<u8>, Vec<u8>) {
        (
            wat::parse_str(DOUBLER).expect("guest assembles"),
            vec![9u8, 8, 7, 6],
        )
    }

    fn hashes(program: &[u8], input: &[u8]) -> ([u8; 32], [u8; 32]) {
        (Sha256::digest(program).into(), Sha256::digest(input).into())
    }

    const ME: [u8; 32] = [0xAB; 32];

    fn cfg() -> VerifierCfg {
        VerifierCfg { min_balance_reserve: 5 }
    }

    fn committee(
        id: u8,
        phase: VerifierPhase,
        ph: [u8; 32],
        ih: [u8; 32],
        already_committed: bool,
    ) -> VerifierCommitteeView {
        VerifierCommitteeView {
            job_id: [id; 32],
            phase,
            commit_by: 100,
            reveal_by: 200,
            verifier_bond: 20,
            already_committed,
            already_revealed: false,
            program_hash: ph,
            input_hash: ih,
            da_root: [id; 32],
            settled: false,
        }
    }

    fn tick(height: u64, committees: Vec<VerifierCommitteeView>) -> VerifierTick {
        VerifierTick {
            now_height: height,
            committees,
            my_address: Address(ME),
            my_balance: 10_000,
        }
    }

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<TxKind>) -> Vec<TxKind> {
        let mut out = Vec::new();
        while let Ok(k) = rx.try_recv() {
            out.push(k);
        }
        out
    }

    /// A committing verifier with a real re-executed result commits exactly once, with the frozen
    /// commitment bound to the persisted (result_hash, salt), and the salt is durably on disk.
    #[test]
    fn commits_with_a_stored_salt() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);
        let expected_rh = reexecute(ph, ih, &program, &input, WasmLimits::default()).unwrap();

        let dir = scratch_dir("commit");
        let mut salts = SaltStore::open(&dir).unwrap();
        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        let mut lp = VerifierLoop::new(cfg(), WasmLimits::default(), FixedBlob(blob), AnyAttestation);

        let t = tick(50, vec![committee(1, VerifierPhase::Committing, ph, ih, false)]);
        lp.process(&t, &mut salts, &atx).unwrap();

        let out = drain(&mut arx);
        assert_eq!(out.len(), 1, "exactly one Commit");
        // The salt was persisted; the commitment binds to it.
        let (stored_rh, salt) = salts.get(&[1; 32]).expect("salt persisted before Commit");
        assert_eq!(stored_rh, expected_rh, "salt store holds the re-executed result");
        let expected_commit =
            make_commitment(&ParticipantId(ME), &stored_rh, &salt, 20).commit;
        match &out[0] {
            TxKind::Commit { job_id, commit, bond } => {
                assert_eq!(*job_id, [1; 32]);
                assert_eq!(*commit, expected_commit, "commitment binds to the stored (rh, salt)");
                assert_eq!(*bond, Amount::from_raw(20), "bond == verifier_bond");
            }
            other => panic!("expected Commit, got {other:?}"),
        }

        // Idempotency: a second identical tick (commit still in flight, cooldown active) → no re-commit.
        lp.process(&t, &mut salts, &atx).unwrap();
        assert!(drain(&mut arx).is_empty(), "no re-commit within cooldown");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P3: when `SaltStore::insert` fails to fsync (its directory has been removed), the loop clears
    /// the phantom in-memory entry and emits NO Commit — never a commit whose salt isn't on disk.
    #[test]
    fn p3_salt_fsync_failure_suppresses_commit() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);

        let dir = scratch_dir("p3");
        let mut salts = SaltStore::open(&dir).unwrap();
        // Pull the directory out from under the store so `persist()` (temp-file open) fails.
        std::fs::remove_dir_all(&dir).unwrap();

        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        let mut lp = VerifierLoop::new(cfg(), WasmLimits::default(), FixedBlob(blob), AnyAttestation);

        let t = tick(50, vec![committee(1, VerifierPhase::Committing, ph, ih, false)]);
        lp.process(&t, &mut salts, &atx).unwrap();

        assert!(
            drain(&mut arx).is_empty(),
            "salt fsync failed → NO Commit emitted"
        );
        assert!(
            salts.get(&[1; 32]).is_none(),
            "phantom in-memory salt entry cleared after the failed insert"
        );
    }

    /// P14: after a restart the in-memory result cache is empty, but a persisted salt repopulates it
    /// so a resumed node can still commit an un-reflected commit (already_committed == false).
    /// Non-vacuity: `NoAttestationSource` means the ONLY way a result appears is the salt recovery.
    #[test]
    fn p14_restart_recovers_result_from_salt_to_commit() {
        let rh = [0x33u8; 32];
        let salt = [0x44u8; 32];
        let dir = scratch_dir("p14");
        let mut salts = SaltStore::open(&dir).unwrap();
        // Pre-crash state: the salt was persisted (but the Commit never applied on-chain).
        salts.insert([7; 32], rh, salt).unwrap();

        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        // Fresh loop (empty result cache) + NoAttestationSource (cannot re-execute).
        let mut lp =
            VerifierLoop::new(cfg(), WasmLimits::default(), FixedBlob(vec![]), NoAttestationSource);

        let t = tick(
            50,
            vec![committee(7, VerifierPhase::Committing, [0; 32], [0; 32], false)],
        );
        lp.process(&t, &mut salts, &atx).unwrap();

        let out = drain(&mut arx);
        assert_eq!(out.len(), 1, "resumed node commits from the recovered salt");
        let expected_commit = make_commitment(&ParticipantId(ME), &rh, &salt, 20).commit;
        match &out[0] {
            TxKind::Commit { job_id, commit, .. } => {
                assert_eq!(*job_id, [7; 32]);
                assert_eq!(*commit, expected_commit, "commit binds the recovered (rh, salt)");
            }
            other => panic!("expected Commit, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A committed verifier in the Revealing phase reveals exactly once with the stored salt (across
    /// a simulated restart: separate loop invocations sharing the same durable `SaltStore`).
    #[test]
    fn commit_then_reveal_across_restart() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);
        let expected_rh = reexecute(ph, ih, &program, &input, WasmLimits::default()).unwrap();

        let dir = scratch_dir("cr");
        let mut salts = SaltStore::open(&dir).unwrap();

        // Invocation 1 (pre-restart): commit.
        {
            let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
            let mut lp =
                VerifierLoop::new(cfg(), WasmLimits::default(), FixedBlob(blob), AnyAttestation);
            let t = tick(50, vec![committee(1, VerifierPhase::Committing, ph, ih, false)]);
            lp.process(&t, &mut salts, &atx).unwrap();
            assert_eq!(drain(&mut arx).len(), 1, "commit pre-restart");
        }
        let (stored_rh, salt) = salts.get(&[1; 32]).expect("salt survives");
        assert_eq!(stored_rh, expected_rh);

        // Invocation 2 (post-restart): fresh loop, empty caches; the job is now Revealing + committed.
        {
            let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
            let mut lp = VerifierLoop::new(
                cfg(),
                WasmLimits::default(),
                FixedBlob(vec![]),
                NoAttestationSource,
            );
            let t = tick(150, vec![committee(1, VerifierPhase::Revealing, ph, ih, true)]);
            lp.process(&t, &mut salts, &atx).unwrap();
            let out = drain(&mut arx);
            assert_eq!(out.len(), 1, "one Reveal");
            assert!(
                matches!(
                    &out[0],
                    TxKind::Reveal { job_id: j, result_hash: r, salt: s }
                        if *j == [1; 32] && *r == stored_rh && *s == salt
                ),
                "reveal uses the salt recovered from disk"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P7: `run_verifier_loop` coalesces a backlog to the newest tick. An old in-window tick alone
    /// commits; but with a newer past-`commit_by` tick queued behind it, only the newest is
    /// processed → no Commit.
    #[test]
    fn p7_coalesces_to_newest_tick() {
        let (program, input) = program_input();
        let (ph, ih) = hashes(&program, &input);
        let blob = encode_job_blob(&program, &input);

        // Baseline (non-vacuity): the OLD tick alone commits.
        {
            let dir = scratch_dir("p7a");
            let mut salts = SaltStore::open(&dir).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(tick(50, vec![committee(1, VerifierPhase::Committing, ph, ih, false)]))
                .unwrap();
            drop(tx);
            let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
            run_verifier_loop(
                rx,
                atx,
                FixedBlob(blob.clone()),
                AnyAttestation,
                &mut salts,
                WasmLimits::default(),
                cfg(),
            );
            assert_eq!(drain(&mut arx).len(), 1, "old tick alone commits");
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Coalesced: OLD (height 50, in window) then NEW (height 999, past commit_by 100) → only NEW.
        {
            let dir = scratch_dir("p7b");
            let mut salts = SaltStore::open(&dir).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(tick(50, vec![committee(1, VerifierPhase::Committing, ph, ih, false)]))
                .unwrap();
            tx.send(tick(999, vec![committee(1, VerifierPhase::Committing, ph, ih, false)]))
                .unwrap();
            drop(tx);
            let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
            run_verifier_loop(
                rx,
                atx,
                FixedBlob(blob),
                AnyAttestation,
                &mut salts,
                WasmLimits::default(),
                cfg(),
            );
            assert!(
                drain(&mut arx).is_empty(),
                "coalesced to the newest (past-window) tick → no Commit"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Salt GC: once a job is `settled` the loop drops the salt secret and its caches.
    #[test]
    fn settled_job_gcs_the_salt() {
        let rh = [0x55u8; 32];
        let salt = [0x66u8; 32];
        let dir = scratch_dir("gc");
        let mut salts = SaltStore::open(&dir).unwrap();
        salts.insert([1; 32], rh, salt).unwrap();

        let (atx, _arx) = tokio::sync::mpsc::unbounded_channel();
        let mut lp =
            VerifierLoop::new(cfg(), WasmLimits::default(), FixedBlob(vec![]), NoAttestationSource);

        let mut c = committee(1, VerifierPhase::Other, [0; 32], [0; 32], true);
        c.settled = true;
        lp.process(&tick(300, vec![c]), &mut salts, &atx).unwrap();

        assert!(salts.get(&[1; 32]).is_none(), "settled → salt GC'd");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
