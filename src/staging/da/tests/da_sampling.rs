//! Integration: full availability -> Available; >50% withheld -> Abstain; the re-bind
//! security test; reproducibility under a paused clock.
//! New file; no existing-file changes required beyond lib.rs module wiring (sampling+facade).
use commputer_da::commit::{build_attestation, chunk_proof};
use commputer_da::facade::{AvailabilityOutcome, DataAvailability};
use commputer_da::params::{ChunkingParams, ProviderId};
use commputer_da::transport::{InMemoryTransport, ManualClock};
use sha2::{Digest, Sha256};

fn chunk_hash(da_root: [u8; 32], index: u16) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(da_root);
    h.update(index.to_le_bytes());
    h.finalize().into()
}

/// Publish all 2N coded chunks into the transport, held by one provider.
fn publish(t: &InMemoryTransport, prov: ProviderId, att: &commputer_da::params::DaAttestation, coded: &[Vec<u8>]) {
    for i in 0..att.n_total {
        let path = chunk_proof(coded, i);
        t.put(chunk_hash(att.da_root, i), prov, coded[i as usize].clone(), path);
    }
}

fn da<'a>(t: &'a InMemoryTransport, c: &'a ManualClock) -> DataAvailability<'a, InMemoryTransport, ManualClock> {
    DataAvailability { transport: t, clock: c, retry_window_ticks: 1000, max_attempts_per_chunk: 4 }
}

#[test]
fn full_availability_returns_available_bytes() {
    let bytes = b"the people's data, content-addressed and verified".to_vec();
    let (att, coded) = build_attestation(&bytes, &ChunkingParams { chunk_size: 8, params_version: 1 }).unwrap();
    let (t, c) = (InMemoryTransport::new(), ManualClock::new());
    publish(&t, ProviderId([1; 32]), &att, &coded);
    let out = da(&t, &c).verify_available(&att, [7; 32], 0, [42; 32]);
    assert_eq!(out, AvailabilityOutcome::Available(bytes));
}

#[test]
fn majority_withheld_abstains() {
    let bytes = vec![5u8; 200];
    let (att, coded) = build_attestation(&bytes, &ChunkingParams { chunk_size: 8, params_version: 1 }).unwrap();
    let (t, c) = (InMemoryTransport::new(), ManualClock::new());
    publish(&t, ProviderId([1; 32]), &att, &coded);
    // Withhold N+1 chunks so FEWER than N remain — reconstruction is impossible
    // regardless of which indices the seed sampled, so this abstains DETERMINISTICALLY
    // (not merely with probability 1-(1/2)^16). Keeps chunks [0, N-1) = N-1 < N.
    assert!(att.n_data >= 2);
    for i in (att.n_data - 1)..att.n_total {
        t.withhold(chunk_hash(att.da_root, i));
    }
    let out = da(&t, &c).verify_available(&att, [7; 32], 0, [42; 32]);
    assert_eq!(out, AvailabilityOutcome::Abstain);
}

#[test]
fn rebind_rejects_wrong_bytes_under_valid_root() {
    // Build an attestation for bytes A, but serve chunks for a DIFFERENT object B under
    // A's program_id (simulate a malicious publisher rooting non-A data). Reconstruct
    // yields B; sha256(B) != program_id(A) -> Abstain.
    let a = b"genuine program A".to_vec();
    let b = b"malicious program B (same length!)".to_vec();
    let p = ChunkingParams { chunk_size: 8, params_version: 1 };
    let (att_a, _) = build_attestation(&a, &p).unwrap();
    let (att_b, coded_b) = build_attestation(&b, &p).unwrap();
    // forge: claim A's program_id but B's da_root/data/chunks (an honest verifier samples
    // against the da_root it was given; the re-bind is the backstop).
    let forged = commputer_da::params::DaAttestation { program_id: att_a.program_id, ..att_b };
    let (t, c) = (InMemoryTransport::new(), ManualClock::new());
    publish(&t, ProviderId([1; 32]), &forged, &coded_b);
    let out = da(&t, &c).verify_available(&forged, [7; 32], 0, [42; 32]);
    assert_eq!(out, AvailabilityOutcome::Abstain, "re-bind must reject wrong bytes");
}

#[test]
fn abstain_timing_is_clock_driven_and_reproducible() {
    let bytes = vec![9u8; 50];
    let (att, _coded) = build_attestation(&bytes, &ChunkingParams { chunk_size: 8, params_version: 1 }).unwrap();
    let (t, c) = (InMemoryTransport::new(), ManualClock::new()); // nothing published -> all misses
    let out1 = da(&t, &c).verify_available(&att, [7; 32], 0, [42; 32]);
    let out2 = da(&t, &c).verify_available(&att, [7; 32], 0, [42; 32]);
    assert_eq!(out1, AvailabilityOutcome::Abstain);
    assert_eq!(out1, out2, "deterministic under a paused clock");
}

// ---------------------------------------------------------------------------
// Fake-committee harness — proves §7.1 composition WITHOUT touching engine.rs
// ---------------------------------------------------------------------------
//
// Three verifiers each call `resolve_and_populate` PRE-COMMIT (via the adapter).
// Verifier 3 has its chunks withheld so it abstains; verifiers 1 and 2 get full data
// and resolve Available. The toy ledger tracks:
//   - `escrowed`: how many bonds have been put in escrow
//   - `revealed`: which verifiers submitted a reveal (byte-slice of reconstructed data)
//
// Assertions prove the §7.1 model:
//   1. Exactly 2 bonds escrowed (the abstainer never escrowed — `escrowed == 2 * BOND`).
//   2. The two revealing verifiers agree on the same bytes (toy quorum of 2-of-2 reached).
//   3. A toy `total_supply` conservation law holds: the pot is fully accounted for
//      (no value created or destroyed) because the abstainer never participated.
//
// This test does NOT import or modify engine.rs, oracle.rs, settlement.rs, verdict.rs,
// or any pouw file. It is self-contained inside commputer-da.

#[test]
fn fake_committee_harness_proves_sec71_abstain_composition() {
    use commputer_da::adapter::resolve_and_populate;
    use std::collections::HashMap;

    // --- toy ledger --------------------------------------------------------
    // Each verifier starts with STARTING_BALANCE coins. total_supply = 3 * STARTING_BALANCE
    // (exact, no division). On Available, the verifier moves BOND into escrow.
    // On Abstain, no funds move. Conservation: free + escrowed == total_supply at all times.
    const BOND: i64 = 100;
    const STARTING_BALANCE: i64 = 1_000;

    struct ToyLedger {
        balances: HashMap<u8, i64>,  // verifier_id -> balance
        escrowed: i64,               // sum of bonds currently held in escrow
        total_supply: i64,
    }
    impl ToyLedger {
        fn new(verifiers: &[u8], starting_balance: i64) -> Self {
            let mut balances = HashMap::new();
            for &v in verifiers { balances.insert(v, starting_balance); }
            let total_supply = starting_balance * verifiers.len() as i64;
            ToyLedger { balances, escrowed: 0, total_supply }
        }
        fn escrow(&mut self, verifier: u8, amount: i64) {
            *self.balances.get_mut(&verifier).unwrap() -= amount;
            self.escrowed += amount;
        }
        fn conservation_holds(&self) -> bool {
            let free: i64 = self.balances.values().sum();
            free + self.escrowed == self.total_supply
        }
    }

    // --- setup: 3 verifiers, one transport (shared network) ----------------
    let verifier_ids: [u8; 3] = [1, 2, 3];
    let mut ledger = ToyLedger::new(&verifier_ids, STARTING_BALANCE);

    let bytes = b"committee input data - real program bytes".to_vec();
    let params = ChunkingParams { chunk_size: 8, params_version: 1 };
    let (att, coded) = build_attestation(&bytes, &params).unwrap();

    // Two transports: one full, one with withheld chunks.
    // Verifiers 1 & 2 use the full transport; verifier 3 uses the withheld one.
    let t_full = InMemoryTransport::new();
    let t_withheld = InMemoryTransport::new();
    let c = ManualClock::new();

    // Publish all coded chunks into t_full.
    publish(&t_full, ProviderId([99; 32]), &att, &coded);
    // Publish all coded chunks into t_withheld, then remove enough that < N remain
    // so verifier 3 is forced to abstain regardless of which chunks it samples.
    publish(&t_withheld, ProviderId([99; 32]), &att, &coded);
    assert!(att.n_data >= 2);
    for i in (att.n_data - 1)..att.n_total {
        t_withheld.withhold(chunk_hash(att.da_root, i));
    }

    // --- per-verifier pre-commit resolution --------------------------------
    let da_full     = DataAvailability { transport: &t_full,     clock: &c, retry_window_ticks: 1000, max_attempts_per_chunk: 4 };
    let da_withheld = DataAvailability { transport: &t_withheld, clock: &c, retry_window_ticks: 1000, max_attempts_per_chunk: 4 };

    let job_id = [7u8; 32];
    let epoch  = 0u64;
    let mut revealed: Vec<(u8, Vec<u8>)> = Vec::new(); // (verifier_id, reconstructed_bytes)

    for &vid in &verifier_ids {
        let verifier_id = [vid; 32];
        // Verifier 3 sees the withheld transport; 1 & 2 see the full one.
        let participating = if vid == 3 {
            let mut store = None::<Vec<u8>>;
            let ok = resolve_and_populate(&da_withheld, &att, job_id, epoch, verifier_id, |b| { store = Some(b.to_vec()); });
            if ok { revealed.push((vid, store.unwrap())); }
            ok
        } else {
            let mut store = None::<Vec<u8>>;
            let ok = resolve_and_populate(&da_full, &att, job_id, epoch, verifier_id, |b| { store = Some(b.to_vec()); });
            if ok { revealed.push((vid, store.unwrap())); }
            ok
        };

        // On Available: escrow the bond (pre-commit). On Abstain: do nothing.
        if participating {
            ledger.escrow(vid, BOND);
        }
    }

    // --- assertions proving §7.1 ------------------------------------------

    // 1. Exactly 2 bonds escrowed (abstainer never escrowed).
    assert_eq!(ledger.escrowed, 2 * BOND,
        "exactly 2 of 3 verifiers should have escrowed; the abstainer stranded nothing");

    // 2. The two revealing verifiers agree on the reconstructed bytes (toy quorum = 2-of-2).
    assert_eq!(revealed.len(), 2,
        "exactly 2 verifiers should have reached Available and submitted a reveal");
    let (_, ref first_bytes) = revealed[0];
    let (_, ref second_bytes) = revealed[1];
    assert_eq!(first_bytes, second_bytes,
        "both Available verifiers must agree on the same reconstructed bytes (quorum)");
    assert_eq!(first_bytes, &bytes, "reconstructed bytes must match the original");

    // 3. Conservation: no value created or destroyed (the abstainer never moved any funds).
    assert!(ledger.conservation_holds(),
        "toy ledger conservation must hold: free + escrowed == total_supply");
}
