//! Content-addressed program bytes: hash -> raw .wasm (spec §7). NO fetching,
//! NO eviction — data availability is deferred cycle #2 (spec §10.2).
//! New file; wired via wasm/mod.rs. No existing-file changes.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct ProgramStore {
    programs: HashMap<[u8; 32], Arc<[u8]>>,
}

impl ProgramStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store raw .wasm bytes under their sha256. The RAW bytes are the canonical
    /// program identity — never a compiled/serialized artifact (spec §7).
    pub fn insert(&mut self, bytes: impl Into<Arc<[u8]>>) -> [u8; 32] {
        let bytes: Arc<[u8]> = bytes.into();
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        self.programs.insert(hash, bytes);
        hash
    }

    pub fn get(&self, hash: &[u8; 32]) -> Option<Arc<[u8]>> {
        self.programs.get(hash).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn insert_addresses_by_content_and_get_roundtrips() {
        let mut s = ProgramStore::new();
        let bytes = b"fake-wasm".to_vec();
        let expected: [u8; 32] = Sha256::digest(&bytes).into();
        let hash = s.insert(bytes.clone());
        assert_eq!(hash, expected, "address must be sha256 of the RAW bytes (spec §7)");
        assert_eq!(s.get(&hash).as_deref(), Some(&bytes[..]));
        assert!(s.get(&[0u8; 32]).is_none());
    }
}
