//! da_attestation.rs — the Q15 attestation resolver (Track-2 Phase A). This is the
//! payout-critical seam: it turns a bare on-chain 32-byte `da_root` (all `SubmitJobV2`
//! carries) into the full `DaAttestation` an executor/verifier needs to run
//! `verify_available` and reconstruct a job's `program‖input` bytes. Without it the
//! actor loops ship with `NoAttestationSource` → every resolve is `None` → Abstain →
//! pots refund; WITH it, a Confirmed job's pot can actually pay out.
//!
//! ADDITIVE — no on-chain schema change, no genesis reset. The mechanism (see
//! `da_publisher.rs::publish_job_blob`): the publisher ALSO stores + advertises the
//! serialized `DaAttestation` as a well-known DA object keyed deterministically from
//! `da_root` (`attestation_key(da_root) = sha256(da_root ‖ "/attestation")`). This file
//! is the fetch half:
//!   * [`DaBackedAttestationSource`] implements [`crate::executor_loop::AttestationSource`]:
//!     `resolve(da_root)` recomputes `attestation_key`, fetches that object over the DA
//!     transport (FindProviders + FetchChunk), deserializes it, and rebinds it by
//!     `att.da_root == da_root`. `None` on any failure ⇒ Abstain (honest inert).
//!   * [`BridgeBlobFetcher`] implements [`crate::executor_loop::BlobFetcher`]: given a
//!     resolved attestation, drives the frozen `DataAvailability::verify_available` over
//!     the same transport to fetch + Merkle-verify + RS-reconstruct + `sha256`-rebind the
//!     `program‖input` blob. `Available(bytes)` ⇒ `Some`, `Abstain` ⇒ `None`.
//!
//! SECURITY: the resolver does NOT (cannot) Merkle-verify the attestation object itself —
//! it is the thing being resolved. That is safe because the DOWNSTREAM `verify_available`
//! re-derives full integrity over the coded chunks: every fetched chunk Merkle-verifies
//! against `da_root`, and the reconstructed bytes must satisfy `sha256(recon)==program_id`.
//! A forged/mismatched attestation therefore only ever degrades to Abstain (refund), never
//! a wrong payout. The `att.da_root == da_root` rebind in `resolve` is a cheap early reject.
//!
//! GENERIC over the transport `T: DaTransport` so the exact same code is (a) driven
//! in-process against a `DaStore`-backed transport in tests and (b) instantiated in
//! production over `commputer_pouw_onchain::da_transport::BridgeTransport` (whose
//! `DaCommand`s the PROTECTED event loop services against the libp2p swarm). This
//! supersedes the concrete, inert `executor_loop::BridgeBlobFetcher` as the production
//! blob fetcher; both actor loops re-use the two `executor_loop` traits this file impls.
//!
//! WHERE THIS IS WIRED IN (later, PROTECTED — NOT wired now; this module is inert): the
//! `main.rs` executor/verifier spawn constructs `DaBackedAttestationSource::new(bridge)` +
//! `BridgeBlobFetcher::new(bridge, verifier_id)` (each over a `BridgeTransport` cloned from
//! the shared `da_cmd_tx`) and hands them to `executor_loop::run` / the verifier loop in
//! place of `NoAttestationSource`. FILES NEEDING CHANGES for the live wire-in: `main.rs`
//! (PROTECTED, founder-gated) + `pub mod da_attestation;` in `lib.rs` (this change).

// Inert until the PROTECTED wire-in: no in-tree constructor of these resolvers yet.
#![allow(dead_code)]

use commputer_da::facade::{AvailabilityOutcome, DataAvailability};
use commputer_da::params::DaAttestation;
use commputer_da::transport::DaTransport;
use commputer_pouw_onchain::da_transport::{BridgeTransport, MonotonicClock};

use crate::da_publisher::{attestation_key, deserialize_attestation};
use crate::executor_loop::{
    AttestationSource, BlobFetcher, DEFAULT_DA_MAX_ATTEMPTS_PER_CHUNK,
    DEFAULT_DA_RETRY_WINDOW_TICKS,
};

/// The production [`AttestationSource`]: resolve a bare on-chain `da_root` into the full
/// [`DaAttestation`] by fetching the well-known attestation object `da_publisher` published
/// under [`attestation_key`]. Generic over the DA transport so it is testable in-process
/// (`T` = a `DaStore`-backed transport) and deployable in production (`T` =
/// [`BridgeTransport`]).
pub struct DaBackedAttestationSource<T> {
    bridge: T,
}

impl<T> DaBackedAttestationSource<T> {
    pub fn new(bridge: T) -> Self {
        Self { bridge }
    }
}

impl<T: DaTransport> AttestationSource for DaBackedAttestationSource<T> {
    fn resolve(&self, da_root: [u8; 32]) -> Option<DaAttestation> {
        let key = attestation_key(da_root);
        // Find whoever advertises the attestation object; fetch + deserialize the first
        // one that yields a valid attestation FOR this da_root. (The transport's own
        // response for a coded chunk carries a Merkle path we ignore here — the
        // attestation object is self-identifying and rebinds by da_root; the downstream
        // verify_available supplies real integrity over the coded chunks.)
        for provider in self.bridge.find_providers(key) {
            let Some((bytes, _path)) = self.bridge.fetch_chunk(key, provider) else {
                continue;
            };
            let Some(att) = deserialize_attestation(&bytes) else {
                continue;
            };
            if att.da_root == da_root {
                return Some(att);
            }
        }
        None
    }
}

/// The production [`BlobFetcher`]: reconstruct a job's `program‖input` blob for a resolved
/// attestation by driving the frozen `DataAvailability::verify_available` over a DA
/// transport + a real monotonic clock. Generic over the transport for the same reason as
/// [`DaBackedAttestationSource`]. `Available(bytes)` ⇒ `Some(bytes)`; `Abstain` ⇒ `None`.
pub struct BridgeBlobFetcher<T> {
    bridge: T,
    /// This node's DA identity — one input to the deterministic sampling seed.
    verifier_id: [u8; 32],
    retry_window_ticks: u64,
    max_attempts_per_chunk: u32,
}

impl<T> BridgeBlobFetcher<T> {
    pub fn new(bridge: T, verifier_id: [u8; 32]) -> Self {
        Self {
            bridge,
            verifier_id,
            retry_window_ticks: DEFAULT_DA_RETRY_WINDOW_TICKS,
            max_attempts_per_chunk: DEFAULT_DA_MAX_ATTEMPTS_PER_CHUNK,
        }
    }

    pub fn with_params(
        bridge: T,
        verifier_id: [u8; 32],
        retry_window_ticks: u64,
        max_attempts_per_chunk: u32,
    ) -> Self {
        Self {
            bridge,
            verifier_id,
            retry_window_ticks,
            max_attempts_per_chunk,
        }
    }
}

impl<T: DaTransport> BlobFetcher for BridgeBlobFetcher<T> {
    fn fetch_blob(&self, att: &DaAttestation) -> Option<Vec<u8>> {
        let clock = MonotonicClock::new();
        let da = DataAvailability {
            transport: &self.bridge,
            clock: &clock,
            retry_window_ticks: self.retry_window_ticks,
            max_attempts_per_chunk: self.max_attempts_per_chunk,
        };
        // The sampling seed (job_id, epoch, verifier_id) only picks WHICH chunks are
        // sampled first; for a fully-published blob every sampled chunk is present and
        // reconstruction re-binds via sha256==program_id, so the recovered bytes are
        // seed-independent. Use a stable per-attestation job seed + this node's DA id.
        match da.verify_available(att, att.program_id, 0, self.verifier_id) {
            AvailabilityOutcome::Available(bytes) => Some(bytes),
            AvailabilityOutcome::Abstain => None,
        }
    }
}

/// Convenience aliases for the production instantiation (`T = BridgeTransport`).
pub type BridgeAttestationSource = DaBackedAttestationSource<BridgeTransport>;
pub type BridgeDaBlobFetcher = BridgeBlobFetcher<BridgeTransport>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    use commputer_da::params::ProviderId;
    use commputer_da::transport::MerklePath;

    use crate::da_publisher::{attestation_key, deserialize_merkle_path, publish_job_blob};
    use crate::da_store::DaStore;
    use crate::executor_planner::split_job_blob;

    /// A known-good WASM guest that doubles each input byte (mod 256) — shared with the DA
    /// publisher/executor determinism tests; a program `reexecute` accepts.
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

    fn tmp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "commputer-da-attestation-test-{tag}-{}-{}",
            std::process::id(),
            id
        ))
    }

    /// A `DaTransport` backed by a real on-disk `DaStore`: advertises itself as the sole
    /// provider for any chunk the store holds and serves it (bytes + deserialized Merkle
    /// path) exactly as the production DA backend will over `/commputer/da/1`. Mirrors the
    /// publisher's golden-test transport so the resolver + fetcher run against what the
    /// publisher actually persisted.
    struct StoreTransport<'a> {
        store: &'a DaStore,
        me: ProviderId,
    }

    impl DaTransport for StoreTransport<'_> {
        fn advertise(&self, _chunk_hash: [u8; 32], _me: ProviderId) {}
        fn find_providers(&self, chunk_hash: [u8; 32]) -> Vec<ProviderId> {
            if self.store.has(chunk_hash) {
                vec![self.me]
            } else {
                vec![]
            }
        }
        fn fetch_chunk(
            &self,
            chunk_hash: [u8; 32],
            from: ProviderId,
        ) -> Option<(Vec<u8>, MerklePath)> {
            if from != self.me {
                return None;
            }
            let da_chunk = self.store.get(chunk_hash).ok().flatten()?;
            let path = deserialize_merkle_path(&da_chunk.merkle_path)?;
            Some((da_chunk.bytes, path))
        }
        fn has_chunk(&self, chunk_hash: [u8; 32]) -> bool {
            self.store.has(chunk_hash)
        }
    }

    fn store_transport(store: &DaStore) -> StoreTransport<'_> {
        StoreTransport {
            store,
            me: ProviderId([200u8; 32]),
        }
    }

    /// THE Q15 round-trip that makes payout reachable: publish a job blob (+ its
    /// attestation object) into a DaStore, then from ONLY the bare `da_root`:
    ///   1. `DaBackedAttestationSource::resolve` recovers the full attestation, and
    ///   2. `BridgeBlobFetcher::fetch_blob` reconstructs the EXACT `program‖input` bytes.
    /// This is the seam an executor's `CompleteJob` and a verifier's `Commit`/`Reveal`
    /// re-execution depend on.
    #[test]
    fn q15_resolve_then_fetch_full_round_trip() {
        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = vec![5u8, 10, 15, 200, 255, 1];
        let store = DaStore::open(tmp_dir("q15-rt")).unwrap();
        let published = publish_job_blob(&store, &program, &input).expect("publish succeeds");

        // Stage 1 — the resolver recovers the full attestation from the bare da_root.
        let source = DaBackedAttestationSource::new(store_transport(&store));
        let resolved = source
            .resolve(published.da_root)
            .expect("resolve recovers the attestation from da_root alone");
        assert_eq!(
            resolved, published,
            "resolved attestation equals the published one"
        );

        // Stage 2 — the fetcher reconstructs the exact program‖input blob.
        let fetcher = BridgeBlobFetcher::new(store_transport(&store), [9u8; 32]);
        let blob = fetcher
            .fetch_blob(&resolved)
            .expect("fetch reconstructs the blob");
        let (rec_program, rec_input) =
            split_job_blob(&blob).expect("recovered envelope splits");
        assert_eq!(rec_program, &program[..], "recovered program matches exactly");
        assert_eq!(rec_input, &input[..], "recovered input matches exactly");
    }

    /// NON-VACUITY: with nothing published, `resolve` returns `None` (⇒ Abstain), so the
    /// round-trip above is not passing on thin air.
    #[test]
    fn resolve_returns_none_for_unknown_da_root() {
        let store = DaStore::open(tmp_dir("q15-unknown")).unwrap();
        let source = DaBackedAttestationSource::new(store_transport(&store));
        assert!(
            source.resolve([0xEEu8; 32]).is_none(),
            "no attestation object stored → None → Abstain"
        );
    }

    /// NON-VACUITY: resolution specifically depends on the attestation OBJECT. Remove just
    /// that object (leaving the coded chunks intact) and `resolve` fails — proving stage 1
    /// really fetches the published attestation blob.
    #[test]
    fn resolve_none_after_attestation_object_removed() {
        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = b"gc-att".to_vec();
        let store = DaStore::open(tmp_dir("q15-remove-att")).unwrap();
        let published = publish_job_blob(&store, &program, &input).expect("publish");

        store
            .remove(attestation_key(published.da_root))
            .expect("remove the attestation object only");
        assert!(!store.has(attestation_key(published.da_root)));

        let source = DaBackedAttestationSource::new(store_transport(&store));
        assert!(
            source.resolve(published.da_root).is_none(),
            "attestation object gone → resolve None even though coded chunks remain"
        );
    }

    /// NON-VACUITY: with the coded chunks withheld, `fetch_blob` Abstains (`None`) — the
    /// facade's `Available` result in the round-trip truly came from the store.
    #[test]
    fn fetch_blob_abstains_when_chunks_withheld() {
        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = b"withheld".to_vec();
        let store = DaStore::open(tmp_dir("q15-withheld")).unwrap();
        let published = publish_job_blob(&store, &program, &input).expect("publish");

        // Drop every stored object → the job's bytes leave the store.
        store.gc(&HashSet::new()).unwrap();

        let fetcher = BridgeBlobFetcher::new(store_transport(&store), [9u8; 32]);
        assert!(
            fetcher.fetch_blob(&published).is_none(),
            "no coded chunks held → Abstain → None"
        );
    }

    /// A resolver whose transport serves a *different* job's attestation for the requested
    /// key must reject it (the `att.da_root == da_root` rebind), rather than return a
    /// wrong attestation.
    #[test]
    fn resolve_rejects_mismatched_da_root() {
        // Publish job A, then serve A's attestation under a DIFFERENT (job B) key by
        // storing A's serialized attestation under attestation_key(B).
        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = b"mismatch".to_vec();
        let store = DaStore::open(tmp_dir("q15-mismatch")).unwrap();
        let a = publish_job_blob(&store, &program, &input).expect("publish A");

        let fake_root = [0x77u8; 32];
        // Copy A's attestation object bytes under B's reserved key.
        let a_blob = store
            .get(attestation_key(a.da_root))
            .unwrap()
            .expect("A's attestation object present");
        store
            .put(attestation_key(fake_root), &a_blob)
            .expect("plant A's attestation under B's key");

        let source = DaBackedAttestationSource::new(store_transport(&store));
        assert!(
            source.resolve(fake_root).is_none(),
            "an attestation whose da_root != the requested da_root is rejected"
        );
        // And resolving A's real root still succeeds.
        assert_eq!(source.resolve(a.da_root), Some(a));
    }

    /// Compile-time guarantee the production instantiations are `Send` (the PROTECTED spawn
    /// hands `Box<dyn AttestationSource + Send>` / a `BlobFetcher` to a dedicated OS thread).
    #[test]
    fn production_types_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<BridgeAttestationSource>();
        assert_send::<BridgeDaBlobFetcher>();
    }

    /// Go-live Task B: verify the EXACT production construction path actually enables client-side
    /// fetch pacing. `main.rs` (PROTECTED, not edited by this change) builds the real
    /// `BridgeAttestationSource`/`BridgeDaBlobFetcher` by handing `BridgeTransport::with_timeout`
    /// straight to `DaBackedAttestationSource::new`/`BridgeBlobFetcher::new` — those two
    /// constructors are transport-agnostic (generic over `T: DaTransport`) and never touch the
    /// pacing knob themselves, so the only place the 150ms default can live is inside
    /// `BridgeTransport::with_timeout` (see `da_transport.rs`). This test drives a real
    /// `BridgeTransport` through that exact constructor (not the test-only `StoreTransport` the
    /// other tests in this module use) and confirms two successive `fetch_chunk`s the transport
    /// makes are paced >= 150ms apart — proving the production wiring is genuinely protected from
    /// the server's 10/s/peer `GetChunk` rate limit without any protected-file edit.
    #[test]
    fn production_with_timeout_construction_enables_default_pacing() {
        use commputer_pouw_onchain::da_transport::{BridgeTransport, DaCommand};

        let chunk = [0x55u8; 32];
        let prov = ProviderId([9u8; 32]);
        let bytes = vec![1u8, 2, 3];

        let (tx, rx) = std::sync::mpsc::channel::<DaCommand>();
        let handle = std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    DaCommand::FetchChunk { reply, .. } => {
                        let _ = reply.send(Some((bytes.clone(), Vec::new())));
                    }
                    DaCommand::FindProviders { reply, .. } => {
                        let _ = reply.send(Vec::new());
                    }
                    DaCommand::HasChunk { reply, .. } => {
                        let _ = reply.send(false);
                    }
                    DaCommand::Advertise { .. } => {}
                }
            }
        });

        // The exact call main.rs makes for the production executor/verifier loops.
        let bridge = BridgeTransport::with_timeout(tx, std::time::Duration::from_secs(5));

        let t0 = std::time::Instant::now();
        assert!(bridge.fetch_chunk(chunk, prov).is_some());
        assert!(bridge.fetch_chunk(chunk, prov).is_some());
        assert!(
            t0.elapsed() >= std::time::Duration::from_millis(150),
            "production BridgeTransport::with_timeout construction must default to >=150ms \
             fetch-pacing; only took {:?}",
            t0.elapsed()
        );

        drop(bridge);
        handle.join().unwrap();
    }
}
