//! Vendored binary sha256 Merkle tree (spec §5.3). Domain separation:
//!   leaf     = sha256(0x00 || index_le(4) || chunk_bytes)
//!   internal = sha256(0x01 || left || right)
//! Odd-node rule (PINNED): promote the lone right node unchanged (no duplicate-leaf
//! malleability — duplicating would enable a CVE-2012-2459-style forgery). Zero deps
//! beyond in-tree sha2.
use crate::params::{DOMAIN_LEAF, DOMAIN_NODE};
use sha2::{Digest, Sha256};

pub fn leaf_hash(index: usize, chunk: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([DOMAIN_LEAF]);
    h.update((index as u32).to_le_bytes());
    h.update(chunk);
    h.finalize().into()
}
pub fn node_hash(l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([DOMAIN_NODE]);
    h.update(l);
    h.update(r);
    h.finalize().into()
}

fn level_up(level: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut up = Vec::with_capacity(level.len().div_ceil(2));
    let mut i = 0;
    while i < level.len() {
        if i + 1 < level.len() {
            up.push(node_hash(&level[i], &level[i + 1]));
            i += 2;
        } else {
            up.push(level[i]); // promote lone right node unchanged
            i += 1;
        }
    }
    up
}

pub fn build_root(chunks: &[Vec<u8>]) -> [u8; 32] {
    assert!(!chunks.is_empty(), "Merkle over zero leaves is undefined");
    let mut level: Vec<[u8; 32]> =
        chunks.iter().enumerate().map(|(i, c)| leaf_hash(i, c)).collect();
    while level.len() > 1 {
        level = level_up(&level);
    }
    level[0]
}

/// Inclusion path: the sibling hash at each level (bottom-up). A `None` marks a
/// promoted-without-sibling level (the verifier skips hashing at that step).
pub fn prove(chunks: &[Vec<u8>], index: usize) -> Vec<Option<[u8; 32]>> {
    let mut level: Vec<[u8; 32]> =
        chunks.iter().enumerate().map(|(i, c)| leaf_hash(i, c)).collect();
    let mut idx = index;
    let mut path = Vec::new();
    while level.len() > 1 {
        if idx % 2 == 0 {
            path.push(level.get(idx + 1).copied()); // right sibling, or None if promoted
        } else {
            path.push(Some(level[idx - 1]));         // left sibling
        }
        idx /= 2;
        level = level_up(&level);
    }
    path
}

pub fn verify(root: &[u8; 32], index: usize, chunk: &[u8], path: &[Option<[u8; 32]>]) -> bool {
    let mut acc = leaf_hash(index, chunk);
    let mut idx = index;
    for sib in path {
        acc = match (idx % 2, sib) {
            (0, Some(r)) => node_hash(&acc, r),
            (0, None) => acc,        // we were the promoted lone node
            (1, Some(l)) => node_hash(l, &acc),
            (1, None) => return false, // a left child can never lack a sibling
            _ => return false,       // unreachable (idx%2 ∈ {0,1}) but the compiler can't prove it on usize
        };
        idx /= 2;
    }
    &acc == root
}

#[cfg(test)]
mod tests {
    use super::*;
    fn leaves() -> Vec<Vec<u8>> { (0..5u8).map(|i| vec![i; 8]).collect() } // 5 = odd count
    #[test]
    fn root_inclusion_proof_verifies() {
        let ls = leaves();
        let root = build_root(&ls);
        for i in 0..ls.len() {
            let path = prove(&ls, i);
            assert!(verify(&root, i, &ls[i], &path), "leaf {i} must verify");
        }
    }
    #[test]
    fn tamper_is_detected() {
        let ls = leaves();
        let root = build_root(&ls);
        let path = prove(&ls, 2);
        let mut bad = ls[2].clone(); bad[0] ^= 0xFF;
        assert!(!verify(&root, 2, &bad, &path), "tampered chunk must fail");
        assert!(!verify(&root, 3, &ls[2], &path), "wrong index must fail");
    }
    #[test]
    fn second_preimage_leaf_vs_internal() {
        // a leaf hash must never collide with an internal node hash (0x00 vs 0x01 tag)
        let a = leaf_hash(0, b"x");
        let b = node_hash(&[0u8;32], &[0u8;32]);
        assert_ne!(a, b);
    }
    #[test]
    fn root_is_deterministic() {
        assert_eq!(build_root(&leaves()), build_root(&leaves()));
    }
}
