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

use std::collections::HashSet;
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

use commputer_pouw::commit_reveal::make_commitment;
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::params::GameParams;
use commputer_pouw::wasm::WasmLimits;
use commputer_pouw_onchain::consensus_params::PhaseWindows;
use commputer_pouw_onchain::escalation_round::PanelPhase;
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
    // F2 viability gate: this 3-verifier harness has candidates == the drawn committee, so the
    // spare pool is 0 < quorum(k_escalate)=5 ⇒ the Escalate terminal takes the FALLBACK — no
    // second-panel round ever opens (contrast the 9-verifier escalation tests below).
    assert!(s.escalation_rounds.is_empty(), "F2 gate: 0 spare candidates < quorum(7) => fallback, no round");
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

/// COLLUSION PATH (WHITEPAPER §332: "a verifier who rubber-stamps a wrong result forfeits its own
/// bond"). Same fraud setup as the Disputed test — the executor COMPLETES WITH A BOGUS `result_hash`
/// — but now ONE committee verifier COLLUDES: instead of the honest verifier loop (DA-fetch,
/// re-execute, reveal the TRUE hash), it directly commits+reveals the executor's SAME bogus hash.
/// The other two verifiers run the REAL honest loop and reveal the TRUE hash, winning quorum ⇒
/// `Verdict::Disputed`. The colluder is on the losing side, so `settle` → `resolve_disputed` must
/// BURN the colluder's bond (NOT return it), while the two honest verifiers are paid the catch
/// bounty + keep their bonds and the cheating executor is slashed. Supply is conserved every block.
///
/// This is the fix's acceptance proof: BEFORE the fix a wrong-side revealer got its bond back at
/// zero cost (collusion a free option); AFTER, its bond is forfeited (burned).
#[test]
fn pouw_disputed_burns_colluding_verifier_bond() {
    // The bogus result the cheating executor commits to — and the colluding verifier rubber-stamps.
    const BOGUS_HASH: [u8; 32] = [0xABu8; 32];
    // Fixed salt the colluder uses to open its (bogus) commitment.
    const COLLUDER_SALT: [u8; 32] = [0x5Au8; 32];

    let submitter = addr(2);
    let executor = addr(1);
    let verifiers = [addr(3), addr(4), addr(5)];
    let colluder = verifiers[2]; // addr(5): rubber-stamps the executor's wrong hash
    let honest = [verifiers[0], verifiers[1]]; // addr(3), addr(4): re-execute + reveal the TRUE hash

    let mut state = ChainState::new();
    state.phase_windows = PhaseWindows { result_blocks: 3, commit_blocks: 3, reveal_blocks: 3, claim_blocks: 6 };
    state.apply_block(&genesis_block()).unwrap();

    // Identical funding + bonding to the Disputed test.
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
    let submitter_before = bal(&state, submitter);
    let executor_before = bal(&state, executor);
    let honest_before = bal(&state, honest[0]);
    let colluder_before = bal(&state, colluder);
    assert_eq!(
        (submitter_before, executor_before, honest_before, colluder_before),
        (MIN_BUDGET, MIN_BUDGET, VERIFIER_BOND, VERIFIER_BOND)
    );

    // Publish + submit the job (budget escrowed).
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

    // Salt stores only for the 2 HONEST verifiers (the colluder is hand-driven).
    let mut salts: Vec<SaltStore> =
        (0..honest.len()).map(|i| SaltStore::open(scratch(&format!("salt{i}"))).unwrap()).collect();
    let exec_cfg = ExecutorCfg { max_concurrent_claims: 4, min_balance_reserve: 0, executor_bond: EXECUTOR_BOND_FLAT };
    let ver_cfg = VerifierCfg { min_balance_reserve: 0 };

    let (mut claims, mut completes, mut honest_commits, mut honest_reveals, mut blocks) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut cheated = false; // inject the fraudulent CompleteJob exactly once
    let mut colluder_committed = false;
    let mut colluder_revealed = false;

    while (!state.pending_jobs.is_empty() || !state.job_lifecycles.is_empty()) && blocks < 40 {
        let now = state.blocks.height();
        let mut txs: Vec<Transaction> = Vec::new();

        // EXECUTOR: honest CLAIM while pending; then inject ONE bogus CompleteJob.
        if !state.pending_jobs.is_empty() {
            let exec_view = executor_loop::build_chain_view(
                now, 0, executor, bal(&state, executor), &state.pending_jobs, &state.job_lifecycles,
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

        // HONEST verifiers: REAL loop ⇒ re-execute + commit/reveal the TRUE hash.
        for (i, &v) in honest.iter().enumerate() {
            let tick = verifier_loop::build_verifier_views(now, v, bal(&state, v), &state.job_lifecycles);
            for kind in drive_verifier(&store, tick, &mut salts[i], ver_cfg, v.0) {
                match kind {
                    TxKind::Commit { .. } => honest_commits += 1,
                    TxKind::Reveal { .. } => honest_reveals += 1,
                    other => panic!("honest verifier emitted unexpected {other:?}"),
                }
                txs.push(unsigned(v, nonce(&state, v), kind));
            }
        }

        // COLLUDER: hand-inject Commit(bogus) during Committing, Reveal(bogus) during Revealing —
        // it rubber-stamps the executor's WRONG hash instead of re-executing.
        if let Some(lc) = state.job_lifecycles.get(&job) {
            match lc.phase() {
                Phase::Committing if !colluder_committed => {
                    let c = make_commitment(&ParticipantId(colluder.0), &BOGUS_HASH, &COLLUDER_SALT, VERIFIER_BOND);
                    colluder_committed = true;
                    txs.push(unsigned(
                        colluder,
                        nonce(&state, colluder),
                        TxKind::Commit { job_id: job, commit: c.commit, bond: Amount::from_raw(VERIFIER_BOND) },
                    ));
                }
                Phase::Revealing if colluder_committed && !colluder_revealed => {
                    colluder_revealed = true;
                    txs.push(unsigned(
                        colluder,
                        nonce(&state, colluder),
                        TxKind::Reveal { job_id: job, result_hash: BOGUS_HASH, salt: COLLUDER_SALT },
                    ));
                }
                _ => {}
            }
        }

        state.apply_block(&next_block(&state, txs)).unwrap();
        blocks += 1;
        assert_eq!(conserved(&state), conserved0, "money conserved after driven block {blocks}");
    }

    assert!(state.job_lifecycles.is_empty(), "lifecycle settled + drained within the block bound");
    assert!(state.pending_jobs.is_empty(), "no pending record left behind");

    // The loops really drove the collusion: honest claim, one bogus complete, 2 honest commit+reveal
    // pairs (TRUE hash), and the colluder's injected commit+reveal of the bogus hash.
    assert!(claims >= 1, "executor loop emitted a ClaimJob");
    assert!(cheated, "the fraudulent CompleteJob was injected");
    assert_eq!(completes, 1, "exactly one fraudulent CompleteJob");
    assert_eq!(honest_commits, 2, "both honest verifier loops committed the true hash");
    assert_eq!(honest_reveals, 2, "both honest verifier loops revealed the true hash");
    assert!(colluder_committed && colluder_revealed, "the colluder committed + revealed the bogus hash");

    let s = &state;

    // ── The EXACT resolve_disputed split with §332 forfeiture. Inputs: budget = e_bond = 1_000_000,
    //    v_bond = 20, dispute_bounty_bps = 2_000. honest = 2 revealers; wrong_side = the 1 colluder.
    //      submitter refunded the full budget; catch-bounty = 20% of the slashed executor bond split
    //      across the 2 HONEST revealers; the rest of the executor bond burned; the 2 honest bonds
    //      returned; the COLLUDER's bond BURNED (the fix). ──
    let e_bond = MIN_BUDGET;
    let bounty_pool = MIN_BUDGET * DISPUTE_BOUNTY_BPS / 10_000; // 200_000
    let bounty_each = bounty_pool / 2; // pay_even across the 2 honest = 100_000
    let verifiers_paid = bounty_each * 2; // 200_000 (no rounding remainder)
    let exec_bond_burn = e_bond - verifiers_paid; // 800_000
    let total_burn = exec_bond_burn + VERIFIER_BOND; // + the forfeited colluder bond (20) = 800_020
    assert_eq!((bounty_pool, bounty_each, verifiers_paid, exec_bond_burn), (200_000, 100_000, 200_000, 800_000));

    // SUBMITTER made whole (Disputed ⇒ full refund).
    assert_eq!(bal(s, submitter), MIN_BUDGET, "submitter fully refunded on Disputed");

    // EXECUTOR slashed to zero (bond gone, no worker comp).
    assert_eq!(bal(s, executor), 0, "cheating executor slashed to zero");
    assert_eq!(executor_before - bal(s, executor), MIN_BUDGET, "executor lost its entire bond");

    // HONEST verifiers paid: bond returned (nets zero) + bounty share ⇒ net delta = the bounty share.
    for &v in &honest {
        assert_eq!(bal(s, v), VERIFIER_BOND + bounty_each, "honest verifier: bond back + bounty share");
        assert_eq!(bal(s, v) - honest_before, bounty_each, "honest net delta = bounty share");
    }

    // ★ THE FIX: the COLLUDER forfeited its bond — balance 0, a NET LOSS of one VERIFIER_BOND.
    // (BEFORE the fix this would be VERIFIER_BOND: bond returned at zero cost.)
    assert_eq!(bal(s, colluder), 0, "colluding verifier's bond FORFEITED (burned), not returned");
    assert_eq!(colluder_before - bal(s, colluder), VERIFIER_BOND, "colluder net delta = -1 bond (forfeited)");

    // Protocol burn = non-bounty remainder of the slashed executor bond + the forfeited colluder bond.
    assert_eq!(s.total_burned, total_burn, "burn = exec-bond remainder + forfeited colluder bond");

    // Pot drained; supply conserved; pot fully accounted for (budget + e_bond + 3 v_bonds in ==
    // refund + bounty + burns + 2 returned honest bonds out).
    assert_eq!(s.escrowed_for_job(&job), 0, "pot drained to 0 at the Disputed terminal");
    assert_eq!(conserved(s), conserved0, "total supply conserved across the collusion lifecycle");
    assert_eq!(
        MIN_BUDGET + verifiers_paid + total_burn + 2 * VERIFIER_BOND,
        MIN_BUDGET + e_bond + 3 * VERIFIER_BOND,
        "pot fully accounted (in == out)"
    );
    assert!(blocks <= 20, "converged quickly (was {})", blocks);
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// ESCALATION E2E — the second-panel verification round through the REAL loops.
//
// A 9-bonded-verifier harness: round 1 is forced to a 3-way committee split (the 3 DRAWN members
// are HAND-FED three DISTINCT wrong hashes — the fraud path's colluder template — while the
// executor runs the REAL loop and posts its TRUE re-executed hash) ⇒ `compute_verdict` →
// NoQuorum ⇒ `Terminal::Escalate`. With 6 spare candidates ≥ quorum(k_escalate)=5 the F2
// viability gate PASSES and a real `EscalationRound` opens, owning the held pot
// (budget + Be + 3 revealer bonds). The panel is then driven either through the REAL verifier
// loops (`build_verifier_views_with_escalations` ⇒ DA-fetch, re-execute, commit+reveal the TRUE
// hash ⇒ Confirmed) or hand-fed a 3-way split (⇒ the bounded NoQuorum terminal). All money
// asserts mirror the FROZEN `settle_noquorum_*` math, pinned to the audited game by
// `golden_full_panel_matches_frozen_escalation_resolve` (escalation_round.rs).
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// GameParams::default().escalation_reward_bps — the share of SLASHED bonds paid to the panel.
const ESCALATION_REWARD_BPS: u64 = 1_000;

/// Round-1 committee split: three DISTINCT hashes, all ALSO different from the executor's TRUE
/// re-executed hash (asserted against the opened round's record), so ALL THREE round-1 revealers
/// end wrong-side under every panel verdict — the simple all-slashed accounting.
const SPLIT_HASHES: [[u8; 32]; 3] = [[0xB1u8; 32], [0xB2u8; 32], [0xB3u8; 32]];
const SPLIT_SALT: [u8; 32] = [0x51u8; 32];

/// Everything the two escalation tests need once round 1 has split and the round has OPENED.
/// `blocks` carries the driven-block count across the open + panel phases (one shared bound).
struct EscalationOpen {
    state: ChainState,
    store: DaStore,
    /// One durable salt store per verifier, index-aligned with `verifiers` (panel members use
    /// theirs when driven through the real loop; round-1 members are hand-fed and never touch one).
    salts: Vec<SaltStore>,
    job: [u8; 32],
    conserved0: u64,
    submitter: Address,
    executor: Address,
    /// All 9 bonded verifiers (funding order — NOT the draw).
    verifiers: Vec<Address>,
    /// The 3 drawn round-1 committee members, DISCOVERED from state (never hardcoded).
    committee: Vec<Address>,
    /// The drawn escalation panel, DISCOVERED from the opened round (never hardcoded).
    panel: Vec<Address>,
    /// The executor's TRUE re-executed result hash (from the round's record).
    executor_hash: [u8; 32],
    blocks: u32,
}

/// Set up a 9-bonded-verifier chain, publish + submit the job, drive the REAL executor loop
/// (claim + honest complete), hand-feed the drawn round-1 committee a 3-way split, and drive
/// blocks until the primary lifecycle settles Escalate and the F2 gate opens a REAL
/// `EscalationRound`. Asserts conservation after every block and the exact held pot at open.
fn open_escalation_round() -> EscalationOpen {
    let submitter = addr(2);
    let executor = addr(1);
    let verifiers: Vec<Address> = (3u8..12).map(addr).collect(); // 9 bonded verifiers

    let mut state = ChainState::new();
    // Same short windows as drive_round (claim@1 ⇒ result_by 4 / commit_by 7 / reveal_by 10 ⇒
    // primary settles + round opens at height 11 with panel commit_by 13 / reveal_by 16).
    state.phase_windows = PhaseWindows { result_blocks: 3, commit_blocks: 3, reveal_blocks: 3, claim_blocks: 6 };
    state.apply_block(&genesis_block()).unwrap();

    // Identical funding shape to drive_round, with 9 verifiers instead of 3.
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

    let salts: Vec<SaltStore> =
        (0..verifiers.len()).map(|i| SaltStore::open(scratch(&format!("esalt{i}"))).unwrap()).collect();
    let exec_cfg = ExecutorCfg { max_concurrent_claims: 4, min_balance_reserve: 0, executor_bond: EXECUTOR_BOND_FLAT };

    let (mut claims, mut completes, mut blocks) = (0u32, 0u32, 0u32);
    let mut committee: Vec<Address> = Vec::new();
    let mut fed_commit = [false; 3];
    let mut fed_reveal = [false; 3];

    // Drive until the F2 gate opens the round (primary drained into it), or the bound trips.
    while !state.escalation_rounds.contains_key(&job) && blocks < 40 {
        let now = state.blocks.height();
        let mut txs: Vec<Transaction> = Vec::new();

        // EXECUTOR: the REAL loop end-to-end — honest ClaimJob, then the honest CompleteJob
        // carrying its TRUE DA-fetched + re-executed result hash.
        let exec_view = executor_loop::build_chain_view(
            now, 0, executor, bal(&state, executor), &state.pending_jobs, &state.job_lifecycles,
        );
        for kind in drive_executor(&store, exec_view, exec_cfg) {
            match kind {
                TxKind::ClaimJob { .. } => claims += 1,
                TxKind::CompleteJob { .. } => completes += 1,
                other => panic!("executor emitted unexpected {other:?}"),
            }
            txs.push(unsigned(executor, nonce(&state, executor), kind));
        }

        // Discover the drawn round-1 committee from state (populated in the CompleteJob block's
        // apply tail) — membership is a function of block hashes, never hardcoded.
        if committee.is_empty() {
            if let Some(lc) = state.job_lifecycles.get(&job) {
                let rec = lc.to_record();
                if !rec.committee.is_empty() {
                    assert_eq!(rec.committee.len(), 3, "k=3 committee drawn");
                    committee = rec.committee.iter().map(|b| Address(*b)).collect();
                }
            }
        }

        // ROUND-1 COMMITTEE: hand-feed the 3-way split (the fraud path's colluder template) —
        // each member commits then reveals its OWN distinct wrong hash ⇒ NoQuorum ⇒ Escalate.
        if !committee.is_empty() {
            if let Some(lc) = state.job_lifecycles.get(&job) {
                match lc.phase() {
                    Phase::Committing => {
                        for (i, &m) in committee.iter().enumerate() {
                            if !fed_commit[i] {
                                fed_commit[i] = true;
                                let c = make_commitment(
                                    &ParticipantId(m.0), &SPLIT_HASHES[i], &SPLIT_SALT, VERIFIER_BOND,
                                );
                                txs.push(unsigned(m, nonce(&state, m), TxKind::Commit {
                                    job_id: job, commit: c.commit, bond: Amount::from_raw(VERIFIER_BOND),
                                }));
                            }
                        }
                    }
                    Phase::Revealing => {
                        for (i, &m) in committee.iter().enumerate() {
                            if fed_commit[i] && !fed_reveal[i] {
                                fed_reveal[i] = true;
                                txs.push(unsigned(m, nonce(&state, m), TxKind::Reveal {
                                    job_id: job, result_hash: SPLIT_HASHES[i], salt: SPLIT_SALT,
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        state.apply_block(&next_block(&state, txs)).unwrap();
        blocks += 1;
        assert_eq!(conserved(&state), conserved0, "money conserved after driven block {blocks} (round 1)");
    }

    // Round 1 really ran through the loops + the hand-fed split.
    assert!(claims >= 1, "executor loop emitted a ClaimJob");
    assert!(completes >= 1, "executor loop emitted the honest CompleteJob (real DA fetch + re-execute)");
    assert!(
        fed_commit.iter().all(|&x| x) && fed_reveal.iter().all(|&x| x),
        "all three round-1 committee members were hand-fed a distinct commit+reveal"
    );
    assert!(state.pending_jobs.is_empty(), "no pending record left behind");
    assert!(state.job_lifecycles.is_empty(), "primary lifecycle drained into the escalation round");

    // The F2 gate PASSED and the round owns the held pot.
    let round = state.escalation_rounds.get(&job).expect("F2 gate (6 spares >= quorum 5): round opened");
    let rec = round.to_record();
    let executor_hash = rec.executor_hash;
    assert!(
        !SPLIT_HASHES.contains(&executor_hash),
        "all three round-1 split hashes differ from the executor's TRUE hash (all wrong-side)"
    );
    let panel: Vec<Address> = round.panel().iter().map(|p| Address(p.0)).collect();
    let p = GameParams::default();
    assert!(
        panel.len() >= p.quorum(p.k_escalate),
        "panel {} >= quorum(k_escalate) {}", panel.len(), p.quorum(p.k_escalate)
    );
    // The panel is EXACTLY the 6 spare candidates: 9 bonded verifiers minus the 3-member
    // round-1 committee (< k_escalate=7 ⇒ select_committee takes the whole spare pool).
    let spare: HashSet<Address> =
        verifiers.iter().copied().filter(|v| !committee.contains(v)).collect();
    assert_eq!(panel.len(), 6, "panel == the 6 spare candidates");
    assert_eq!(panel.iter().copied().collect::<HashSet<_>>(), spare, "panel set == spare set");
    // Held pot: budget + Be + the 3 round-1 revealer bonds — no money moved at open.
    let e_bond = MIN_BUDGET; // budget.max(GameParams::default().executor_bond) — budget dominates
    assert_eq!(
        state.escrowed_for_job(&job),
        MIN_BUDGET + e_bond + 3 * VERIFIER_BOND,
        "round owns the held pot at open"
    );
    // Pre-panel balance baselines the tests' deltas build on.
    assert_eq!(bal(&state, submitter), 0, "submitter's budget escrowed");
    assert_eq!(bal(&state, executor), 0, "executor's bond escrowed");
    for &m in &committee {
        assert_eq!(bal(&state, m), 0, "round-1 revealer's bond held in the pot");
    }
    for &pm in &panel {
        assert_eq!(bal(&state, pm), VERIFIER_BOND, "spare candidate untouched pre-panel");
    }
    assert_eq!(state.total_burned, 0, "nothing burned before the panel settles");

    EscalationOpen {
        state, store, salts, job, conserved0, submitter, executor, verifiers, committee, panel,
        executor_hash, blocks,
    }
}

/// ESCALATION (a) — the panel CONFIRMS through the REAL loops: every one of the 6 panel members
/// is driven per block via `build_verifier_views_with_escalations` + the REAL
/// `run_verifier_loop` — they DA-fetch the blob, RE-EXECUTE it (deriving the executor's TRUE
/// hash), and commit+reveal it against the round. 6 agreeing reveals ≥ quorum(k_escalate)=5 and
/// matching the executor's hash ⇒ `Verdict::Confirmed` ⇒ the frozen `settle_noquorum_confirmed`:
/// the executor is paid 85% of the budget + bond back; the vindicated-verifier set is EMPTY (all
/// three round-1 revealers were wrong-side) so the 10% pool burns with the 5% slice; the three
/// slashed round-1 bonds fund the panel's escalation reward; the submitter's budget is CONSUMED.
#[test]
fn pouw_escalation_panel_confirms_pays_executor_and_panel() {
    let mut h = open_escalation_round();
    let ver_cfg = VerifierCfg { min_balance_reserve: 0 };
    let (mut commits, mut reveals) = (0u32, 0u32);

    // Drive the PANEL through the REAL verifier loops until the round settles + drains.
    while !h.state.escalation_rounds.is_empty() && h.blocks < 60 {
        let now = h.state.blocks.height();
        let mut txs: Vec<Transaction> = Vec::new();
        for (i, &v) in h.verifiers.iter().enumerate() {
            if h.committee.contains(&v) {
                continue; // round-1 revealers are NOT panel members (their bonds sit slashed in the pot)
            }
            let tick = verifier_loop::build_verifier_views_with_escalations(
                now, v, bal(&h.state, v), &h.state.job_lifecycles, &h.state.escalation_rounds,
            );
            for kind in drive_verifier(&h.store, tick, &mut h.salts[i], ver_cfg, v.0) {
                match kind {
                    TxKind::Commit { .. } => commits += 1,
                    TxKind::Reveal { .. } => reveals += 1,
                    other => panic!("panel verifier emitted unexpected {other:?}"),
                }
                txs.push(unsigned(v, nonce(&h.state, v), kind));
            }
        }
        h.state.apply_block(&next_block(&h.state, txs)).unwrap();
        h.blocks += 1;
        assert_eq!(conserved(&h.state), h.conserved0, "money conserved after driven block {} (panel)", h.blocks);
    }

    let s = &h.state;
    assert!(s.escalation_rounds.is_empty(), "escalation round settled + drained within the block bound");
    assert!(s.job_lifecycles.is_empty() && s.pending_jobs.is_empty(), "no job state left behind");

    // The REAL loops drove the whole panel: 6 commits + 6 reveals of the re-executed TRUE hash.
    let n_panel = h.panel.len() as u64; // 6 (asserted at open)
    assert_eq!(commits, n_panel as u32, "every panel member's REAL loop committed");
    assert_eq!(reveals, n_panel as u32, "every panel member's REAL loop revealed the TRUE hash");

    // ── The EXACT settle_noquorum_confirmed split (frozen settlement.rs:247; the on-chain round
    //    is pinned to it by the golden-oracle test). Inputs: budget = 1_000_000; Be = 1_000_000;
    //    vindicated = [] (ALL THREE round-1 revealers wrong-side); rejected bonds = 3×20;
    //    panel = 6 revealers; escalation_reward_bps = 1_000. ──
    let e_bond = MIN_BUDGET;
    let worker_share = MIN_BUDGET * WORKER_BPS / 10_000; // 850_000
    // No vindicated original verifier ⇒ pay_even over [] pays nothing ⇒ the whole 10% pool
    // burns with the 5% protocol slice: budget_burn = budget − worker − 0 = 15%.
    let budget_burn = MIN_BUDGET - worker_share; // 150_000
    let committee_slash = 3 * VERIFIER_BOND; // 60 — the slashed wrong-side round-1 bonds
    let panel_pool = committee_slash * ESCALATION_REWARD_BPS / 10_000; // 6
    let panel_each = panel_pool / n_panel; // 1
    let panel_paid = panel_each * n_panel; // 6
    let burn = budget_burn + (committee_slash - panel_paid); // 150_000 + 54 = 150_054
    assert_eq!((worker_share, panel_each, burn), (850_000, 1, 150_054));

    // EXECUTOR: bond escrowed at claim nets zero ⇒ net +85% of the budget vs its pre-submit
    // 1_000_000 baseline (funded MIN_BUDGET → 0 at claim → worker share + bond back).
    assert_eq!(bal(s, h.executor), worker_share + e_bond, "executor: 85% of budget + bond back");
    // ROUND-1 COMMITTEE: all three wrong-side ⇒ each lost its verifier bond (pre-round 20 → 0).
    for &m in &h.committee {
        assert_eq!(bal(s, m), 0, "wrong-side round-1 revealer slashed (lost its verifier bond)");
    }
    // PANEL: bond back + its escalation-reward share ⇒ strictly above its pre-round balance.
    assert!(panel_each > 0, "non-vacuous panel reward");
    for &pm in &h.panel {
        assert_eq!(bal(s, pm), VERIFIER_BOND + panel_each, "panel member: bond back + reward share");
        assert!(bal(s, pm) > VERIFIER_BOND, "panel member ended above its pre-round balance");
    }
    // SUBMITTER: Confirmed ⇒ the budget was CONSUMED, not refunded.
    assert_eq!(bal(s, h.submitter), 0, "submitter's budget consumed");
    assert_eq!(s.total_burned, burn, "burn = unpaid 10% pool + 5% slice + slashed-bond remainder");
    assert_eq!(s.escrowed_for_job(&h.job), 0, "pot drained to 0");
    assert_eq!(conserved(s), h.conserved0, "total supply conserved");
    // Pot fully accounted: (budget + Be + 3 slashed bonds + 6 panel bonds) in ==
    // (worker + Be back + 6×(bond back + reward) + burn) out.
    assert_eq!(
        MIN_BUDGET + e_bond + committee_slash + n_panel * VERIFIER_BOND,
        worker_share + e_bond + n_panel * (VERIFIER_BOND + panel_each) + burn,
        "pot fully accounted (in == out)"
    );
    assert!(h.blocks <= 30, "converged (was {})", h.blocks);
}

/// ESCALATION (b) — the panel ALSO splits ⇒ the BOUNDED NoQuorum terminal (no third round).
/// Same 9-verifier open, but the 6 panel members are HAND-FED commits+reveals split 2/2/2
/// across three distinct hashes (none matching the executor's TRUE hash) ⇒ no value reaches
/// quorum(k_escalate)=5 ⇒ `Verdict::NoQuorum` ⇒ the frozen `settle_noquorum_disputed` with
/// honest = [] and rejected = ALL THREE round-1 revealers: the submitter is refunded the full
/// budget; the executor's bond is CONSUMED (10% escalation share to the panel, the 90%
/// remainder burned — the unpaid challenger-reward share burns with it); all three round-1
/// bonds burn; the panel keeps bond + reward.
#[test]
fn pouw_escalation_panel_noquorum_burns_executor_bond() {
    // 2/2/2 over three distinct hashes: the largest agreement class is 2 < quorum(7)=5.
    const PANEL_SPLIT: [[u8; 32]; 3] = [[0xC1u8; 32], [0xC2u8; 32], [0xC3u8; 32]];
    const PANEL_SALT: [u8; 32] = [0x77u8; 32];

    let mut h = open_escalation_round();
    assert!(
        !PANEL_SPLIT.contains(&h.executor_hash),
        "panel split hashes all differ from the executor's TRUE hash"
    );

    let panel = h.panel.clone();
    let mut fed_commit = vec![false; panel.len()];
    let mut fed_reveal = vec![false; panel.len()];

    // Hand-feed the panel split, driving blocks until the round settles + drains.
    while !h.state.escalation_rounds.is_empty() && h.blocks < 60 {
        let mut txs: Vec<Transaction> = Vec::new();
        if let Some(er) = h.state.escalation_rounds.get(&h.job) {
            match er.phase() {
                PanelPhase::Committing => {
                    for (i, &m) in panel.iter().enumerate() {
                        if !fed_commit[i] {
                            fed_commit[i] = true;
                            let c = make_commitment(
                                &ParticipantId(m.0), &PANEL_SPLIT[i % 3], &PANEL_SALT, VERIFIER_BOND,
                            );
                            txs.push(unsigned(m, nonce(&h.state, m), TxKind::Commit {
                                job_id: h.job, commit: c.commit, bond: Amount::from_raw(VERIFIER_BOND),
                            }));
                        }
                    }
                }
                PanelPhase::Revealing => {
                    for (i, &m) in panel.iter().enumerate() {
                        if fed_commit[i] && !fed_reveal[i] {
                            fed_reveal[i] = true;
                            txs.push(unsigned(m, nonce(&h.state, m), TxKind::Reveal {
                                job_id: h.job, result_hash: PANEL_SPLIT[i % 3], salt: PANEL_SALT,
                            }));
                        }
                    }
                }
                PanelPhase::Settled => {}
            }
        }
        h.state.apply_block(&next_block(&h.state, txs)).unwrap();
        h.blocks += 1;
        assert_eq!(conserved(&h.state), h.conserved0, "money conserved after driven block {} (panel)", h.blocks);
    }

    let s = &h.state;
    assert!(s.escalation_rounds.is_empty(), "escalation round settled + drained within the block bound");
    assert!(
        fed_commit.iter().all(|&x| x) && fed_reveal.iter().all(|&x| x),
        "every panel member committed + revealed its split hash"
    );

    // ── The EXACT bounded-terminal split: EscalationRound::settle's NoQuorum arm calls the
    //    frozen settle_noquorum_disputed (settlement.rs:305) with honest = [] and rejected =
    //    the WHOLE round-1 committee. Inputs: budget = Be = 1_000_000; challenger_reward_bps =
    //    escalation_reward_bps = 1_000; panel = 6 revealers; rejected bonds = 3×20.
    //    NB the brief's "burned ≥ Be" over-approximates: 10% of the slashed Be pays the panel,
    //    so the EXACT burn is Be − panel_paid + committee bonds = 900_064 < Be. ──
    let n_panel = panel.len() as u64; // 6
    let e_bond = MIN_BUDGET;
    let panel_pool = e_bond * ESCALATION_REWARD_BPS / 10_000; // 100_000
    let panel_each = panel_pool / n_panel; // 16_666
    let panel_paid = panel_each * n_panel; // 99_996
    let honest_paid = 0u64; // challenger-reward share has no recipient (no honest round-1 revealer)
    let executor_burn = e_bond - honest_paid - panel_paid; // 900_004
    let committee_slash = 3 * VERIFIER_BOND; // 60 — all three round-1 revealers slashed
    let burn = executor_burn + committee_slash; // 900_064
    assert_eq!((panel_each, executor_burn, burn), (16_666, 900_004, 900_064));

    // SUBMITTER made whole on the bounded terminal.
    assert_eq!(bal(s, h.submitter), MIN_BUDGET, "submitter refunded the full budget");
    // EXECUTOR: bond CONSUMED — no bond back, no worker pay (pre-submit 1_000_000 → 0).
    assert_eq!(bal(s, h.executor), 0, "executor's bond consumed (no bond back, no worker pay)");
    // ROUND-1 COMMITTEE: the whole committee is slashed on the bounded terminal.
    for &m in &h.committee {
        assert_eq!(bal(s, m), 0, "round-1 revealer slashed (lost its verifier bond)");
    }
    // PANEL: bond back + reward from the slashed executor bond ⇒ above pre-round balance.
    for &pm in &panel {
        assert_eq!(bal(s, pm), VERIFIER_BOND + panel_each, "panel member: bond back + reward share");
        assert!(bal(s, pm) >= VERIFIER_BOND, "panel member ended >= its pre-round balance");
    }
    assert_eq!(s.total_burned, burn, "burn = Be minus the panel share, plus the 3 slashed round-1 bonds");
    assert_eq!(s.escrowed_for_job(&h.job), 0, "pot drained to 0");
    assert_eq!(conserved(s), h.conserved0, "total supply conserved");
    // Pot fully accounted: (budget + Be + 3 slashed bonds + 6 panel bonds) in ==
    // (refund + 6×(bond back + reward) + burn) out.
    assert_eq!(
        MIN_BUDGET + e_bond + committee_slash + n_panel * VERIFIER_BOND,
        MIN_BUDGET + n_panel * (VERIFIER_BOND + panel_each) + burn,
        "pot fully accounted (in == out)"
    );
    assert!(h.blocks <= 30, "converged (was {})", h.blocks);
}
