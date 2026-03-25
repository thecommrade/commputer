use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// A stored data entry associated with a compute job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataEntry {
    pub data_hash: [u8; 32],
    pub size_bytes: u64,
    pub stored_at_height: u64,
    pub uploader_hex: String,
    pub pinned: bool,
    pub pin_expiry: Option<u64>,
}

/// In-memory data store for managing uploaded data entries.
pub struct DataStore {
    pub entries: HashMap<[u8; 32], DataEntry>,
    pub total_size_bytes: u64,
}

impl DataStore {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            total_size_bytes: 0,
        }
    }

    /// Store a new data entry. Returns true if newly inserted.
    pub fn store_data(
        &mut self,
        hash: [u8; 32],
        size: u64,
        uploader: &str,
        height: u64,
    ) -> bool {
        if self.entries.contains_key(&hash) {
            return false;
        }
        let entry = DataEntry {
            data_hash: hash,
            size_bytes: size,
            stored_at_height: height,
            uploader_hex: uploader.to_string(),
            pinned: false,
            pin_expiry: None,
        };
        self.entries.insert(hash, entry);
        self.total_size_bytes += size;
        true
    }

    /// Get an entry by hash.
    pub fn get_entry(&self, hash: &[u8; 32]) -> Option<&DataEntry> {
        self.entries.get(hash)
    }

    /// Pin data so it won't be garbage collected.
    pub fn pin_data(&mut self, hash: &[u8; 32], duration_blocks: u64) -> bool {
        match self.entries.get_mut(hash) {
            Some(entry) => {
                entry.pinned = true;
                let current_height = entry.stored_at_height;
                entry.pin_expiry = Some(current_height + duration_blocks);
                true
            }
            None => false,
        }
    }

    /// Unpin data, allowing it to be garbage collected.
    pub fn unpin_data(&mut self, hash: &[u8; 32]) -> bool {
        match self.entries.get_mut(hash) {
            Some(entry) => {
                entry.pinned = false;
                entry.pin_expiry = None;
                true
            }
            None => false,
        }
    }

    /// Calculate total size of all pinned entries.
    pub fn total_pinned_size(&self) -> u64 {
        self.entries
            .values()
            .filter(|e| e.pinned)
            .map(|e| e.size_bytes)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hash(val: u8) -> [u8; 32] {
        let mut h = [0u8; 32];
        h[0] = val;
        h
    }

    #[test]
    fn test_store_and_retrieve() {
        let mut store = DataStore::new();
        let hash = test_hash(1);
        assert!(store.store_data(hash, 1024, "alice", 100));
        assert!(!store.store_data(hash, 1024, "alice", 100)); // duplicate

        let entry = store.get_entry(&hash).unwrap();
        assert_eq!(entry.size_bytes, 1024);
        assert_eq!(entry.uploader_hex, "alice");
        assert_eq!(store.total_size_bytes, 1024);
    }

    #[test]
    fn test_pin_unpin() {
        let mut store = DataStore::new();
        let hash = test_hash(2);
        store.store_data(hash, 2048, "bob", 50);

        assert!(store.pin_data(&hash, 1000));
        let entry = store.get_entry(&hash).unwrap();
        assert!(entry.pinned);
        assert_eq!(entry.pin_expiry, Some(1050));

        assert!(store.unpin_data(&hash));
        let entry = store.get_entry(&hash).unwrap();
        assert!(!entry.pinned);
        assert!(entry.pin_expiry.is_none());
    }

    #[test]
    fn test_pin_nonexistent() {
        let mut store = DataStore::new();
        let hash = test_hash(99);
        assert!(!store.pin_data(&hash, 100));
        assert!(!store.unpin_data(&hash));
    }

    #[test]
    fn test_total_pinned_size() {
        let mut store = DataStore::new();
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let h3 = test_hash(3);

        store.store_data(h1, 100, "a", 1);
        store.store_data(h2, 200, "b", 1);
        store.store_data(h3, 300, "c", 1);

        store.pin_data(&h1, 100);
        store.pin_data(&h3, 100);

        assert_eq!(store.total_pinned_size(), 400);
    }
}
