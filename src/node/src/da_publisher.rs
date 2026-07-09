// da_publisher.rs — the PoUW data-availability PUBLISHER (Track-2 Phase 0, DA substrate).
//
// WHAT: the one node-side step that makes a job's bytes ENTER the DA network. A
// submitter's node runs `publish_job_blob` before it emits the on-chain
// `SubmitJobV2` (which carries only `da_root`, not the bytes). It:
//   1. encodes the `program‖input` envelope as ONE blob (founder decision Q1) via
//      `executor_planner::encode_job_blob` — `[program_len:u32 LE][program][input]`;
//   2. runs the frozen da-crate `build_attestation` over that envelope to get the
//      `DaAttestation` (whose `da_root` the tx will anchor, and whose `program_id`
//      is `sha256(envelope)` — the re-bind identity a fetch checks) plus the 2N
//      systematic-RS coded chunks;
//   3. persists EVERY coded chunk into the node-local `DaStore` as a
//      `DaChunk{ bytes, merkle_path }`, keyed by its TRANSPORT chunk_hash
//      `sha256(da_root ‖ index_le)` (== `commputer_da::facade::chunk_hash`), so a
//      later `DataAvailability::verify_available` fetch resolves + Merkle-verifies +
//      RS-reconstructs + sha256-rebinds the exact same envelope. The `merkle_path`
//      is the inclusion path serialized in the `LocalDiskTransport` on-disk shape
//      (a `Vec<Option<[u8;32]>>`), which is exactly what the wire `DaChunk` carries.
//
// This mirrors the frozen golden `pouw-e2e/world.rs::publish` (build_attestation +
// `transport.put` of all 2N chunks with their `chunk_proof` paths, keyed by
// `chunk_hash(&att,i)`) — but persists to the real on-disk `DaStore` instead of the
// test `InMemoryTransport`, and covers the WHOLE `program‖input` envelope rather
// than the program alone (world.rs held input out-of-band).
//
//   4. (Q15) ALSO persists the SERIALIZED `DaAttestation` itself as a well-known DA
//      object under `attestation_key(da_root) = sha256(da_root ‖ "/attestation")`, so a
//      node holding ONLY the bare on-chain `da_root` can resolve the full attestation it
//      needs to run `verify_available` (see `da_attestation.rs`). Additive — a new DA
//      object, no `SubmitJobV2` schema change, no reset.
//
// `live_chunk_hashes(&att)` returns the set of transport keys a job's chunks + its
// attestation blob live under, so the `DaStore::gc` caller can scope retention to active
// jobs (it must retain the attestation blob for the life of the job).
//
// The DA outcome is NEVER hashed into consensus (it degrades to Abstain), so nothing
// here is consensus- or fork-relevant; this is pure local plumbing that is
// deterministic (no wall-clock, no rng) so every node re-encoding the same bytes
// agrees on `da_root` and on every chunk key.
//
// WIRING (INERT until the PROTECTED wire-in — no in-tree caller yet): main.rs
// constructs the `DaStore`; the event_loop / a submit path calls `publish_job_blob`
// when a job's bytes must be made available, advertises the returned live chunk
// hashes over `/commputer/da/1`, and gc()s scoped to `live_chunk_hashes` at
// settlement. Nothing here is wired into the running node yet.
// FILES NEEDING CHANGES (later, gated): node/src/main.rs (construct the store),
// node/src/event_loop.rs (PROTECTED: call publish + advertise + gc), and the DA
// backend that serves `DaRequest::GetChunk` from `DaStore::get`.

// Inert until the PROTECTED wire-in: no in-tree callers of the publisher yet.
#![allow(dead_code)]

use std::collections::HashSet;

use commputer_da::commit::{build_attestation, chunk_proof};
use commputer_da::facade::chunk_hash;
use commputer_da::params::{ChunkingParams, DaAttestation, DaError};
use commputer_network::da_protocol::DaChunk;
use sha2::{Digest, Sha256};

use crate::da_store::DaStore;
use crate::executor_planner::encode_job_blob;

/// Domain-separation suffix for the reserved DA object that holds a job's serialized
/// [`DaAttestation`]. Distinct from the `index_le` suffix `chunk_hash` uses, so an
/// attestation blob key can never collide with a coded-chunk key.
const ATTESTATION_KEY_SUFFIX: &[u8] = b"/attestation";

/// Everything that can go wrong publishing a job blob into the DA store.
#[derive(Debug)]
pub enum PublishError {
    /// The da crate could not build the attestation over the envelope. The dominant
    /// cause is `DaError::TooLarge`: an envelope needing > 128 data chunks exceeds the
    /// GF(2^8) rate-1/2 ceiling (~8 MiB at the 64 KiB default chunk size).
    Encode(DaError),
    /// The blob store rejected or failed to persist a coded chunk (per-chunk size cap,
    /// total-store byte budget, or an underlying I/O error).
    Store(std::io::Error),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Encode(e) => write!(f, "da attestation build failed: {e:?}"),
            PublishError::Store(e) => write!(f, "da chunk store failed: {e}"),
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PublishError::Encode(_) => None,
            PublishError::Store(e) => Some(e),
        }
    }
}

impl From<DaError> for PublishError {
    fn from(e: DaError) -> Self {
        PublishError::Encode(e)
    }
}

impl From<std::io::Error> for PublishError {
    fn from(e: std::io::Error) -> Self {
        PublishError::Store(e)
    }
}

/// Publish a job's `program‖input` envelope into the node-local DA store so the bytes
/// enter the DA network. Returns the `DaAttestation` (the caller anchors its `da_root`
/// on-chain via `SubmitJobV2`). Deterministic: the same `program`/`input` always yield
/// the same `da_root` and the same chunk keys.
///
/// The chunking policy is the consensus-anchored DA default (`ChunkingParams::default`
/// = 64 KiB chunks, `params_version` 1) — the same policy every other node re-encodes
/// under, so `da_root` agrees network-wide. (Should the DA params ever become
/// node-configurable, this is where the compile-anchored params would be threaded.)
pub fn publish_job_blob(
    store: &DaStore,
    program: &[u8],
    input: &[u8],
) -> Result<DaAttestation, PublishError> {
    // (1) One envelope = program‖input (founder Q1).
    let envelope = encode_job_blob(program, input);

    // (2) Build the attestation + 2N coded chunks over the WHOLE envelope. `program_id`
    //     in the returned attestation is `sha256(envelope)` — the re-bind identity.
    let params = ChunkingParams::default();
    let (att, coded) = build_attestation(&envelope, &params)?;

    // (3) Persist every coded chunk, keyed by its transport chunk_hash so a later
    //     `verify_available` fetch (which addresses by `chunk_hash(&att, index)`)
    //     resolves it. Each stored `DaChunk` carries the chunk bytes plus the
    //     serialized Merkle inclusion path proving `bytes` sits at `index` under
    //     `da_root`.
    for index in 0..att.n_total {
        let key = chunk_hash(&att, index);
        let path = chunk_proof(&coded, index);
        let da_chunk = DaChunk {
            bytes: coded[index as usize].clone(),
            merkle_path: serialize_merkle_path(&path),
        };
        store.put(key, &da_chunk)?;
    }

    // (4) Q15 — ALSO publish the serialized `DaAttestation` itself as a well-known DA
    //     object keyed deterministically from `da_root` (`attestation_key`). This is the
    //     additive resolution channel: a fetcher holding ONLY the bare on-chain 32-byte
    //     `da_root` (all `SubmitJobV2` carries) fetches this object, deserializes it, and
    //     recovers the full attestation (`program_id`, `n_data`, `n_total`, `data_len`, …)
    //     it needs to run `verify_available`. No on-chain schema change, no reset — just a
    //     new DA object. The attestation crate (`DaAttestation`) does NOT derive
    //     Serialize/Deserialize (it is `#[derive(Clone, Copy, Debug, PartialEq, Eq)]`
    //     only), so its 7 fields are hand-serialized into a fixed 82-byte layout by
    //     `serialize_attestation`. Stored as a single raw `DaChunk` with a trivial (empty)
    //     Merkle path: the object is self-identifying (the resolver rebinds it by checking
    //     `att.da_root == da_root`), and the downstream `verify_available` re-derives real
    //     integrity over the coded chunks (Merkle-path + `sha256(recon)==program_id`), so a
    //     forged attestation only ever degrades to Abstain, never a wrong payout.
    let attestation_blob = DaChunk {
        bytes: serialize_attestation(&att),
        merkle_path: serialize_merkle_path(&[]),
    };
    store.put(attestation_key(att.da_root), &attestation_blob)?;

    Ok(att)
}

/// The reserved DA-object key under which a job's serialized [`DaAttestation`] is stored
/// and advertised: `sha256(da_root ‖ "/attestation")`. Deterministic and
/// domain-separated from every coded-chunk key (`chunk_hash` suffixes `index_le`, this
/// suffixes the ASCII tag), so the attestation blob can never collide with chunk `0`.
/// A [`crate::da_attestation::DaBackedAttestationSource`] recomputes this from a bare
/// on-chain `da_root` to resolve the full attestation (Q15).
pub fn attestation_key(da_root: [u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(da_root);
    h.update(ATTESTATION_KEY_SUFFIX);
    h.finalize().into()
}

/// Serialize a [`DaAttestation`] into a fixed 82-byte layout (all multi-byte fields
/// little-endian). `DaAttestation` derives no `Serialize`/`Deserialize`, so this is the
/// canonical on-DA encoding of the attestation blob. Layout:
///   `[program_id:32][da_root:32][data_len:u64][chunk_size:u32][n_data:u16][n_total:u16][params_version:u16]`.
pub fn serialize_attestation(att: &DaAttestation) -> Vec<u8> {
    let mut buf = Vec::with_capacity(82);
    buf.extend_from_slice(&att.program_id);
    buf.extend_from_slice(&att.da_root);
    buf.extend_from_slice(&att.data_len.to_le_bytes());
    buf.extend_from_slice(&att.chunk_size.to_le_bytes());
    buf.extend_from_slice(&att.n_data.to_le_bytes());
    buf.extend_from_slice(&att.n_total.to_le_bytes());
    buf.extend_from_slice(&att.params_version.to_le_bytes());
    buf
}

/// Inverse of [`serialize_attestation`]. Returns `None` on any length mismatch so a
/// corrupt/hostile attestation blob cannot panic the resolver — it simply fails to
/// resolve → the loop Abstains (honest inert).
pub fn deserialize_attestation(raw: &[u8]) -> Option<DaAttestation> {
    if raw.len() != 82 {
        return None;
    }
    let program_id: [u8; 32] = raw[0..32].try_into().ok()?;
    let da_root: [u8; 32] = raw[32..64].try_into().ok()?;
    let data_len = u64::from_le_bytes(raw[64..72].try_into().ok()?);
    let chunk_size = u32::from_le_bytes(raw[72..76].try_into().ok()?);
    let n_data = u16::from_le_bytes(raw[76..78].try_into().ok()?);
    let n_total = u16::from_le_bytes(raw[78..80].try_into().ok()?);
    let params_version = u16::from_le_bytes(raw[80..82].try_into().ok()?);
    Some(DaAttestation {
        program_id,
        da_root,
        data_len,
        chunk_size,
        n_data,
        n_total,
        params_version,
    })
}

/// The set of transport keys a published job lives under: every coded-chunk key
/// (`sha256(da_root ‖ index_le)` for `index` in `[0, n_total)`) PLUS the reserved
/// attestation-blob key ([`attestation_key`]). A `DaStore::gc` caller unions these across
/// its active jobs to scope retention — the attestation blob MUST be retained so a fetcher
/// can still resolve `da_root → DaAttestation` (Q15) for the life of the job.
pub fn live_chunk_hashes(attestation: &DaAttestation) -> HashSet<[u8; 32]> {
    let mut set: HashSet<[u8; 32]> = (0..attestation.n_total)
        .map(|index| chunk_hash(attestation, index))
        .collect();
    set.insert(attestation_key(attestation.da_root));
    set
}

/// Serialize a Merkle inclusion path into the `LocalDiskTransport` on-disk shape — the
/// exact bytes the wire/stored `DaChunk.merkle_path` carries:
///   `[path_len: u32 LE]` then per element `[present: u8]` (`0x01` Some / `0x00` None),
///   and for a Some element the 32-byte sibling hash.
pub fn serialize_merkle_path(path: &[Option<[u8; 32]>]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + path.len() * 33);
    buf.extend_from_slice(&(path.len() as u32).to_le_bytes());
    for elem in path {
        match elem {
            Some(h) => {
                buf.push(0x01);
                buf.extend_from_slice(h);
            }
            None => buf.push(0x00),
        }
    }
    buf
}

/// Inverse of [`serialize_merkle_path`]. Returns `None` on any structural mismatch
/// (truncation, an invalid present-tag, or trailing garbage) so a corrupt/hostile
/// `merkle_path` cannot panic the fetch path — the DA facade then simply Abstains.
pub fn deserialize_merkle_path(raw: &[u8]) -> Option<Vec<Option<[u8; 32]>>> {
    let mut pos = 0usize;
    if pos + 4 > raw.len() {
        return None;
    }
    let path_len = u32::from_le_bytes(raw[pos..pos + 4].try_into().ok()?) as usize;
    pos += 4;
    let mut path = Vec::with_capacity(path_len);
    for _ in 0..path_len {
        if pos >= raw.len() {
            return None;
        }
        let present = raw[pos];
        pos += 1;
        match present {
            0x01 => {
                if pos + 32 > raw.len() {
                    return None;
                }
                let mut h = [0u8; 32];
                h.copy_from_slice(&raw[pos..pos + 32]);
                pos += 32;
                path.push(Some(h));
            }
            0x00 => path.push(None),
            _ => return None, // invalid present-tag
        }
    }
    if pos != raw.len() {
        return None; // trailing garbage / truncation
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor_planner::split_job_blob;
    use crate::pouw_executor::execute_job;
    use commputer_da::facade::{AvailabilityOutcome, DataAvailability};
    use commputer_da::params::ProviderId;
    use commputer_da::transport::{DaTransport, ManualClock, MerklePath};
    use commputer_pouw::wasm::WasmLimits;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    /// A valid WASM guest that doubles each input byte (mod 256). Shared with the
    /// executor_planner determinism test — a known-good program `execute_job` accepts.
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
            "commputer-da-publisher-test-{tag}-{}-{}",
            std::process::id(),
            id
        ))
    }

    /// A `DaTransport` backed by a real on-disk `DaStore`: it advertises itself as the
    /// sole provider for any chunk the store holds and serves it (bytes + deserialized
    /// Merkle path) exactly as the production DA backend will over `/commputer/da/1`.
    /// This lets the frozen da-crate facade drive `verify_available` against what the
    /// publisher actually persisted — the golden fetch path in-process.
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
        fn fetch_chunk(&self, chunk_hash: [u8; 32], from: ProviderId) -> Option<(Vec<u8>, MerklePath)> {
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

    /// THE golden round-trip: publish a real program+input envelope → persist its coded
    /// chunks → fetch them back through the REAL da-crate facade over a DaStore-backed
    /// transport → reconstruct + split the envelope → assert the recovered program+input
    /// match byte-for-byte AND that `execute_job` over the recovered bytes yields the
    /// same `result_hash` as running `execute_job` over the originals. This reproduces
    /// world.rs's happy path (publish → gate_pool → verify) in-process, plus the
    /// executor's re-execution seam the on-chain pay-out depends on.
    #[test]
    fn publish_store_fetch_split_execute_round_trip() {
        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = vec![1u8, 2, 3, 40, 250, 128, 7];
        let program_hash: [u8; 32] = Sha256::digest(&program).into();
        let input_hash: [u8; 32] = Sha256::digest(&input).into();

        // Publish into a fresh on-disk store.
        let store = DaStore::open(tmp_dir("golden")).unwrap();
        let att = publish_job_blob(&store, &program, &input).expect("publish succeeds");

        // The attestation binds sha256(envelope), and every live chunk is on disk.
        let envelope = encode_job_blob(&program, &input);
        let expect_id: [u8; 32] = Sha256::digest(&envelope).into();
        assert_eq!(att.program_id, expect_id, "program_id must be sha256(envelope)");
        let live = live_chunk_hashes(&att);
        assert_eq!(
            live.len(),
            att.n_total as usize + 1,
            "one live key per coded chunk PLUS the reserved attestation-blob key"
        );
        assert!(
            live.contains(&attestation_key(att.da_root)),
            "the attestation key is in the live set so gc retains it"
        );
        for &key in &live {
            assert!(store.has(key), "every live chunk must be persisted");
        }

        // Fetch back through the frozen da-crate facade over the DaStore — the exact
        // path a verifier's verify_available runs at production time.
        let transport = StoreTransport {
            store: &store,
            me: ProviderId([200u8; 32]),
        };
        let clock = ManualClock::new();
        let da = DataAvailability {
            transport: &transport,
            clock: &clock,
            retry_window_ticks: 1_000,
            max_attempts_per_chunk: 8,
        };
        let recovered_envelope = match da.verify_available(&att, [7u8; 32], 1, [11u8; 32]) {
            AvailabilityOutcome::Available(bytes) => bytes,
            AvailabilityOutcome::Abstain => panic!("a fully-published blob must be DA-available"),
        };

        // Split the envelope back into program + input; both must match exactly.
        let (rec_program, rec_input) =
            split_job_blob(&recovered_envelope).expect("recovered envelope splits");
        assert_eq!(rec_program, &program[..], "recovered program matches");
        assert_eq!(rec_input, &input[..], "recovered input matches");

        // THE seam: executing the DA-recovered bytes reproduces the direct result_hash.
        let via_da = execute_job(
            program_hash,
            input_hash,
            rec_program,
            rec_input,
            WasmLimits::default(),
        )
        .expect("recovered program executes");
        let direct = execute_job(
            program_hash,
            input_hash,
            &program,
            &input,
            WasmLimits::default(),
        )
        .expect("original program executes");
        assert_eq!(
            via_da, direct,
            "publish→store→fetch→split→execute must reproduce the executor result_hash"
        );
    }

    /// NON-VACUITY: the facade's Available result truly came from the store. After
    /// gc()'ing every chunk away, the same fetch Abstains — proving the golden test
    /// above is not passing on thin air.
    #[test]
    fn withheld_chunks_make_the_blob_unavailable() {
        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = b"input-bytes".to_vec();
        let store = DaStore::open(tmp_dir("withhold")).unwrap();
        let att = publish_job_blob(&store, &program, &input).expect("publish succeeds");

        // Drop every chunk (empty live set) — the job's bytes AND its attestation blob
        // leave the store (n_total coded chunks + 1 attestation object).
        let removed = store.gc(&HashSet::new()).unwrap();
        assert_eq!(
            removed,
            att.n_total as usize + 1,
            "all coded chunks + the attestation blob gc'd"
        );

        let transport = StoreTransport {
            store: &store,
            me: ProviderId([200u8; 32]),
        };
        let clock = ManualClock::new();
        let da = DataAvailability {
            transport: &transport,
            clock: &clock,
            retry_window_ticks: 1_000,
            max_attempts_per_chunk: 8,
        };
        assert_eq!(
            da.verify_available(&att, [7u8; 32], 1, [11u8; 32]),
            AvailabilityOutcome::Abstain,
            "with no chunks held the blob must be unavailable"
        );
    }

    /// Publishing the same program+input is deterministic: identical `da_root`,
    /// `program_id`, `n_total`, and identical live chunk-key set (determinism is
    /// sacred — every node must agree on da_root and every chunk address).
    #[test]
    fn publish_is_deterministic() {
        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = vec![9u8, 8, 7, 6];

        let store_a = DaStore::open(tmp_dir("det-a")).unwrap();
        let store_b = DaStore::open(tmp_dir("det-b")).unwrap();
        let att_a = publish_job_blob(&store_a, &program, &input).expect("publish a");
        let att_b = publish_job_blob(&store_b, &program, &input).expect("publish b");

        assert_eq!(att_a.da_root, att_b.da_root, "da_root must be deterministic");
        assert_eq!(att_a.program_id, att_b.program_id);
        assert_eq!(att_a.n_total, att_b.n_total);
        assert_eq!(
            live_chunk_hashes(&att_a),
            live_chunk_hashes(&att_b),
            "chunk key set must be deterministic"
        );
    }

    /// The Merkle-path serialization round-trips through its inverse, including the
    /// Some/None mix a promoted-lone-node path produces. This is the encoding the
    /// stored/wire `DaChunk.merkle_path` carries, so the fetch path can reconstruct it.
    #[test]
    fn merkle_path_serde_round_trips() {
        let path: Vec<Option<[u8; 32]>> = vec![Some([0xab; 32]), None, Some([0xcd; 32]), None];
        let bytes = serialize_merkle_path(&path);
        let back = deserialize_merkle_path(&bytes).expect("well-formed path deserializes");
        assert_eq!(back, path, "merkle path must round-trip exactly");

        // An empty path (single-leaf tree) round-trips too.
        let empty: Vec<Option<[u8; 32]>> = Vec::new();
        assert_eq!(
            deserialize_merkle_path(&serialize_merkle_path(&empty)),
            Some(empty)
        );
    }

    /// Malformed serialized paths are rejected (return None), never panicked on: a
    /// truncated prefix, a declared length overrunning the buffer, an invalid tag, and
    /// trailing garbage.
    #[test]
    fn deserialize_merkle_path_rejects_malformed() {
        assert!(deserialize_merkle_path(&[]).is_none()); // < 4-byte prefix
        assert!(deserialize_merkle_path(&[0, 1, 2]).is_none()); // < 4-byte prefix
        // declares 1 element but no element byte follows.
        assert!(deserialize_merkle_path(&1u32.to_le_bytes()).is_none());
        // invalid present-tag (0x02).
        let mut bad_tag = 1u32.to_le_bytes().to_vec();
        bad_tag.push(0x02);
        assert!(deserialize_merkle_path(&bad_tag).is_none());
        // valid single None element + one trailing garbage byte.
        let mut trailing = 1u32.to_le_bytes().to_vec();
        trailing.push(0x00);
        trailing.push(0xFF);
        assert!(deserialize_merkle_path(&trailing).is_none());
    }

    /// The published chunks Merkle-verify against the attestation via the frozen
    /// da-crate `verify_chunk` — the same check `verify_available`'s `fetch_verified`
    /// applies before accepting a fetched chunk. Proves the stored (bytes, path) pair
    /// is a valid inclusion proof, not just round-trippable bytes.
    #[test]
    fn stored_chunks_merkle_verify_against_attestation() {
        use commputer_da::commit::verify_chunk;

        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = b"another-input".to_vec();
        let store = DaStore::open(tmp_dir("merkle-verify")).unwrap();
        let att = publish_job_blob(&store, &program, &input).expect("publish succeeds");

        for index in 0..att.n_total {
            let key = chunk_hash(&att, index);
            let da_chunk = store.get(key).unwrap().expect("chunk present");
            let path = deserialize_merkle_path(&da_chunk.merkle_path).expect("path deserializes");
            assert!(
                verify_chunk(&att, index, &da_chunk.bytes, &path),
                "chunk {index} must Merkle-verify under da_root"
            );
        }
    }

    /// Q15: `publish_job_blob` ALSO persists the serialized attestation under the reserved
    /// `attestation_key`, and fetching that blob back deserializes to the exact original
    /// `DaAttestation`. This is the additive resolution channel the on-chain `da_root`
    /// (all `SubmitJobV2` carries) resolves through.
    #[test]
    fn publish_stores_resolvable_attestation_blob() {
        let program = wat::parse_str(DOUBLER).expect("guest assembles");
        let input = b"attestation-resolve".to_vec();
        let store = DaStore::open(tmp_dir("att-blob")).unwrap();
        let att = publish_job_blob(&store, &program, &input).expect("publish succeeds");

        let key = attestation_key(att.da_root);
        assert!(
            store.has(key),
            "the attestation blob must be persisted under attestation_key"
        );
        assert!(
            live_chunk_hashes(&att).contains(&key),
            "the attestation key must be gc-retained"
        );

        // Fetch the blob back and deserialize → equals the original attestation.
        let blob = store.get(key).unwrap().expect("attestation blob present");
        let recovered =
            deserialize_attestation(&blob.bytes).expect("attestation blob deserializes");
        assert_eq!(
            recovered, att,
            "the fetched attestation blob equals the published attestation"
        );
    }

    /// The attestation field-serialization round-trips exactly and never panics on a
    /// malformed (wrong-length) buffer.
    #[test]
    fn attestation_serde_round_trips() {
        let att = DaAttestation {
            program_id: [3u8; 32],
            da_root: [7u8; 32],
            data_len: 123_456,
            chunk_size: 65_536,
            n_data: 5,
            n_total: 10,
            params_version: 1,
        };
        let bytes = serialize_attestation(&att);
        assert_eq!(bytes.len(), 82, "fixed 82-byte layout");
        assert_eq!(
            deserialize_attestation(&bytes),
            Some(att),
            "attestation must round-trip exactly"
        );
        // Wrong length → None (never a panic).
        assert_eq!(deserialize_attestation(&bytes[..81]), None);
        assert_eq!(deserialize_attestation(&[]), None);
        let mut too_long = bytes.clone();
        too_long.push(0);
        assert_eq!(deserialize_attestation(&too_long), None);
    }

    /// `attestation_key` is deterministic and domain-separated from every coded-chunk key
    /// (so the attestation object can never collide with chunk 0).
    #[test]
    fn attestation_key_is_deterministic_and_domain_separated() {
        let root = [9u8; 32];
        assert_eq!(attestation_key(root), attestation_key(root), "deterministic");
        let att = DaAttestation {
            program_id: [0u8; 32],
            da_root: root,
            data_len: 0,
            chunk_size: 65_536,
            n_data: 1,
            n_total: 2,
            params_version: 1,
        };
        for i in 0..att.n_total {
            assert_ne!(
                attestation_key(root),
                chunk_hash(&att, i),
                "attestation key must not collide with any coded-chunk key"
            );
        }
    }
}
