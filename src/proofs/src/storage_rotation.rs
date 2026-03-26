//! Item 144: Storage proof rotation.
//!
//! Periodically reassign chunks to different validators to prevent hoarding.
//! Each epoch, a deterministic shuffle decides which validator holds which chunks.

use commputer_core::identity::Address;
use sha2::{Digest, Sha256};


/// Manages storage chunk rotation across validators.
pub struct StorageRotation {
    /// Number of total chunks in the system.
    pub total_chunks: usize,
    /// How many epochs between rotations.
    pub rotation_interval: u64,
    /// Current epoch.
    pub current_epoch: u64,
}

/// Assignment of chunks to a validator for a given epoch.
#[derive(Debug, Clone)]
pub struct ChunkAssignment {
    pub validator: Address,
    pub chunk_indices: Vec<usize>,
    pub epoch: u64,
}

impl StorageRotation {
    /// Create a new storage rotation manager.
    pub fn new(total_chunks: usize, rotation_interval: u64) -> Self {
        Self {
            total_chunks,
            rotation_interval,
            current_epoch: 0,
        }
    }

    /// Advance to the given epoch.
    pub fn set_epoch(&mut self, epoch: u64) {
        self.current_epoch = epoch;
    }

    /// Check if a rotation should happen at the current epoch.
    pub fn should_rotate(&self) -> bool {
        self.current_epoch > 0 && self.current_epoch % self.rotation_interval == 0
    }

    /// Compute chunk assignments for all validators at the given epoch.
    /// Uses a deterministic shuffle seeded by the epoch.
    pub fn assign_chunks(
        &self,
        validators: &[Address],
        epoch: u64,
    ) -> Vec<ChunkAssignment> {
        if validators.is_empty() || self.total_chunks == 0 {
            return vec![];
        }

        // Deterministic permutation of chunk indices using epoch-seeded Fisher-Yates.
        let mut indices: Vec<usize> = (0..self.total_chunks).collect();
        let epoch_seed = Self::epoch_seed(epoch);

        // Fisher-Yates shuffle with deterministic randomness.
        for i in (1..indices.len()).rev() {
            let j = Self::deterministic_index(&epoch_seed, i as u64, (i + 1) as u64);
            indices.swap(i, j);
        }

        // Distribute chunks round-robin among validators.
        let chunks_per_validator = self.total_chunks / validators.len();
        let remainder = self.total_chunks % validators.len();

        let mut assignments = Vec::with_capacity(validators.len());
        let mut offset = 0;

        for (v_idx, validator) in validators.iter().enumerate() {
            let count = chunks_per_validator + if v_idx < remainder { 1 } else { 0 };
            let chunk_indices = indices[offset..offset + count].to_vec();
            offset += count;

            assignments.push(ChunkAssignment {
                validator: *validator,
                chunk_indices,
                epoch,
            });
        }

        assignments
    }

    /// Get the assignment for a specific validator at the given epoch.
    pub fn get_validator_chunks(
        &self,
        validator: &Address,
        validators: &[Address],
        epoch: u64,
    ) -> Vec<usize> {
        let assignments = self.assign_chunks(validators, epoch);
        assignments
            .into_iter()
            .find(|a| a.validator == *validator)
            .map(|a| a.chunk_indices)
            .unwrap_or_default()
    }

    /// Check that assignments changed between two epochs (rotation occurred).
    pub fn rotation_occurred(
        &self,
        validators: &[Address],
        epoch_a: u64,
        epoch_b: u64,
    ) -> bool {
        if validators.is_empty() || self.total_chunks == 0 {
            return false;
        }
        let assign_a = self.assign_chunks(validators, epoch_a);
        let assign_b = self.assign_chunks(validators, epoch_b);

        // Compare first validator's assignments.
        if let (Some(a), Some(b)) = (assign_a.first(), assign_b.first()) {
            a.chunk_indices != b.chunk_indices
        } else {
            false
        }
    }

    /// Derive a deterministic epoch seed.
    fn epoch_seed(epoch: u64) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"storage_rotation_epoch:");
        hasher.update(epoch.to_le_bytes());
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Deterministic index in [0, modulus) from a seed + counter.
    fn deterministic_index(seed: &[u8; 32], counter: u64, modulus: u64) -> usize {
        let mut hasher = Sha256::new();
        hasher.update(seed);
        hasher.update(counter.to_le_bytes());
        let h = hasher.finalize();
        let raw = u64::from_le_bytes(h[..8].try_into().unwrap());
        (raw % modulus) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    #[test]
    fn item_144_assign_chunks_distributes_evenly() {
        let rotation = StorageRotation::new(100, 10);
        let validators = vec![test_addr(1), test_addr(2), test_addr(3), test_addr(4)];
        let assignments = rotation.assign_chunks(&validators, 0);

        assert_eq!(assignments.len(), 4);
        let total: usize = assignments.iter().map(|a| a.chunk_indices.len()).sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn item_144_rotation_changes_assignments() {
        let rotation = StorageRotation::new(100, 10);
        let validators = vec![test_addr(1), test_addr(2)];
        assert!(rotation.rotation_occurred(&validators, 0, 10));
    }

    #[test]
    fn item_144_should_rotate() {
        let mut rotation = StorageRotation::new(100, 10);
        rotation.set_epoch(10);
        assert!(rotation.should_rotate());
        rotation.set_epoch(5);
        assert!(!rotation.should_rotate());
        rotation.set_epoch(0);
        assert!(!rotation.should_rotate());
    }

    #[test]
    fn item_144_deterministic_assignments() {
        let rotation = StorageRotation::new(50, 5);
        let validators = vec![test_addr(1), test_addr(2)];
        let a1 = rotation.assign_chunks(&validators, 7);
        let a2 = rotation.assign_chunks(&validators, 7);
        assert_eq!(a1[0].chunk_indices, a2[0].chunk_indices);
    }

    #[test]
    fn item_144_get_validator_chunks() {
        let rotation = StorageRotation::new(20, 5);
        let validators = vec![test_addr(1), test_addr(2)];
        let chunks = rotation.get_validator_chunks(&test_addr(1), &validators, 0);
        assert_eq!(chunks.len(), 10);
    }

    #[test]
    fn item_144_no_duplicate_chunks() {
        let rotation = StorageRotation::new(50, 5);
        let validators = vec![test_addr(1), test_addr(2), test_addr(3)];
        let assignments = rotation.assign_chunks(&validators, 0);

        let mut all_chunks: Vec<usize> = assignments
            .iter()
            .flat_map(|a| a.chunk_indices.clone())
            .collect();
        all_chunks.sort();
        all_chunks.dedup();
        assert_eq!(all_chunks.len(), 50);
    }
}
