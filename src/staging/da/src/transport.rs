//! Transport seam (spec §6.5). SYNCHRONOUS for v1 (deliberate simplification of the
//! spec's async-trait §6.5): all v1 impls are synchronous, the sim is single-threaded
//! and deterministic, and zero async dep is pulled. A future libp2p adapter implements
//! the same method shapes, wrapping its own async behind a blocking facade. The four
//! methods map 1:1 to Kademlia start_providing/get_providers + Bitswap want-block/want-have.
//!
//! Where it wires in: `src/staging/da/src/facade.rs` (DataAvailability) uses DaTransport;
//! a real libp2p adapter will implement DaTransport and pass &dyn DaTransport.
//! Existing files that need changes: none for Layer 3 (facade.rs is created in Layer 4).
use crate::params::ProviderId;
use std::cell::RefCell;
use std::collections::HashMap;

pub type MerklePath = Vec<Option<[u8; 32]>>;

pub trait DaTransport {
    fn advertise(&self, chunk_hash: [u8; 32], me: ProviderId);
    fn find_providers(&self, chunk_hash: [u8; 32]) -> Vec<ProviderId>;
    fn fetch_chunk(&self, chunk_hash: [u8; 32], from: ProviderId) -> Option<(Vec<u8>, MerklePath)>;
    fn has_chunk(&self, chunk_hash: [u8; 32]) -> bool;
}

/// Logical clock — sim advances ticks explicitly; production wires a monotonic source.
/// NEVER feeds a hashed consensus value (spec §5).
pub trait Clock {
    fn now_tick(&self) -> u64;
}

#[derive(Default)]
pub struct ManualClock {
    t: RefCell<u64>,
}
impl ManualClock {
    pub fn new() -> Self { Self::default() }
    pub fn advance(&self, by: u64) { *self.t.borrow_mut() += by; }
}
impl Clock for ManualClock {
    fn now_tick(&self) -> u64 { *self.t.borrow() }
}

#[derive(Default)]
pub struct InMemoryTransport {
    // chunk_hash -> (providers, bytes, path)
    store: RefCell<HashMap<[u8; 32], (Vec<ProviderId>, Vec<u8>, MerklePath)>>,
}
impl InMemoryTransport {
    pub fn new() -> Self { Self::default() }
    /// Test/sim helper: place a chunk held by `prov`.
    pub fn put(&self, chunk_hash: [u8; 32], prov: ProviderId, bytes: Vec<u8>, path: MerklePath) {
        let mut s = self.store.borrow_mut();
        let e = s.entry(chunk_hash).or_insert_with(|| (vec![], bytes.clone(), path.clone()));
        if !e.0.contains(&prov) { e.0.push(prov); }
        e.1 = bytes; e.2 = path;
    }
    /// Simulate withholding: remove a chunk entirely.
    pub fn withhold(&self, chunk_hash: [u8; 32]) { self.store.borrow_mut().remove(&chunk_hash); }
}
impl DaTransport for InMemoryTransport {
    fn advertise(&self, chunk_hash: [u8; 32], me: ProviderId) {
        let mut s = self.store.borrow_mut();
        let e = s.entry(chunk_hash).or_insert_with(|| (vec![], vec![], vec![]));
        if !e.0.contains(&me) { e.0.push(me); }
    }
    fn find_providers(&self, chunk_hash: [u8; 32]) -> Vec<ProviderId> {
        self.store.borrow().get(&chunk_hash).map(|e| e.0.clone()).unwrap_or_default()
    }
    fn fetch_chunk(&self, chunk_hash: [u8; 32], from: ProviderId) -> Option<(Vec<u8>, MerklePath)> {
        let s = self.store.borrow();
        let e = s.get(&chunk_hash)?;
        if e.0.contains(&from) && !e.1.is_empty() { Some((e.1.clone(), e.2.clone())) } else { None }
    }
    fn has_chunk(&self, chunk_hash: [u8; 32]) -> bool {
        self.store.borrow().get(&chunk_hash).map(|e| !e.1.is_empty()).unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// LocalDiskTransport — thin persistent variant for storage/sim tests.
//
// Design choice: FULL round-trip. Both the chunk bytes AND the MerklePath are
// stored on disk.
//
// Encoding (bincode-free, zero new deps):
//   File layout for each chunk (keyed by hex(chunk_hash)):
//     [bytes_len: u64 le] [bytes...]
//     [path_len: u32 le]  (number of path elements)
//     for each element:
//       [present: u8]  (0x01 = Some hash present, 0x00 = None)
//       if present: [hash: 32 bytes]
//
// The temp directory is created once at construction (unique per instance via a
// counter embedded in the dir name); it is NOT cleaned up automatically so that
// tests can inspect artifacts. A Drop impl could be added later.
// ---------------------------------------------------------------------------
use std::sync::atomic::{AtomicU64, Ordering};

static DISK_TRANSPORT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct LocalDiskTransport {
    dir: std::path::PathBuf,
    providers: RefCell<HashMap<[u8; 32], Vec<ProviderId>>>,
}

impl LocalDiskTransport {
    pub fn new() -> Self {
        let id = DISK_TRANSPORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("commputer-da-disk-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(&dir).expect("LocalDiskTransport: could not create temp dir");
        Self { dir, providers: RefCell::new(HashMap::new()) }
    }

    fn chunk_path(&self, chunk_hash: [u8; 32]) -> std::path::PathBuf {
        let hex: String = chunk_hash.iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(hex)
    }

    /// Test helper: store a chunk held by `prov`.
    pub fn put(&self, chunk_hash: [u8; 32], prov: ProviderId, bytes: Vec<u8>, path: MerklePath) {
        // Encode
        let mut buf = Vec::new();
        // bytes section
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(&bytes);
        // path section
        buf.extend_from_slice(&(path.len() as u32).to_le_bytes());
        for elem in &path {
            match elem {
                Some(h) => { buf.push(0x01); buf.extend_from_slice(h); }
                None    => { buf.push(0x00); }
            }
        }
        std::fs::write(self.chunk_path(chunk_hash), &buf)
            .expect("LocalDiskTransport: write failed");
        // register provider
        let mut p = self.providers.borrow_mut();
        let e = p.entry(chunk_hash).or_default();
        if !e.contains(&prov) { e.push(prov); }
    }

    fn load(&self, chunk_hash: [u8; 32]) -> Option<(Vec<u8>, MerklePath)> {
        let raw = std::fs::read(self.chunk_path(chunk_hash)).ok()?;
        let mut pos = 0;
        // bytes
        if pos + 8 > raw.len() { return None; }
        let bytes_len = u64::from_le_bytes(raw[pos..pos+8].try_into().ok()?) as usize;
        pos += 8;
        if pos + bytes_len > raw.len() { return None; }
        let bytes = raw[pos..pos+bytes_len].to_vec();
        pos += bytes_len;
        // path
        if pos + 4 > raw.len() { return None; }
        let path_len = u32::from_le_bytes(raw[pos..pos+4].try_into().ok()?) as usize;
        pos += 4;
        let mut merkle_path = Vec::with_capacity(path_len);
        for _ in 0..path_len {
            if pos >= raw.len() { return None; }
            let present = raw[pos]; pos += 1;
            if present == 0x01 {
                if pos + 32 > raw.len() { return None; }
                let mut h = [0u8; 32];
                h.copy_from_slice(&raw[pos..pos+32]);
                pos += 32;
                merkle_path.push(Some(h));
            } else {
                merkle_path.push(None);
            }
        }
        Some((bytes, merkle_path))
    }
}

impl DaTransport for LocalDiskTransport {
    fn advertise(&self, chunk_hash: [u8; 32], me: ProviderId) {
        let mut p = self.providers.borrow_mut();
        let e = p.entry(chunk_hash).or_default();
        if !e.contains(&me) { e.push(me); }
    }
    fn find_providers(&self, chunk_hash: [u8; 32]) -> Vec<ProviderId> {
        self.providers.borrow().get(&chunk_hash).cloned().unwrap_or_default()
    }
    fn fetch_chunk(&self, chunk_hash: [u8; 32], from: ProviderId) -> Option<(Vec<u8>, MerklePath)> {
        let has_provider = self.providers.borrow().get(&chunk_hash)
            .map(|v| v.contains(&from))
            .unwrap_or(false);
        if !has_provider { return None; }
        self.load(chunk_hash)
    }
    fn has_chunk(&self, chunk_hash: [u8; 32]) -> bool {
        self.chunk_path(chunk_hash).exists()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ProviderId;

    #[test]
    fn advertise_find_fetch_roundtrip() {
        let t = InMemoryTransport::new();
        let prov = ProviderId([7; 32]);
        let chunk_hash = [9u8; 32];
        t.put(chunk_hash, prov, vec![1, 2, 3], vec![]); // (chunk bytes, merkle path placeholder)
        assert!(t.has_chunk(chunk_hash));
        let provs = t.find_providers(chunk_hash);
        assert_eq!(provs, vec![prov]);
        let (bytes, _path) = t.fetch_chunk(chunk_hash, prov).unwrap();
        assert_eq!(bytes, vec![1, 2, 3]);
        assert!(t.fetch_chunk([0; 32], prov).is_none()); // miss
    }

    #[test]
    fn clock_advances_manually() {
        let c = ManualClock::new();
        assert_eq!(c.now_tick(), 0);
        c.advance(5);
        assert_eq!(c.now_tick(), 5);
    }

    #[test]
    fn local_disk_roundtrip() {
        let t = LocalDiskTransport::new();
        let prov = ProviderId([3; 32]);
        let chunk_hash = [11u8; 32];
        // Construct a non-trivial MerklePath: Some, None, Some
        let path: MerklePath = vec![
            Some([0xabu8; 32]),
            None,
            Some([0xcdu8; 32]),
        ];
        let bytes = vec![10u8, 20, 30, 40, 50];
        t.put(chunk_hash, prov, bytes.clone(), path.clone());

        assert!(t.has_chunk(chunk_hash));
        assert_eq!(t.find_providers(chunk_hash), vec![prov]);

        let (fetched_bytes, fetched_path) = t.fetch_chunk(chunk_hash, prov).unwrap();
        assert_eq!(fetched_bytes, bytes, "bytes round-trip");
        assert_eq!(fetched_path, path, "merkle path round-trip");

        // miss for unknown chunk
        assert!(t.fetch_chunk([0u8; 32], prov).is_none());
        // miss for wrong provider
        assert!(t.fetch_chunk(chunk_hash, ProviderId([99; 32])).is_none());
    }
}
