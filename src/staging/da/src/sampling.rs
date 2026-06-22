//! Deterministic sampling challenge (spec §5.4). seed = sha256(DOMAIN_SAMPLING ||
//! da_root || job_id || committee_epoch_le || verifier_id); a counter-hash PRNG
//! (sha256(seed || ctr_le), zero deps) drives seeded Fisher-Yates over [0, n_total).
//! Sample count = min(SAMPLES_PER_VERIFIER, n_total) (degenerate small-program case).
//! This is the ONLY consensus-touching code that depends on per-verifier identity.
//! New file; wired by adding `pub mod sampling;` to lib.rs.
use crate::params::{DOMAIN_SAMPLING, SAMPLES_PER_VERIFIER};
use sha2::{Digest, Sha256};

fn seed(da_root: [u8; 32], job_id: [u8; 32], epoch: u64, verifier_id: [u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN_SAMPLING);
    h.update(da_root);
    h.update(job_id);
    h.update(epoch.to_le_bytes());
    h.update(verifier_id);
    h.finalize().into()
}

/// Counter-hash PRNG: stream of u64 from sha256(seed || ctr_le).
struct CtrHash { seed: [u8; 32], ctr: u64 }
impl CtrHash {
    fn next_u64(&mut self) -> u64 {
        let mut h = Sha256::new();
        h.update(self.seed);
        h.update(self.ctr.to_le_bytes());
        self.ctr += 1;
        let d: [u8; 32] = h.finalize().into();
        u64::from_le_bytes(d[..8].try_into().unwrap())
    }
}

/// The s = min(SAMPLES_PER_VERIFIER, n_total) distinct chunk indices this verifier must sample.
pub fn sample_indices(
    da_root: [u8; 32], job_id: [u8; 32], epoch: u64, verifier_id: [u8; 32], n_total: usize,
) -> Vec<u16> {
    let s = SAMPLES_PER_VERIFIER.min(n_total);
    let mut prng = CtrHash { seed: seed(da_root, job_id, epoch, verifier_id), ctr: 0 };
    // Partial Fisher-Yates over a [0, n_total) deck: pick the first s positions.
    let mut deck: Vec<u16> = (0..n_total as u16).collect();
    for i in 0..s {
        let j = i + (prng.next_u64() as usize) % (n_total - i);
        deck.swap(i, j);
    }
    deck.truncate(s);
    deck
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_indices_in_range_and_deterministic() {
        let s = sample_indices([3; 32], [4; 32], 7, [9; 32], 100);
        assert_eq!(s.len(), 16);
        let mut sorted = s.clone(); sorted.sort(); sorted.dedup();
        assert_eq!(sorted.len(), 16, "distinct");
        assert!(s.iter().all(|&i| (i as usize) < 100));
        assert_eq!(s, sample_indices([3; 32], [4; 32], 7, [9; 32], 100), "deterministic");
    }
    #[test]
    fn tiny_set_samples_whole_domain() {
        let s = sample_indices([1; 32], [2; 32], 0, [3; 32], 4); // n_total=4 < 16
        let mut sorted = s.clone(); sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2, 3], "full coverage, no panic/loop");
    }
    #[test]
    fn seed_binds_to_verifier_id() {
        let a = sample_indices([1; 32], [2; 32], 0, [10; 32], 64);
        let b = sample_indices([1; 32], [2; 32], 0, [11; 32], 64); // different verifier
        assert_ne!(a, b, "obligation is per-verifier (non-grindable)");
    }
}
