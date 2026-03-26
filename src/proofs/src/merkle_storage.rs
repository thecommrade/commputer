//! Item 143: Storage proof with merkle verification.
//!
//! Stored chunks form a merkle tree. Challenges provide a random leaf index
//! and the prover must return the chunk data plus the merkle proof path.
//! The verifier checks the proof against the known merkle root.

use sha2::{Digest, Sha256};
use serde::{Serialize, Deserialize};

/// Size of each chunk in the merkle tree.
const CHUNK_SIZE: usize = 256;

/// A merkle tree over storage chunks.
#[derive(Debug, Clone)]
pub struct MerkleStorageTree {
    /// Leaf hashes (one per chunk).
    leaves: Vec<[u8; 32]>,
    /// All tree nodes stored level by level (root at index 0 of level 0).
    /// For simplicity, stored as a flat vector where the root is at index 0.
    nodes: Vec<[u8; 32]>,
    /// The original chunks of data.
    chunks: Vec<Vec<u8>>,
}

/// A merkle proof for a single chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    /// Index of the leaf being proved.
    pub leaf_index: usize,
    /// The chunk data at this leaf.
    pub chunk_data: Vec<u8>,
    /// Sibling hashes along the path from leaf to root.
    pub siblings: Vec<[u8; 32]>,
    /// Whether each sibling is on the left (true) or right (false).
    pub sibling_is_left: Vec<bool>,
}

/// Prover that uses merkle trees for storage verification.
pub struct MerkleStorageProver;

impl MerkleStorageTree {
    /// Build a merkle tree from raw data, splitting into CHUNK_SIZE chunks.
    pub fn build(data: &[u8]) -> Self {
        let chunks: Vec<Vec<u8>> = data
            .chunks(CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();

        // Ensure power-of-two leaf count by padding with empty chunks.
        let leaf_count = chunks.len().next_power_of_two();
        let mut leaves = Vec::with_capacity(leaf_count);
        for chunk in &chunks {
            leaves.push(Self::hash_chunk(chunk));
        }
        // Pad to power of two.
        while leaves.len() < leaf_count {
            leaves.push([0u8; 32]);
        }

        // Build the tree bottom-up. Total nodes = 2*leaf_count - 1.
        let total_nodes = 2 * leaf_count - 1;
        let mut nodes = vec![[0u8; 32]; total_nodes];

        // Place leaves at the end of the nodes array.
        let leaf_start = leaf_count - 1;
        for (i, leaf) in leaves.iter().enumerate() {
            nodes[leaf_start + i] = *leaf;
        }

        // Build internal nodes bottom-up.
        for i in (0..leaf_start).rev() {
            let left = nodes[2 * i + 1];
            let right = nodes[2 * i + 2];
            nodes[i] = Self::hash_pair(&left, &right);
        }

        Self {
            leaves,
            nodes,
            chunks,
        }
    }

    /// Get the merkle root hash.
    pub fn root(&self) -> [u8; 32] {
        if self.nodes.is_empty() {
            [0u8; 32]
        } else {
            self.nodes[0]
        }
    }

    /// Generate a merkle proof for the chunk at `leaf_index`.
    pub fn prove(&self, leaf_index: usize) -> Option<MerkleProof> {
        if leaf_index >= self.leaves.len() {
            return None;
        }

        let chunk_data = if leaf_index < self.chunks.len() {
            self.chunks[leaf_index].clone()
        } else {
            vec![]
        };

        let leaf_count = self.leaves.len();
        let mut siblings = Vec::new();
        let mut sibling_is_left = Vec::new();

        let mut idx = leaf_count - 1 + leaf_index; // Position in nodes array.
        while idx > 0 {
            let parent = (idx - 1) / 2;
            let sibling_idx = if idx % 2 == 1 {
                idx + 1 // Current is left child, sibling is right
            } else {
                idx - 1 // Current is right child, sibling is left
            };

            if sibling_idx < self.nodes.len() {
                siblings.push(self.nodes[sibling_idx]);
                sibling_is_left.push(idx % 2 == 0); // Sibling is left if current is right child
            }

            idx = parent;
        }

        Some(MerkleProof {
            leaf_index,
            chunk_data,
            siblings,
            sibling_is_left,
        })
    }

    /// Verify a merkle proof against a known root.
    pub fn verify_proof(root: &[u8; 32], proof: &MerkleProof) -> bool {
        let mut current = Self::hash_chunk(&proof.chunk_data);

        for (i, sibling) in proof.siblings.iter().enumerate() {
            if proof.sibling_is_left[i] {
                current = Self::hash_pair(sibling, &current);
            } else {
                current = Self::hash_pair(&current, sibling);
            }
        }

        current == *root
    }

    fn hash_chunk(data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"chunk:");
        hasher.update(data);
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    fn hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"node:");
        hasher.update(left);
        hasher.update(right);
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }
}

impl MerkleStorageProver {
    /// Build a merkle tree from the validator's stored data.
    pub fn build_tree(data: &[u8]) -> MerkleStorageTree {
        MerkleStorageTree::build(data)
    }

    /// Generate a proof for a random chunk index derived from the challenge seed.
    pub fn prove_chunk(tree: &MerkleStorageTree, seed: &[u8]) -> Option<MerkleProof> {
        if tree.leaves.is_empty() {
            return None;
        }
        let index = Self::derive_chunk_index(seed, tree.leaves.len());
        tree.prove(index)
    }

    /// Verify a storage merkle proof against a known root.
    pub fn verify(root: &[u8; 32], proof: &MerkleProof) -> bool {
        MerkleStorageTree::verify_proof(root, proof)
    }

    /// Derive a deterministic chunk index from a seed.
    fn derive_chunk_index(seed: &[u8], num_chunks: usize) -> usize {
        let mut hasher = Sha256::new();
        hasher.update(b"merkle_chunk_index:");
        hasher.update(seed);
        let h = hasher.finalize();
        let raw = u64::from_le_bytes(h[..8].try_into().unwrap());
        (raw as usize) % num_chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_143_build_and_prove() {
        let data = vec![42u8; 4096]; // 16 chunks of 256 bytes
        let tree = MerkleStorageProver::build_tree(&data);
        let root = tree.root();
        assert_ne!(root, [0u8; 32]);

        let proof = tree.prove(0).unwrap();
        assert!(MerkleStorageTree::verify_proof(&root, &proof));
    }

    #[test]
    fn item_143_prove_all_leaves() {
        let data = vec![99u8; 2048]; // 8 chunks
        let tree = MerkleStorageProver::build_tree(&data);
        let root = tree.root();

        for i in 0..8 {
            let proof = tree.prove(i).unwrap();
            assert!(
                MerkleStorageTree::verify_proof(&root, &proof),
                "proof for leaf {} should verify",
                i
            );
        }
    }

    #[test]
    fn item_143_tampered_proof_fails() {
        let data = vec![42u8; 4096];
        let tree = MerkleStorageProver::build_tree(&data);
        let root = tree.root();

        let mut proof = tree.prove(0).unwrap();
        proof.chunk_data[0] ^= 0xFF; // Tamper with chunk data.
        assert!(!MerkleStorageTree::verify_proof(&root, &proof));
    }

    #[test]
    fn item_143_wrong_root_fails() {
        let data = vec![42u8; 4096];
        let tree = MerkleStorageProver::build_tree(&data);
        let wrong_root = [0xFFu8; 32];

        let proof = tree.prove(0).unwrap();
        assert!(!MerkleStorageTree::verify_proof(&wrong_root, &proof));
    }

    #[test]
    fn item_143_prove_chunk_from_seed() {
        let data = vec![42u8; 4096];
        let tree = MerkleStorageProver::build_tree(&data);
        let root = tree.root();

        let seed = [1u8; 32];
        let proof = MerkleStorageProver::prove_chunk(&tree, &seed).unwrap();
        assert!(MerkleStorageProver::verify(&root, &proof));
    }

    #[test]
    fn item_143_deterministic_root() {
        let data = vec![42u8; 4096];
        let t1 = MerkleStorageProver::build_tree(&data);
        let t2 = MerkleStorageProver::build_tree(&data);
        assert_eq!(t1.root(), t2.root());
    }
}
