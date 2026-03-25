//! Merkle proof generation and verification for light client support.
//! Features 212-213.

use sha2::{Sha256, Digest};

/// A merkle inclusion proof: a list of sibling hashes with direction indicators.
/// `true` means the sibling is on the right; `false` means left.
#[derive(Debug, Clone)]
pub struct MerkleProof {
    pub siblings: Vec<(bool, [u8; 32])>,
}

/// Generate a merkle inclusion proof for the leaf at `tx_index` within `leaves`.
/// Returns `None` if the index is out of bounds or there are no leaves.
pub fn generate_merkle_proof(leaves: &[[u8; 32]], tx_index: usize) -> Option<MerkleProof> {
    if leaves.is_empty() || tx_index >= leaves.len() {
        return None;
    }
    if leaves.len() == 1 {
        // Single leaf: the root is the leaf itself, no siblings needed.
        return Some(MerkleProof { siblings: vec![] });
    }

    let mut siblings = Vec::new();
    let mut current_layer: Vec<[u8; 32]> = leaves.to_vec();
    let mut index = tx_index;

    while current_layer.len() > 1 {
        // If odd number of leaves, duplicate the last one.
        if current_layer.len() % 2 != 0 {
            let last = *current_layer.last().unwrap();
            current_layer.push(last);
        }

        let sibling_index = if index % 2 == 0 { index + 1 } else { index - 1 };
        // is_right: true if the sibling is to the right of us
        let is_right = index % 2 == 0;
        siblings.push((is_right, current_layer[sibling_index]));

        // Build next layer
        let mut next_layer = Vec::with_capacity(current_layer.len() / 2);
        for pair in current_layer.chunks(2) {
            let mut hasher = Sha256::new();
            hasher.update(pair[0]);
            hasher.update(pair[1]);
            let hash = hasher.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&hash);
            next_layer.push(out);
        }

        index /= 2;
        current_layer = next_layer;
    }

    Some(MerkleProof { siblings })
}

/// Verify a merkle inclusion proof: given the leaf hash, proof, and expected root,
/// check that the proof connects the leaf to the root.
pub fn verify_merkle_proof(leaf_hash: [u8; 32], proof: &MerkleProof, root: [u8; 32]) -> bool {
    let mut current = leaf_hash;

    for (is_right, sibling) in &proof.siblings {
        let mut hasher = Sha256::new();
        if *is_right {
            // sibling is on the right
            hasher.update(current);
            hasher.update(sibling);
        } else {
            // sibling is on the left
            hasher.update(sibling);
            hasher.update(current);
        }
        let hash = hasher.finalize();
        current = [0u8; 32];
        current.copy_from_slice(&hash);
    }

    current == root
}

/// Compute the merkle root of a set of 32-byte leaves. Matches block.rs logic.
pub fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    if leaves.len() == 1 {
        return leaves[0];
    }

    let mut next_level = Vec::with_capacity((leaves.len() + 1) / 2);
    for pair in leaves.chunks(2) {
        let mut hasher = Sha256::new();
        hasher.update(pair[0]);
        if pair.len() == 2 {
            hasher.update(pair[1]);
        } else {
            hasher.update(pair[0]); // duplicate odd leaf
        }
        let hash = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        next_level.push(out);
    }

    merkle_root(&next_level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Sha256, Digest};

    fn hash_bytes(data: &[u8]) -> [u8; 32] {
        let h = Sha256::digest(data);
        let mut out = [0u8; 32];
        out.copy_from_slice(&h);
        out
    }

    // Feature 212: Merkle proof generation and verification

    #[test]
    fn merkle_proof_single_leaf() {
        let leaf = hash_bytes(b"tx0");
        let leaves = vec![leaf];
        let root = merkle_root(&leaves);
        assert_eq!(root, leaf);

        let proof = generate_merkle_proof(&leaves, 0).unwrap();
        assert!(proof.siblings.is_empty());
        assert!(verify_merkle_proof(leaf, &proof, root));
    }

    #[test]
    fn merkle_proof_two_leaves() {
        let leaves = vec![hash_bytes(b"tx0"), hash_bytes(b"tx1")];
        let root = merkle_root(&leaves);

        // Proof for leaf 0
        let proof0 = generate_merkle_proof(&leaves, 0).unwrap();
        assert!(verify_merkle_proof(leaves[0], &proof0, root));

        // Proof for leaf 1
        let proof1 = generate_merkle_proof(&leaves, 1).unwrap();
        assert!(verify_merkle_proof(leaves[1], &proof1, root));

        // Wrong leaf should fail
        assert!(!verify_merkle_proof(leaves[1], &proof0, root));
    }

    #[test]
    fn merkle_proof_four_leaves() {
        let leaves: Vec<[u8; 32]> = (0..4)
            .map(|i| hash_bytes(format!("tx{}", i).as_bytes()))
            .collect();
        let root = merkle_root(&leaves);

        for i in 0..4 {
            let proof = generate_merkle_proof(&leaves, i).unwrap();
            assert!(
                verify_merkle_proof(leaves[i], &proof, root),
                "Proof failed for leaf {}",
                i
            );
        }
    }

    #[test]
    fn merkle_proof_odd_leaves() {
        let leaves: Vec<[u8; 32]> = (0..5)
            .map(|i| hash_bytes(format!("tx{}", i).as_bytes()))
            .collect();
        let root = merkle_root(&leaves);

        for i in 0..5 {
            let proof = generate_merkle_proof(&leaves, i).unwrap();
            assert!(
                verify_merkle_proof(leaves[i], &proof, root),
                "Proof failed for leaf {}",
                i
            );
        }
    }

    #[test]
    fn merkle_proof_seven_leaves() {
        let leaves: Vec<[u8; 32]> = (0..7)
            .map(|i| hash_bytes(format!("tx{}", i).as_bytes()))
            .collect();
        let root = merkle_root(&leaves);

        for i in 0..7 {
            let proof = generate_merkle_proof(&leaves, i).unwrap();
            assert!(
                verify_merkle_proof(leaves[i], &proof, root),
                "Proof failed for leaf {}",
                i
            );
        }
    }

    #[test]
    fn merkle_proof_out_of_bounds() {
        let leaves = vec![hash_bytes(b"tx0")];
        assert!(generate_merkle_proof(&leaves, 1).is_none());
        assert!(generate_merkle_proof(&[], 0).is_none());
    }

    #[test]
    fn merkle_proof_wrong_root_fails() {
        let leaves = vec![hash_bytes(b"tx0"), hash_bytes(b"tx1")];
        let root = merkle_root(&leaves);
        let proof = generate_merkle_proof(&leaves, 0).unwrap();

        let fake_root = [0xFFu8; 32];
        assert!(!verify_merkle_proof(leaves[0], &proof, fake_root));
        assert!(verify_merkle_proof(leaves[0], &proof, root));
    }

    // Feature 213: Light client verification test

    #[test]
    fn light_client_verification() {
        // Simulate: create a block with transactions, extract header + merkle proof,
        // verify tx inclusion using only the header and proof (no full block needed).
        let tx_hashes: Vec<[u8; 32]> = (0..10)
            .map(|i| hash_bytes(format!("transaction_{}", i).as_bytes()))
            .collect();

        let tx_root = merkle_root(&tx_hashes);

        // A light client receives only the header (containing tx_root) and a merkle proof.
        // Verify that transaction 3 is included.
        let tx_index = 3;
        let proof = generate_merkle_proof(&tx_hashes, tx_index).unwrap();

        // Light client verification: given tx_hash, proof, and header.tx_root
        assert!(verify_merkle_proof(tx_hashes[tx_index], &proof, tx_root));

        // Verify a forged tx hash fails
        let forged_hash = hash_bytes(b"forged_transaction");
        assert!(!verify_merkle_proof(forged_hash, &proof, tx_root));
    }
}
