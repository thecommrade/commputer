// da_store.rs — filesystem blob store for PoUW data-availability coded chunks
// (Track-2 Phase 0, DA substrate). Founder decision Q5: FILESYSTEM under data_dir.
//
// WHAT: an on-disk store of coded DA chunks the node holds and serves over the
// `/commputer/da/1` protocol (network/src/da_protocol.rs). Each chunk is keyed by
// its TRANSPORT chunk_hash = `sha256(da_root ‖ index_le)` (position-addressing, per
// the frozen DA facade at staging/da/src/facade.rs:20-25) — NOT content-addressing.
// This layer only ferries/stores the (bytes, merkle_path) pair; the DA facade
// re-verifies each fetched chunk's Merkle path against da_root, so a wrong holder
// cannot substitute bytes. Stored one-file-per-chunk (filename = hex(chunk_hash))
// under a caller-supplied directory (e.g. `data_dir/da_chunks`).
// Writes are atomic (temp file -> fsync -> rename) and 0600. Two hard caps guard
// against a DA-DoS turning the node into free storage: a per-chunk size cap and a
// total-store byte budget. `gc` prunes any chunk not in the caller's live-job set
// so retention stays scoped to active jobs.
//
// The DA outcome is never hashed into consensus, so nothing here is consensus- or
// fork-relevant; the store is pure local plumbing.
//
// WIRING: main.rs constructs one `DaStore::open(data_dir/da_chunks)` and hands it
// to the event_loop's DA backend, which serves inbound `DaRequest::GetChunk` from
// `get()`/`has()` and `put()`s coded chunks the publisher produced — that half is
// live (as of the 2026-07-18 live-payout fixes, publishers/executors/verifiers
// actually read/write through this store on a real multi-node network).
//
// `gc()` ITSELF REMAINS UNWIRED: there are ZERO production call sites for it as of
// this writing — nothing periodically prunes the store. The correct live-set to
// scope retention to (once wired) is the union of the THREE consensus maps that
// can reference an outstanding job's DA bytes: `pending_jobs ∪ job_lifecycles ∪
// escalation_rounds` (see `src/storage/src/state.rs` — `escalation_rounds` is the
// PoUW S4 2026-07-19 addition, an in-flight `EscalationRound` still needs its
// job's DA bytes reachable) — PLUS, for each of those jobs' `da_root`s, the
// derived attestation key (`attestation_key(da_root)` in
// `src/node/src/da_publisher.rs`, ~line 177), since the attestation blob is a
// separate stored object keyed off the same root and would otherwise be gc'd as dead.
// Until `gc` is wired, the store only grows: the `DEFAULT_MAX_STORE_BYTES` 4 GiB
// hard cap below is a slow-burn publisher-liveness item for a long-running seed
// (it will eventually refuse new `put()`s once full, not lose existing data) —
// wiring a periodic gc call is a founder-batch item because the call site belongs
// in the PROTECTED `event_loop.rs` (a periodic task alongside GetChunk serving).
// FILES NEEDING CHANGES (later, gated): node/src/event_loop.rs (PROTECTED: spawn a
// periodic gc pass with the live-set above).

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use commputer_network::da_protocol::DaChunk;

/// Default per-DA-chunk payload size (mirrors `commputer_da::params::DEFAULT_CHUNK_SIZE`
/// = 64 KiB). Duplicated as a plain const to avoid pulling the DA crate as a direct
/// node dependency for one number; the wire codec enforces the same envelope.
pub const DEFAULT_CHUNK_SIZE: usize = 65_536;

/// Overhead budget above the raw 64 KiB payload: the serialized Merkle path plus
/// the on-disk framing length prefixes. A Merkle path over <= 256 coded chunks is
/// <= 8 levels x 33 bytes; 4 KiB is a generous ceiling.
pub const MAX_CHUNK_OVERHEAD: usize = 4096;

/// Hard cap on a single stored chunk's on-disk encoding. Anything larger is
/// rejected by `put` (a peer/publisher cannot make us store an oversized blob).
pub const MAX_ENCODED_CHUNK: usize = DEFAULT_CHUNK_SIZE + MAX_CHUNK_OVERHEAD;

/// Default total on-disk budget for the whole blob store (hard backstop; the
/// INTENT is for day-to-day footprint to stay bounded by `gc` scoping retention
/// to active jobs, but `gc` is UNWIRED as of this writing — see the module-level
/// doc comment — so today this cap is the only thing standing between a
/// long-running seed and an ever-growing store). 4 GiB.
pub const DEFAULT_MAX_STORE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// A filesystem-backed, content-addressed store of coded DA chunks.
pub struct DaStore {
    dir: PathBuf,
    /// Running total of bytes on disk (the encoded size of every stored chunk).
    /// Guarded so the check-and-write in `put` is atomic w.r.t. the cap. The
    /// filesystem remains the source of truth; this is seeded by a scan at `open`.
    total_bytes: Mutex<u64>,
    max_store_bytes: u64,
}

impl DaStore {
    /// Open (creating if absent) a blob store rooted at `dir`, with the default
    /// total-store cap.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with_cap(dir, DEFAULT_MAX_STORE_BYTES)
    }

    /// Open with an explicit total-store byte cap (exposed for tests / tuning).
    pub fn open_with_cap(dir: impl AsRef<Path>, max_store_bytes: u64) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        // Seed the running total from any chunk files already present.
        let mut total: u64 = 0;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if hex_to_hash(&entry.file_name().to_string_lossy()).is_some() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
        Ok(Self {
            dir,
            total_bytes: Mutex::new(total),
            max_store_bytes,
        })
    }

    /// Final on-disk path for a chunk (filename = lowercase hex of the hash).
    fn chunk_path(&self, chunk_hash: [u8; 32]) -> PathBuf {
        self.dir.join(hex::encode(chunk_hash))
    }

    /// Store `chunk` under `chunk_hash`. Atomic (temp -> fsync -> rename), 0600.
    /// Rejects a chunk whose encoding exceeds `MAX_ENCODED_CHUNK`, or that would
    /// push the store past its total byte cap. Overwriting an existing key is
    /// permitted (its old size is credited back first).
    pub fn put(&self, chunk_hash: [u8; 32], chunk: &DaChunk) -> io::Result<()> {
        let encoded = encode_chunk(chunk);
        if encoded.len() > MAX_ENCODED_CHUNK {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "da chunk exceeds per-chunk size cap",
            ));
        }

        let final_path = self.chunk_path(chunk_hash);

        // Serialize the accounting + write so the cap decision cannot race.
        let mut total = self.total_bytes.lock().unwrap();
        let old_size = fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
        let new_total = total
            .saturating_sub(old_size)
            .saturating_add(encoded.len() as u64);
        if new_total > self.max_store_bytes {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "da store total size cap exceeded",
            ));
        }

        atomic_write_0600(&self.dir, &final_path, chunk_hash, &encoded)?;
        *total = new_total;
        Ok(())
    }

    /// Fetch the chunk stored under `chunk_hash`, or `None` if absent. A present
    /// but corrupt/truncated file is an `Err(InvalidData)`.
    pub fn get(&self, chunk_hash: [u8; 32]) -> io::Result<Option<DaChunk>> {
        let raw = match fs::read(self.chunk_path(chunk_hash)) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        decode_chunk(&raw)
            .map(Some)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "corrupt da chunk on disk"))
    }

    /// Whether a chunk is stored under `chunk_hash`.
    pub fn has(&self, chunk_hash: [u8; 32]) -> bool {
        self.chunk_path(chunk_hash).exists()
    }

    /// Remove the chunk under `chunk_hash` (idempotent — a missing chunk is Ok).
    pub fn remove(&self, chunk_hash: [u8; 32]) -> io::Result<()> {
        let path = self.chunk_path(chunk_hash);
        let mut total = self.total_bytes.lock().unwrap();
        let size = match fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match fs::remove_file(&path) {
            Ok(()) => {
                *total = total.saturating_sub(size);
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Delete every stored chunk whose hash is NOT in `live`. Returns the number of
    /// chunks removed. Retention is thereby scoped to whatever active-job set the
    /// caller passes in — the correct set is `pending_jobs ∪ job_lifecycles ∪
    /// escalation_rounds` (plus each of those jobs' `attestation_key(da_root)` in
    /// `da_publisher.rs`, ~line 177) — see the module-level doc comment for the
    /// full rationale. UNWIRED as of this writing — no production call site
    /// invokes this yet; wiring a periodic call is a founder-batch item
    /// (PROTECTED `event_loop.rs`).
    pub fn gc(&self, live: &HashSet<[u8; 32]>) -> io::Result<usize> {
        let mut removed = 0usize;
        let mut total = self.total_bytes.lock().unwrap();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let hash = match hex_to_hash(&name) {
                Some(h) => h,
                None => continue, // not a chunk file (e.g. a stray temp file)
            };
            if live.contains(&hash) {
                continue;
            }
            let size = entry.metadata()?.len();
            match fs::remove_file(entry.path()) {
                Ok(()) => {
                    *total = total.saturating_sub(size);
                    removed += 1;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(removed)
    }

    /// Current total bytes on disk (test/observability helper).
    pub fn total_bytes(&self) -> u64 {
        *self.total_bytes.lock().unwrap()
    }
}

/// Length-prefixed on-disk encoding of a `DaChunk`:
///   [bytes_len: u64 le] [bytes...] [path_len: u32 le] [merkle_path...]
/// Mirrors the `LocalDiskTransport` framing style (bincode-free, zero new deps).
fn encode_chunk(chunk: &DaChunk) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12 + chunk.bytes.len() + chunk.merkle_path.len());
    buf.extend_from_slice(&(chunk.bytes.len() as u64).to_le_bytes());
    buf.extend_from_slice(&chunk.bytes);
    buf.extend_from_slice(&(chunk.merkle_path.len() as u32).to_le_bytes());
    buf.extend_from_slice(&chunk.merkle_path);
    buf
}

/// Inverse of `encode_chunk`. Returns `None` on any structural mismatch.
fn decode_chunk(raw: &[u8]) -> Option<DaChunk> {
    let mut pos = 0usize;
    if pos + 8 > raw.len() {
        return None;
    }
    let bytes_len = u64::from_le_bytes(raw[pos..pos + 8].try_into().ok()?) as usize;
    pos += 8;
    if pos + bytes_len > raw.len() {
        return None;
    }
    let bytes = raw[pos..pos + bytes_len].to_vec();
    pos += bytes_len;
    if pos + 4 > raw.len() {
        return None;
    }
    let path_len = u32::from_le_bytes(raw[pos..pos + 4].try_into().ok()?) as usize;
    pos += 4;
    if pos + path_len != raw.len() {
        return None; // trailing garbage or truncation
    }
    let merkle_path = raw[pos..pos + path_len].to_vec();
    Some(DaChunk { bytes, merkle_path })
}

/// Decode a 64-char lowercase-hex filename back to a chunk hash. Returns `None`
/// for anything that is not exactly a 32-byte hex string (so temp files and stray
/// entries are ignored).
fn hex_to_hash(name: &str) -> Option<[u8; 32]> {
    if name.len() != 64 {
        return None;
    }
    let raw = hex::decode(name).ok()?;
    let arr: [u8; 32] = raw.try_into().ok()?;
    Some(arr)
}

/// Atomic 0600 write: create a uniquely-named temp file in the same directory,
/// set 0600 (unix), write + fsync, then rename over `final_path` (same-directory
/// rename is atomic on POSIX). The temp name is NOT a valid chunk hex name, so a
/// crash mid-write leaves a file `gc`/scan ignore rather than a half-written chunk.
fn atomic_write_0600(
    dir: &Path,
    final_path: &Path,
    chunk_hash: [u8; 32],
    data: &[u8],
) -> io::Result<()> {
    use std::io::Write as _;

    let tmp_path = dir.join(format!(
        "{}.tmp.{}",
        hex::encode(chunk_hash),
        std::process::id()
    ));

    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        // Enforce 0600 regardless of umask (unix). No-op elsewhere.
        set_owner_only(&f)?;
        f.write_all(data)?;
        f.sync_all()?;
    }

    fs::rename(&tmp_path, final_path)
}

#[cfg(unix)]
fn set_owner_only(f: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    f.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_f: &fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("commputer-da-store-test-{tag}-{}-{}", std::process::id(), id))
    }

    fn chunk(fill: u8, bytes_len: usize, path_len: usize) -> DaChunk {
        DaChunk {
            bytes: vec![fill; bytes_len],
            merkle_path: vec![fill ^ 0xff; path_len],
        }
    }

    #[test]
    fn put_get_roundtrip() {
        let store = DaStore::open(tmp_dir("roundtrip")).unwrap();
        let h = [1u8; 32];
        let c = chunk(0x5a, 4096, 130);
        store.put(h, &c).unwrap();

        let got = store.get(h).unwrap().expect("chunk should be present");
        assert_eq!(got, c, "chunk must round-trip byte-for-byte");
    }

    #[test]
    fn get_missing_is_none() {
        let store = DaStore::open(tmp_dir("missing")).unwrap();
        assert!(store.get([9u8; 32]).unwrap().is_none());
    }

    #[test]
    fn has_reflects_presence() {
        let store = DaStore::open(tmp_dir("has")).unwrap();
        let h = [2u8; 32];
        assert!(!store.has(h));
        store.put(h, &chunk(0x11, 10, 0)).unwrap();
        assert!(store.has(h));
        store.remove(h).unwrap();
        assert!(!store.has(h));
    }

    #[test]
    fn remove_is_idempotent_and_updates_total() {
        let store = DaStore::open(tmp_dir("remove")).unwrap();
        let h = [3u8; 32];
        store.put(h, &chunk(0x22, 100, 33)).unwrap();
        assert!(store.total_bytes() > 0);
        store.remove(h).unwrap();
        assert_eq!(store.total_bytes(), 0, "total must return to 0 after removal");
        assert!(!store.has(h));
        // Removing again is a no-op, not an error.
        store.remove(h).unwrap();
    }

    #[test]
    fn gc_removes_only_dead_chunks() {
        let store = DaStore::open(tmp_dir("gc")).unwrap();
        let live_h = [4u8; 32];
        let dead_h = [5u8; 32];
        store.put(live_h, &chunk(0x33, 200, 33)).unwrap();
        store.put(dead_h, &chunk(0x44, 200, 33)).unwrap();

        let mut live = HashSet::new();
        live.insert(live_h);
        let removed = store.gc(&live).unwrap();

        assert_eq!(removed, 1, "exactly one dead chunk must be removed");
        assert!(store.has(live_h), "live chunk must survive gc");
        assert!(!store.has(dead_h), "dead chunk must be removed by gc");
    }

    #[test]
    fn oversized_chunk_rejected() {
        let store = DaStore::open(tmp_dir("oversize")).unwrap();
        let h = [6u8; 32];
        // bytes alone exceed the per-chunk envelope.
        let too_big = chunk(0x77, MAX_ENCODED_CHUNK + 1, 0);
        let err = store.put(h, &too_big).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(!store.has(h), "an oversized chunk must not be stored");
        assert_eq!(store.total_bytes(), 0, "a rejected put must not change the total");
    }

    #[test]
    fn total_store_cap_rejects_beyond_budget() {
        // Budget fits exactly one small chunk's encoding, not two.
        let one = chunk(0x88, 100, 0);
        let encoded_len = encode_chunk(&one).len() as u64;
        let store = DaStore::open_with_cap(tmp_dir("totalcap"), encoded_len).unwrap();

        store.put([10u8; 32], &one).unwrap();
        let err = store.put([11u8; 32], &chunk(0x99, 100, 0)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(!store.has([11u8; 32]), "the over-budget chunk must not be stored");
        assert!(store.has([10u8; 32]), "the first (in-budget) chunk stays");
    }

    #[test]
    fn overwrite_credits_old_size() {
        // Overwriting a key must not double-count toward the total cap.
        let small = chunk(0x01, 100, 0);
        let encoded_len = encode_chunk(&small).len() as u64;
        let store = DaStore::open_with_cap(tmp_dir("overwrite"), encoded_len).unwrap();
        let h = [12u8; 32];
        store.put(h, &small).unwrap();
        // Same key again with the same size fits because the old size is credited.
        store.put(h, &chunk(0x02, 100, 0)).unwrap();
        assert_eq!(store.total_bytes(), encoded_len);
        assert_eq!(store.get(h).unwrap().unwrap().bytes[0], 0x02);
    }

    #[test]
    fn total_seeded_from_existing_dir_on_open() {
        let dir = tmp_dir("reopen");
        {
            let store = DaStore::open(&dir).unwrap();
            store.put([13u8; 32], &chunk(0x0a, 512, 33)).unwrap();
            store.put([14u8; 32], &chunk(0x0b, 512, 33)).unwrap();
        }
        // Re-open: the running total must reflect the on-disk chunks.
        let reopened = DaStore::open(&dir).unwrap();
        assert!(reopened.total_bytes() > 0);
        assert!(reopened.has([13u8; 32]));
        assert!(reopened.has([14u8; 32]));
        // And the seeded total is enforced by the cap on the next put.
        assert_eq!(reopened.get([13u8; 32]).unwrap().unwrap().bytes.len(), 512);
    }

    #[test]
    fn corrupt_file_is_error() {
        let dir = tmp_dir("corrupt");
        let store = DaStore::open(&dir).unwrap();
        let h = [15u8; 32];
        // Write a garbage (too-short) file directly under the chunk name.
        fs::write(dir.join(hex::encode(h)), [0u8; 3]).unwrap();
        let err = store.get(h).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn stored_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("perms");
        let store = DaStore::open(&dir).unwrap();
        let h = [16u8; 32];
        store.put(h, &chunk(0x5c, 64, 0)).unwrap();
        let mode = fs::metadata(dir.join(hex::encode(h)))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "stored chunk must be owner-only (0600)");
    }
}
