//! Replication / pinning records (spec §6.6). Responsible set = the K provider-ids
//! XOR-closest to a chunk_hash, so every node agrees who SHOULD hold each chunk.
//!
//! Where it wires in: facade.rs (Layer 4) uses `xor_distance` for deterministic
//! provider ordering; the active repair daemon (deferred, spec §10.6) uses
//! `needs_repair` + `PROVIDER_RECORD_TTL_TICKS`/`PROVIDER_REPUBLISH_TICKS`.
//! No existing files need changes for Layer 3.
use crate::params::{ProviderId, REPLICATION_FACTOR_K};
use std::collections::BTreeSet;

pub const PROVIDER_RECORD_TTL_TICKS: u64 = 48 * 3600;   // 48h-equiv
pub const PROVIDER_REPUBLISH_TICKS: u64 = 22 * 3600;    // 22h-equiv (< TTL)

/// A pinning/advertisement record. BTreeSet => deterministic iteration (never HashSet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRecord {
    pub chunk_hash: [u8; 32],
    pub providers: BTreeSet<ProviderId>,
    pub expires_at: u64,
    pub republish_at: u64,
}

/// XOR distance as a big-endian byte comparison key (lower = closer).
pub fn xor_distance(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut d = [0u8; 32];
    for i in 0..32 { d[i] = a[i] ^ b[i]; }
    d
}

/// The K providers responsible for `chunk_hash` (XOR-closest), deterministic.
pub fn responsible_set(chunk_hash: [u8; 32], peers: &[ProviderId]) -> Vec<ProviderId> {
    let mut sorted = peers.to_vec();
    sorted.sort_by_key(|p| xor_distance(&chunk_hash, &p.0));
    sorted.truncate(REPLICATION_FACTOR_K);
    sorted
}

/// Repair signal: true when the live providers fall below K (a hook; the active
/// repair daemon is deferred, spec §10.6).
pub fn needs_repair(live_providers: usize) -> bool {
    live_providers < REPLICATION_FACTOR_K
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ProviderId;

    #[test]
    fn responsible_set_is_k_xor_closest_and_deterministic() {
        let peers: Vec<ProviderId> = (0..40u8).map(|i| ProviderId([i; 32])).collect();
        let chunk_hash = [20u8; 32];
        let a = responsible_set(chunk_hash, &peers);
        let b = responsible_set(chunk_hash, &peers);
        assert_eq!(a, b, "deterministic");
        assert_eq!(a.len(), crate::params::REPLICATION_FACTOR_K.min(peers.len()));
        // every chosen peer is XOR-closer than every unchosen peer
        let chosen: std::collections::BTreeSet<_> = a.iter().copied().collect();
        let max_chosen = a.iter().map(|p| xor_distance(&chunk_hash, &p.0)).max().unwrap();
        for p in &peers {
            if !chosen.contains(p) {
                assert!(xor_distance(&chunk_hash, &p.0) >= max_chosen);
            }
        }
    }

    #[test]
    fn record_ttl_and_republish() {
        assert!(PROVIDER_REPUBLISH_TICKS < PROVIDER_RECORD_TTL_TICKS);
    }
}
