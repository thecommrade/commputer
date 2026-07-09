// peer_hash.rs — full-PeerId bucket keys (E9/[20]/[30]) + the sync-window clamp bound.
//
// WHAT: a non-grindable bucket key derived from the FULL PeerId bytes (the former
// `peer.to_bytes()[..8]` fold exposed only ~2 bytes of ed25519 key entropy behind
// a constant multihash prefix, so ~65k grinds could collide a victim's rate-limit
// / vote-tracking bucket and starve it). Plus `MAX_SYNC_WINDOW`, the per-advance
// clamp bound the protected `advance_network_height` uses, pinned EQUAL to
// `node_state::SANE_MAX_GAP` so the two clamps never fight.
//
// WIRING (INERT until the PROTECTED event_loop commit): event_loop.rs calls
// `peer_bucket` at the 3 consensus-limiter / health-monitor rekey sites and
// `peer_bucket_tagged` in the sync serve handler; `advance_network_height`
// references `MAX_SYNC_WINDOW`. Registered via `pub mod peer_hash;` in lib.rs.
// FILES NEEDING CHANGES: event_loop.rs (PROTECTED) at the reset.

use std::hash::{Hash, Hasher};

/// SECURITY(net-height §0): max blocks ahead of our tip a single advance may raise
/// the target. Pinned EQUAL to `node_state::SANE_MAX_GAP` so the two clamps never
/// fight (P6). The `const _` assert below turns any future divergence into a build
/// error.
pub const MAX_SYNC_WINDOW: u64 = 2000;
const _: () = assert!(MAX_SYNC_WINDOW == crate::node_state::SANE_MAX_GAP);

/// Non-grindable bucket key from the FULL PeerId bytes (was `[..8]` ⇒ ~2 key bytes).
pub fn peer_bucket(peer: &libp2p::PeerId) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    peer.to_bytes().hash(&mut h);
    h.finish()
}

/// Separate GetBlock (tag 0) / GetBlocks (tag 1) buckets so batch sync is not
/// starved by single-block request rate limiting (and vice versa).
pub fn peer_bucket_tagged(peer: &libp2p::PeerId, tag: u8) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    peer.to_bytes().hash(&mut h);
    tag.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_sync_window_matches_sane_max_gap() {
        // Mirror of the compile-time `const _` assert, as a runtime check too.
        assert_eq!(MAX_SYNC_WINDOW, crate::node_state::SANE_MAX_GAP);
    }

    #[test]
    fn peer_bucket_is_deterministic_per_peer() {
        let p = libp2p::PeerId::random();
        assert_eq!(peer_bucket(&p), peer_bucket(&p));
    }

    #[test]
    fn distinct_peers_get_distinct_buckets() {
        // Not a collision proof, but two random PeerIds must not fold to the same
        // bucket in practice (the whole point of hashing the full bytes).
        let a = libp2p::PeerId::random();
        let b = libp2p::PeerId::random();
        assert_ne!(peer_bucket(&a), peer_bucket(&b));
    }

    #[test]
    fn tagged_buckets_differ_by_tag() {
        let p = libp2p::PeerId::random();
        assert_ne!(peer_bucket_tagged(&p, 0), peer_bucket_tagged(&p, 1));
    }
}
