//! pouw_payout_e2e.rs — the FIRST end-to-end PoUW pay-out acceptance test (Track-2 A/B).
//!
//! WHAT THIS PROVES (in-process, no libp2p, runnable under `cargo test`): a submitted compute
//! job flows through the REAL node actor loops (`executor_loop::run` + `verifier_loop::run_verifier_loop`)
//! AND the REAL on-chain apply/settlement path (`ChainState::apply_block` → in-tail
//! `draw_committees_for_completed_jobs` + `settle_due_jobs`) and actually PAYS the executor + the
//! committee verifiers. This is the acceptance proof the Phase A/B loops (previously "verified by
//! construction") compose into a real pay-out.
//!
//! HOW THE LOOPS ARE DRIVEN (no threads, fully deterministic): each block we build the applied-state
//! snapshot with the REAL constructors (`executor_loop::build_chain_view` /
//! `verifier_loop::build_verifier_views`), then invoke the REAL blocking driver (`run` /
//! `run_verifier_loop`) with a one-shot `std::sync::mpsc` channel — send exactly one snapshot, drop
//! the sender, so the loop processes that snapshot and returns when `recv()` sees the closed channel
//! (the same shape the loops' own `run_coalesces_to_newest_view` / `p7_coalesces_to_newest_tick`
//! unit tests use). We drain the emitted `TxKind`s off the actor channel, wrap each into a signed-
//! shape (unsigned; `apply_block` does not verify signatures — see storage `unsigned` helper)
//! `Transaction` from the emitting wallet at its correct on-chain nonce, and include it in the next
//! real block. A fresh loop per block is sound because on-chain state reconciles idempotency (a
//! claimed job leaves `pending_jobs`; a completed job advances phase; `already_committed` /
//! `already_revealed` suppress re-emits) and the verifier's salt is recovered from the DURABLE
//! per-verifier `SaltStore` across the fresh loops (exactly the restart-liveness path).
//!
//! THE Q15 DA PATH IS REAL + IN-PROCESS: `da_publisher::publish_job_blob` persists the 2N coded
//! chunks + the attestation object into an on-disk `DaStore` (tempdir); the loops resolve + fetch
//! through the production `DaBackedAttestationSource` / `BridgeBlobFetcher` driven over a
//! `StoreTransport` (`DaStore` → `DaTransport`) — the same seam production runs, minus the swarm.
//!
//! NON-PROTECTED: this is a NEW integration-test file; it imports the node lib as `commputer::...`
//! and touches no protected file and no frozen crate. No node source was modified — the in-test
//! `StoreTransport` is built purely from public API (`DaStore::{get,has}` +
//! `da_publisher::deserialize_merkle_path` + the public `commputer_da` `DaTransport` trait).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use commputer::da_attestation::{BridgeBlobFetcher, DaBackedAttestationSource};
use commputer::da_publisher::{self, deserialize_merkle_path};
use commputer::da_store::DaStore;
use commputer::executor_loop::{self, ExecutorChainView};
use commputer::executor_planner::ExecutorCfg;
use commputer::salt_store::SaltStore;
use commputer::verifier_loop::{self, VerifierTick};
use commputer::verifier_planner::VerifierCfg;

use commputer_core::block::{Block, BlockHash, BlockHeader};
use commputer_core::compute::ResourceRequirements;
use commputer_core::identity::Address;
use commputer_core::token::Amount;
use commputer_core::transaction::{Transaction, TxKind};

use commputer_da::params::ProviderId;
use commputer_da::transport::{DaTransport, MerklePath};
use commputer_network::da_protocol::DaChunk;

use commputer_pouw::wasm::WasmLimits;
use commputer_pouw_onchain::consensus_params::PhaseWindows;
use commputer_pouw_onchain::lifecycle::Phase;
use commputer_storage::state::ChainState;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// A known-good, tiny, DETERMINISTIC WASM guest (doubles each input byte mod 256) — the same guest
// the frozen executor / DA / planner tests use. The executor and every verifier re-execute THIS
// program over THIS input under the SAME WasmLimits::default(), so they all derive the identical
// result_hash → the committee confirms.
// ─────────────────────────────────────────────────────────────────────────────────────────────
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

// Genesis-default game/stake params (GameParams::default / StakeParams::default), pinned here so
// the expected pay-out split is explicit.
const MIN_BUDGET: u64 = 1_000_000; // commputer_core::compute::MIN_JOB_BUDGET
const EXECUTOR_BOND_FLAT: u64 = 100; // GameParams::default().executor_bond (floor; budget dominates)
const VERIFIER_BOND: u64 = 20; // GameParams::default().verifier_bond
const MIN_BOND: u64 = 1_000; // StakeParams::default().min_bond
const WORKER_BPS: u64 = 8_500;
const VERIFIER_BPS: u64 = 1_000;
const DISPUTE_BOUNTY_BPS: u64 = 2_000; // GameParams::default().dispute_bounty_bps (committee-Disputed catch bounty)

const PROVIDER: ProviderId = ProviderId([200u8; 32]);

// ── in-test DA transport: serve the published chunks straight off the on-disk DaStore ──────────
// Built entirely from public API; mirrors the (private, #[cfg(test)]) StoreTransport the loops'
// own tests use, so the resolver + fetcher run against exactly what the publisher persisted.
struct StoreTransport<'a> {
    store: &'a DaStore,
}

impl DaTransport for StoreTransport<'_> {
    fn advertise(&self, _chunk_hash: [u8; 32], _me: ProviderId) {}
    fn find_providers(&self, chunk_hash: [u8; 32]) -> Vec<ProviderId> {
        if self.store.has(chunk_hash) {
            vec![PROVIDER]
        } else {
            vec![]
        }
    }
    fn fetch_chunk(&self, chunk_hash: [u8; 32], from: ProviderId) -> Option<(Vec<u8>, MerklePath)> {
        if from != PROVIDER {
            return None;
        }
        let da_chunk: DaChunk = self.store.get(chunk_hash).ok().flatten()?;
        let path = deserialize_merkle_path(&da_chunk.merkle_path)?;
        Some((da_chunk.bytes, path))
    }
    fn has_chunk(&self, chunk_hash: [u8; 32]) -> bool {
        self.store.has(chunk_hash)
    }
}

// ── tiny helpers (public-API replicas of the storage crate's private test helpers) ─────────────
fn addr(n: u8) -> Address {
    Address([n; 32])
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("commputer_payout_e2e_{}_{}_{}", tag, std::process::id(), n))
}

fn unsigned(from: Address, nonce: u64, kind: TxKind) -> Transaction {
    Transaction {
        from,
        nonce,
        kind,
        fee: 0, // zero fee ⇒ money is conserved EXACTLY across every driven block
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    }
}

fn genesis_block() -> Block {
    Block {
        header: BlockHeader {
            protocol_version: 1,
            height: 0,
            parent_hash: BlockHash::GENESIS,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 1000,
            producer: addr(0), // zero-address producer earns nothing ⇒ no mint
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
            chain_id: String::new(),
        },
        transactions: vec![],
        proof_summaries: vec![],
        compliance_summary: None,
        epoch_summary: None,
    }
}

fn next_block(state: &ChainState, txs: Vec<Transaction>) -> Block {
    let height = state.blocks.height() + 1;
    Block {
        header: BlockHeader {
            protocol_version: 1,
            height,
            parent_hash: state.blocks.latest().unwrap().hash(),
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 2000 + height,
            producer: addr(0),
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
            chain_id: String::new(),
        },
        transactions: txs,
        proof_summaries: vec![],
        compliance_summary: None,
        epoch_summary: None,
    }
}

fn bal(state: &ChainState, a: Address) -> u64 {
    state.accounts.get(&a).map(|x| x.balance.raw()).unwrap_or(0)
}
fn nonce(state: &ChainState, a: Address) -> u64 {
    state.accounts.get(&a).map(|x| x.nonce).unwrap_or(0)
}

/// The five-bucket conserved quantity (mirrors storage `money_conserved`): spendable + escrowed +
/// active bonded + unbonding cooldown + burned. With zero fees and a zero-address producer nothing
/// is minted, so this is invariant across every block.
fn conserved(state: &ChainState) -> u64 {
    let spendable: u64 = state.accounts.iter().map(|a| a.balance.raw()).sum();
    spendable
        + state.total_escrowed()
        + state.total_bonded()
        + state.total_unbonding()
        + state.total_burned
}

// ── drive the REAL blocking loops one snapshot per block (no threads) ───────────────────────────
fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<TxKind>) -> Vec<TxKind> {
    let mut out = Vec::new();
    while let Ok(k) = rx.try_recv() {
        out.push(k);
    }
    out
}

/// Run the REAL `executor_loop::run` against a single applied-state view and return the emitted
/// `TxKind`s. The DA seams are the production `DaBackedAttestationSource` / `BridgeBlobFetcher`
/// over a `StoreTransport` wrapping the real on-disk `DaStore` — the loop resolves the attestation
/// from the bare on-chain `da_root`, fetches + reconstructs the `program‖input`, and re-executes.
fn drive_executor(store: &DaStore, view: ExecutorChainView, cfg: ExecutorCfg) -> Vec<TxKind> {
    let (snap_tx, snap_rx) = std::sync::mpsc::channel();
    snap_tx.send(view).unwrap();
    drop(snap_tx); // loop processes the one snapshot, then returns on the closed channel
    let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
    let atts = DaBackedAttestationSource::new(StoreTransport { store });
    let fetcher = BridgeBlobFetcher::new(StoreTransport { store }, [0xE0u8; 32]);
    executor_loop::run(cfg, WasmLimits::default(), fetcher, atts, snap_rx, actor_tx);
    drain(&mut actor_rx)
}

/// Run the REAL `verifier_loop::run_verifier_loop` against a single tick with this verifier's
/// DURABLE salt store — the fresh loop recovers its committed (result_hash, salt) from disk exactly
/// as a restarted node would, so commit-then-reveal spans the per-block fresh loops correctly.
fn drive_verifier(
    store: &DaStore,
    tick: VerifierTick,
    salts: &mut SaltStore,
    cfg: VerifierCfg,
    verifier_id: [u8; 32],
) -> Vec<TxKind> {
    let (snap_tx, snap_rx) = std::sync::mpsc::channel();
    snap_tx.send(tick).unwrap();
    drop(snap_tx);
    let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
    let atts = DaBackedAttestationSource::new(StoreTransport { store });
    let fetcher = BridgeBlobFetcher::new(StoreTransport { store }, verifier_id);
    verifier_loop::run_verifier_loop(snap_rx, actor_tx, fetcher, atts, salts, WasmLimits::default(), cfg);
    drain(&mut actor_rx)
}

/// What a driven round settled into, plus the tx-kind counts the loops actually produced.
struct Round {
    state: ChainState,
    job: [u8; 32],
    conserved0: u64,
    submitter: Address,
    executor: Address,
    verifiers: [Address; 3],
    claims: u32,
    completes: u32,
    commits: u32,
    reveals: u32,
    blocks: u32,
}

/// Set up a fresh chain (funded + bonded actors, short phase windows), publish the job blob to a
/// real DaStore, submit it on-chain, then drive the REAL executor loop (+ optionally the REAL
/// verifier loops) block-by-block until the lifecycle settles + drains. Asserts money conservation
/// after every block. `drive_verifiers=false` is the negative control (no committee action).
fn drive_round(drive_verifiers: bool) -> Round {
    let submitter = addr(2);
    let executor = addr(1);
    let verifiers = [addr(3), addr(4), addr(5)];

    let mut state = ChainState::new();
    // Short windows keep the round quick while leaving ample headroom for the ~2-3 block round-trips
    // (claim at parent-height 1 ⇒ result_by 4 / commit_by 7 / reveal_by 10 ⇒ settle at height 11).
    state.phase_windows = PhaseWindows { result_blocks: 3, commit_blocks: 3, reveal_blocks: 3, claim_blocks: 6 };
    state.apply_block(&genesis_block()).unwrap();

    // Fund: submitter can escrow the budget; executor is a validator that can fund its bond
    // (e_bond = max(budget, 100) = budget); each verifier is a compliant validator funded to bond
    // MIN_BOND (eligibility) and still hold exactly one VERIFIER_BOND to escrow on commit.
    state.accounts.get_or_create(submitter).balance = Amount::from_raw(MIN_BUDGET);
    {
        let e = state.accounts.get_or_create(executor);
        e.is_validator = true;
        e.balance = Amount::from_raw(MIN_BUDGET);
    }
    for &v in &verifiers {
        let a = state.accounts.get_or_create(v);
        a.is_validator = true;
        a.balance = Amount::from_raw(MIN_BOND + VERIFIER_BOND);
    }
    // Real bonded stake (moves balance → bonded_stake) so each verifier is an eligible candidate.
    for &v in &verifiers {
        state.bond(&v, MIN_BOND).unwrap();
    }

    let conserved0 = conserved(&state);

    // ── publish the job blob to a real on-disk DaStore, then submit it on-chain (SubmitJobV2). ──
    let program = wat::parse_str(DOUBLER).expect("guest assembles");
    let input = vec![1u8, 2, 3, 40, 7];
    let store = DaStore::open(scratch("da")).unwrap();
    let att = da_publisher::publish_job_blob(&store, &program, &input).expect("publish job blob");
    let program_hash: [u8; 32] = Sha256::digest(&program).into();
    let input_hash: [u8; 32] = Sha256::digest(&input).into();

    let submit_kind = TxKind::SubmitJobV2 {
        program_hash,
        input_hash,
        da_root: att.da_root,
        resources: ResourceRequirements::cpu_only(1, 0),
        max_duration_secs: 60,
        comme_budget: Amount::from_raw(MIN_BUDGET),
        l2_id: None,
    };
    let submit_tx = unsigned(submitter, 0, submit_kind);
    let job = submit_tx.hash().0; // on-chain job identity == tx hash
    state.apply_block(&next_block(&state, vec![submit_tx])).unwrap();
    assert_eq!(state.escrowed_for_job(&job), MIN_BUDGET, "budget escrowed at submit");
    assert_eq!(conserved(&state), conserved0, "conserved after submit");

    // Durable, node-local salt store per verifier.
    let mut salts: Vec<SaltStore> =
        (0..verifiers.len()).map(|i| SaltStore::open(scratch(&format!("salt{i}"))).unwrap()).collect();

    let exec_cfg = ExecutorCfg { max_concurrent_claims: 4, min_balance_reserve: 0, executor_bond: EXECUTOR_BOND_FLAT };
    let ver_cfg = VerifierCfg { min_balance_reserve: 0 };

    let (mut claims, mut completes, mut commits, mut reveals, mut blocks) = (0u32, 0u32, 0u32, 0u32, 0u32);

    // Drive until the lifecycle (and its pending precursor) is gone, or a generous bound trips.
    while (!state.pending_jobs.is_empty() || !state.job_lifecycles.is_empty()) && blocks < 40 {
        // `now` == the PARENT height at which this block's txs apply (== current stored height),
        // matching every on-chain admission window check (claim_by / result_by / commit_by / reveal_by).
        let now = state.blocks.height();
        let mut txs: Vec<Transaction> = Vec::new();

        // Executor loop.
        let exec_view = executor_loop::build_chain_view(
            now,
            0,
            executor,
            bal(&state, executor),
            &state.pending_jobs,
            &state.job_lifecycles,
        );
        for kind in drive_executor(&store, exec_view, exec_cfg) {
            match kind {
                TxKind::ClaimJob { .. } => claims += 1,
                TxKind::CompleteJob { .. } => completes += 1,
                _ => panic!("executor emitted unexpected {kind:?}"),
            }
            txs.push(unsigned(executor, nonce(&state, executor), kind));
        }

        // Verifier loops (each drawn committee member acts on its own tick + salt store).
        if drive_verifiers {
            for (i, &v) in verifiers.iter().enumerate() {
                let tick = verifier_loop::build_verifier_views(now, v, bal(&state, v), &state.job_lifecycles);
                for kind in drive_verifier(&store, tick, &mut salts[i], ver_cfg, v.0) {
                    match kind {
                        TxKind::Commit { .. } => commits += 1,
                        TxKind::Reveal { .. } => reveals += 1,
                        _ => panic!("verifier emitted unexpected {kind:?}"),
                    }
                    txs.push(unsigned(v, nonce(&state, v), kind));
                }
            }
        }

        state.apply_block(&next_block(&state, txs)).unwrap();
        blocks += 1;
        assert_eq!(conserved(&state), conserved0, "money conserved after driven block {blocks}");
    }

    assert!(state.job_lifecycles.is_empty(), "lifecycle settled + drained within the block bound");
    assert!(state.pending_jobs.is_empty(), "no pending record left behind");
    assert_eq!(state.escrowed_for_job(&job), 0, "job pot fully drained at the terminal");

    Round { state, job, conserved0, submitter, executor, verifiers, claims, completes, commits, reveals, blocks }
}

/// HAPPY PATH: the loops drive claim → complete → commit → reveal; the committee confirms; the pot
/// pays out the audited 85 / 10 / 5 split and returns every bond.
#[test]
fn pouw_confirmed_job_pays_executor_and_verifiers_end_to_end() {
    let r = drive_round(true);
    let s = &r.state;

    // The REAL loops produced the whole tx stream (not hand-fed): a claim, a complete, 3 commits, 3 reveals.
    assert!(r.claims >= 1, "executor loop emitted a ClaimJob");
    assert!(r.completes >= 1, "executor loop emitted a CompleteJob (real DA fetch + re-execute)");
    assert_eq!(r.commits, 3, "all three committee verifier loops emitted a Commit");
    assert_eq!(r.reveals, 3, "all three committee verifier loops emitted a Reveal");

    // Audited Confirmed split for budget = MIN_BUDGET, e_bond = max(budget,100) = budget, v_bond = 20:
    let e_bond = MIN_BUDGET; // budget dominates the flat floor
    let worker_share = MIN_BUDGET * WORKER_BPS / 10_000; // 850_000
    let verifier_share = (MIN_BUDGET * VERIFIER_BPS / 10_000) / 3; // floor(100_000/3) = 33_333
    let burn = MIN_BUDGET - worker_share - 3 * verifier_share; // 5% + rounding remainder = 50_001

    // Executor: funded MIN_BUDGET → escrowed e_bond at claim (→0) → worker share + bond back.
    assert_eq!(
        bal(s, r.executor),
        worker_share + e_bond,
        "executor paid 85% of the budget + bond returned"
    );
    // Each verifier: funded VERIFIER_BOND (after bonding MIN_BOND) → escrowed it on commit (→0) →
    // its 10%/k pool share + bond back.
    for &v in &r.verifiers {
        assert_eq!(
            bal(s, v),
            VERIFIER_BOND + verifier_share,
            "verifier paid its 10%/k pool share + bond returned"
        );
    }
    // Submitter spent exactly the budget (Confirmed ⇒ no refund).
    assert_eq!(bal(s, r.submitter), 0, "submitter spent the whole budget");
    assert_eq!(s.total_burned, burn, "exactly the 5% burn slice + rounding remainder");
    assert_eq!(s.escrowed_for_job(&r.job), 0, "pot drained to 0");
    assert_eq!(conserved(s), r.conserved0, "total supply conserved");

    // BEFORE → AFTER deltas (the pay-out): executor +850_000 (worker 85%), each verifier +33_333
    // (10%/3), submitter −1_000_000 (the pot). Bonds net zero (escrowed then returned).
    assert_eq!(worker_share, 850_000);
    assert_eq!(verifier_share, 33_333);
    assert_eq!(burn, 50_001);
    assert!(r.blocks <= 20, "converged quickly (was {})", r.blocks);
}

/// NEGATIVE CONTROL: an IDENTICAL job where the executor still claims + completes (committee drawn)
/// but NO verifier acts ⇒ NoQuorum ⇒ Escalate ⇒ D2-FINAL zero-comp fallback. The machinery must NOT
/// pay: the submitter is fully REFUNDED, the executor's bond returns intact with ZERO worker comp,
/// verifiers are untouched, and nothing is burned.
#[test]
fn pouw_noquorum_refunds_submitter_pays_no_worker_comp() {
    let r = drive_round(false);
    let s = &r.state;

    // Executor still drove claim + complete (so a committee WAS drawn) — but no verifier acted.
    assert!(r.claims >= 1, "executor loop emitted a ClaimJob");
    assert!(r.completes >= 1, "executor loop emitted a CompleteJob");
    assert_eq!(r.commits, 0, "no verifier committed (negative control)");
    assert_eq!(r.reveals, 0, "no verifier revealed (negative control)");

    // resolve_escalation_fallback (zero comp): submitter refunded the full budget; executor bond
    // back with ZERO worker pay; committee bonds n/a (none committed).
    let e_bond = MIN_BUDGET;
    assert_eq!(bal(s, r.submitter), MIN_BUDGET, "submitter fully refunded");
    assert_eq!(bal(s, r.executor), e_bond, "executor bond returned intact, ZERO worker comp");
    for &v in &r.verifiers {
        assert_eq!(bal(s, v), VERIFIER_BOND, "idle verifier untouched (kept its unescrowed bond)");
    }
    assert_eq!(s.total_burned, 0, "the zero-comp fallback burns nothing");
    assert_eq!(s.escrowed_for_job(&r.job), 0, "pot drained to 0");
    assert_eq!(conserved(s), r.conserved0, "total supply conserved");
}

/// FRAUD PATH (the economic-security triptych's third leg: Confirmed pay-out ✓, NoQuorum refund ✓,
/// Disputed slash ← this). Same setup as the Confirmed test, but the executor CLAIMS honestly then
/// COMPLETES WITH A BOGUS `result_hash` — an adversary that bypasses the honest executor loop (which
/// would DA-fetch, re-execute, and post the TRUE hash). The 3 committee verifiers run the REAL
/// `run_verifier_loop` over the in-process DA: they fetch the blob, RE-EXECUTE (deriving the TRUE
/// hash), and Commit+Reveal it. That quorum of true-hash reveals DISAGREES with the executor's
/// committed bogus hash ⇒ `compute_verdict` → `Verdict::Disputed` ⇒ `settle` routes to
/// `resolve_disputed`: the cheating executor's WHOLE bond is slashed (20% catch-bounty to the honest
/// verifiers, the rest burned), the submitter is refunded in full, and every committee bond returns.
/// Nothing on-chain re-executes the executor's claim (`lifecycle_post_result` records the hash
/// verbatim — storage/state.rs:3544) — the verification game is the ONLY thing that catches the lie.
#[test]
fn pouw_disputed_slashes_cheating_executor_pays_honest_verifiers() {
    // The bogus result the cheating executor commits to: a fixed constant that is NOT the true
    // execute_job(DOUBLER, input) output, so the honest committee's re-executed hash disagrees.
    const BOGUS_HASH: [u8; 32] = [0xABu8; 32];

    let submitter = addr(2);
    let executor = addr(1);
    let verifiers = [addr(3), addr(4), addr(5)];

    let mut state = ChainState::new();
    // Identical short windows to drive_round (claim@1 ⇒ result_by 4 / commit_by 7 / reveal_by 10).
    state.phase_windows = PhaseWindows { result_blocks: 3, commit_blocks: 3, reveal_blocks: 3, claim_blocks: 6 };
    state.apply_block(&genesis_block()).unwrap();

    // Identical funding + bonding to drive_round: submitter escrows the budget; the executor is a
    // validator funded to post e_bond = max(budget, 100) = budget; each verifier bonds MIN_BOND for
    // eligibility and keeps exactly one VERIFIER_BOND to escrow on commit.
    state.accounts.get_or_create(submitter).balance = Amount::from_raw(MIN_BUDGET);
    {
        let e = state.accounts.get_or_create(executor);
        e.is_validator = true;
        e.balance = Amount::from_raw(MIN_BUDGET);
    }
    for &v in &verifiers {
        let a = state.accounts.get_or_create(v);
        a.is_validator = true;
        a.balance = Amount::from_raw(MIN_BOND + VERIFIER_BOND);
    }
    for &v in &verifiers {
        state.bond(&v, MIN_BOND).unwrap();
    }

    let conserved0 = conserved(&state);
    // BEFORE snapshot (post-bond, pre-submit) — the reference for the slash / pay-out deltas.
    let submitter_before = bal(&state, submitter); // 1_000_000
    let executor_before = bal(&state, executor); //   1_000_000
    let verifier_before = bal(&state, verifiers[0]); //     20 (VERIFIER_BOND; MIN_BOND is bonded)
    assert_eq!((submitter_before, executor_before, verifier_before), (MIN_BUDGET, MIN_BUDGET, VERIFIER_BOND));

    // Publish the job blob to a real on-disk DaStore, then submit it on-chain (budget escrowed).
    let program = wat::parse_str(DOUBLER).expect("guest assembles");
    let input = vec![1u8, 2, 3, 40, 7];
    let store = DaStore::open(scratch("da")).unwrap();
    let att = da_publisher::publish_job_blob(&store, &program, &input).expect("publish job blob");
    let program_hash: [u8; 32] = Sha256::digest(&program).into();
    let input_hash: [u8; 32] = Sha256::digest(&input).into();
    let submit_kind = TxKind::SubmitJobV2 {
        program_hash,
        input_hash,
        da_root: att.da_root,
        resources: ResourceRequirements::cpu_only(1, 0),
        max_duration_secs: 60,
        comme_budget: Amount::from_raw(MIN_BUDGET),
        l2_id: None,
    };
    let submit_tx = unsigned(submitter, 0, submit_kind);
    let job = submit_tx.hash().0;
    state.apply_block(&next_block(&state, vec![submit_tx])).unwrap();
    assert_eq!(state.escrowed_for_job(&job), MIN_BUDGET, "budget escrowed at submit");
    assert_eq!(conserved(&state), conserved0, "conserved after submit");

    let mut salts: Vec<SaltStore> =
        (0..verifiers.len()).map(|i| SaltStore::open(scratch(&format!("salt{i}"))).unwrap()).collect();
    let exec_cfg = ExecutorCfg { max_concurrent_claims: 4, min_balance_reserve: 0, executor_bond: EXECUTOR_BOND_FLAT };
    let ver_cfg = VerifierCfg { min_balance_reserve: 0 };

    let (mut claims, mut completes, mut commits, mut reveals, mut blocks) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut cheated = false; // guard: inject the fraudulent CompleteJob exactly once

    while (!state.pending_jobs.is_empty() || !state.job_lifecycles.is_empty()) && blocks < 40 {
        let now = state.blocks.height();
        let mut txs: Vec<Transaction> = Vec::new();

        // EXECUTOR. While the job is PENDING, drive the REAL executor loop to obtain the honest
        // ClaimJob. Once it is claimed (lifecycle AwaitingResult, no result yet), inject ONE
        // fraudulent CompleteJob with a BOGUS hash — deliberately NOT driving the honest loop for
        // the complete (it would re-execute the DA blob and post the TRUE hash, yielding Confirmed).
        if !state.pending_jobs.is_empty() {
            let exec_view = executor_loop::build_chain_view(
                now,
                0,
                executor,
                bal(&state, executor),
                &state.pending_jobs,
                &state.job_lifecycles,
            );
            for kind in drive_executor(&store, exec_view, exec_cfg) {
                match kind {
                    TxKind::ClaimJob { .. } => claims += 1,
                    other => panic!("executor should only CLAIM while the job is pending, got {other:?}"),
                }
                txs.push(unsigned(executor, nonce(&state, executor), kind));
            }
        } else if !cheated {
            if let Some(lc) = state.job_lifecycles.get(&job) {
                if lc.phase() == Phase::AwaitingResult && !lc.executor_hash_is_set() {
                    completes += 1;
                    cheated = true;
                    txs.push(unsigned(
                        executor,
                        nonce(&state, executor),
                        TxKind::CompleteJob { job_id: job, result_hash: BOGUS_HASH },
                    ));
                }
            }
        }

        // VERIFIERS: fully HONEST — the REAL verifier loop DA-fetches, re-executes, and
        // commit/reveals the TRUE hash (which disagrees with the executor's committed bogus hash).
        for (i, &v) in verifiers.iter().enumerate() {
            let tick = verifier_loop::build_verifier_views(now, v, bal(&state, v), &state.job_lifecycles);
            for kind in drive_verifier(&store, tick, &mut salts[i], ver_cfg, v.0) {
                match kind {
                    TxKind::Commit { .. } => commits += 1,
                    TxKind::Reveal { .. } => reveals += 1,
                    other => panic!("verifier emitted unexpected {other:?}"),
                }
                txs.push(unsigned(v, nonce(&state, v), kind));
            }
        }

        state.apply_block(&next_block(&state, txs)).unwrap();
        blocks += 1;
        assert_eq!(conserved(&state), conserved0, "money conserved after driven block {blocks}");
    }

    assert!(state.job_lifecycles.is_empty(), "lifecycle settled + drained within the block bound");
    assert!(state.pending_jobs.is_empty(), "no pending record left behind");

    // The loops really drove the fraud: an honest claim, exactly ONE bogus complete, and 3 honest
    // commit+reveal pairs whose re-executed TRUE hash disagreed with the executor's committed hash.
    assert!(claims >= 1, "executor loop emitted a ClaimJob (honest claim)");
    assert!(cheated, "the fraudulent CompleteJob was injected at AwaitingResult");
    assert_eq!(completes, 1, "exactly one fraudulent CompleteJob was submitted");
    assert_eq!(commits, 3, "all three committee verifier loops committed the true hash");
    assert_eq!(reveals, 3, "all three committee verifier loops revealed the true hash");

    let s = &state;

    // ── The EXACT resolve_disputed split (settlement_resolution.rs:181 → frozen
    //    settle_committee_disputed, settlement.rs:102). Inputs: budget = MIN_BUDGET (1_000_000);
    //    e_bond = max(budget, 100) = 1_000_000 (budget dominates the flat floor); v_bond = 20;
    //    dispute_bounty_bps = 2_000; committee = honest_verifiers = all 3 revealers.
    //      submitter refunded the FULL budget; catch-bounty = 20% of the SLASHED executor bond
    //      split evenly across the honest verifiers (pay_even ⇒ floor, remainder burned); the rest
    //      of the bond burned; every committee bond returned. No non-revealer ⇒ no extra forfeiture. ──
    let e_bond = MIN_BUDGET;
    let bounty_pool = MIN_BUDGET * DISPUTE_BOUNTY_BPS / 10_000; // bps(e_bond, 2_000) = 200_000
    let bounty_each = bounty_pool / 3; // pay_even across the 3 honest revealers = 66_666
    let verifiers_paid = bounty_each * 3; // 199_998
    let bond_slash_burn = e_bond - verifiers_paid; // non-bounty remainder + rounding = 800_002
    assert_eq!(bounty_pool, 200_000);
    assert_eq!(bounty_each, 66_666);
    assert_eq!(verifiers_paid, 199_998);
    assert_eq!(bond_slash_burn, 800_002);

    let submitter_after = bal(s, submitter);
    let executor_after = bal(s, executor);

    // SUBMITTER made whole: refunded the full budget (Disputed ⇒ no useful work ⇒ full refund).
    assert_eq!(submitter_after, MIN_BUDGET, "submitter fully refunded on Disputed");
    assert_eq!(submitter_after - submitter_before, 0, "submitter net delta 0 (refunded the escrowed budget)");

    // EXECUTOR SLASHED: its whole 1_000_000 bond is gone (bounty + burn) and it earns ZERO worker
    // comp. This is the fraud penalty — contrast the Confirmed test where it kept bond + 850_000.
    assert_eq!(executor_after, 0, "cheating executor slashed to zero (bond gone, no worker comp)");
    assert_eq!(executor_before - executor_after, MIN_BUDGET, "executor lost its entire 1_000_000 bond");

    // HONEST VERIFIERS PAID: each gets its bounty share; the VERIFIER_BOND escrowed at commit is
    // returned (nets zero), so the net balance delta is EXACTLY the bounty share.
    for &v in &verifiers {
        let after = bal(s, v);
        assert_eq!(after, VERIFIER_BOND + bounty_each, "verifier: bond returned + bounty share (66_686)");
        assert_eq!(after - verifier_before, bounty_each, "verifier net delta = its slice of the slashed bond");
    }

    // Protocol burn = the non-bounty remainder of the slashed executor bond (no non-revealer forfeiture).
    assert_eq!(s.total_burned, bond_slash_burn, "burned the non-bounty remainder of the slashed bond");

    // Pot drained; total supply conserved across the whole fraud lifecycle; pot fully accounted for:
    // the drained (budget + e_bond) == refund + bounty + burn.
    assert_eq!(s.escrowed_for_job(&job), 0, "pot drained to 0 at the Disputed terminal");
    assert_eq!(conserved(s), conserved0, "total supply conserved (balances + bonded + burned)");
    assert_eq!(MIN_BUDGET + verifiers_paid + bond_slash_burn, MIN_BUDGET + e_bond, "pot fully accounted");
    assert!(blocks <= 20, "converged quickly (was {})", blocks);
}
