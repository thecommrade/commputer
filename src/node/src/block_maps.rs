// block_maps.rs — bounds for the block-ingest bookkeeping maps (findings [13]/[24]).
//
// WHAT: handle_received_block (event_loop.rs, PROTECTED) keeps three maps that
// previously grew without bound, populated from PRE-VALIDATION blocks:
//   * orphan_pool:      HashMap<BlockHash, Vec<Block>>     — finding [13], HIGH
//   * producer_blocks:  HashMap<(Address, u64), BlockHash> — finding [24], MEDIUM
//   * block_seen_times: HashMap<BlockHash, u64>            — finding [24], MEDIUM
// The distinct-parent-only cap (`orphan_pool.len() < 100`) never bounded the
// per-parent Vec, and the two maps were never pruned. This module holds the pure
// cap/prune logic so the PROTECTED event_loop hunks stay one-liners and the
// bounds are unit-testable.
//
// WIRING (LIVE with the alpha-reset enforcement batch): event_loop.rs calls
//   * bounded_orphan_insert — 2 sites: handle_received_block (~2033) and
//     apply_synced_block (~3197), both AFTER validate_block_from_peer.
//   * prune_producer_blocks / prune_block_seen_times — handle_received_block,
//     AFTER validate_block_from_peer succeeds.
// FILES NEEDING CHANGES: event_loop.rs (PROTECTED) + `pub mod block_maps;` in lib.rs.

use std::collections::HashMap;

use commputer_core::block::{Block, BlockHash};
use commputer_core::identity::Address;

/// [13]: max orphan blocks buffered under a single parent hash.
pub const MAX_ORPHANS_PER_PARENT: usize = 20;
/// [13]: max orphan blocks buffered across ALL parents.
pub const MAX_ORPHANS_TOTAL: usize = 200;
/// [24]: hard ceiling on the producer_blocks equivocation-tracking map.
pub const MAX_PRODUCER_BLOCKS: usize = 10_000;
/// [24]: hard ceiling on the block_seen_times propagation-timing map.
pub const MAX_BLOCK_SEEN_TIMES: usize = 10_000;

/// [13]: buffer `block` under `parent`, enforcing BOTH a per-parent Vec cap and a
/// global total cap. Refuse-on-full (drop the incoming block): memory is fully
/// bounded and `process_orphans` drains a bucket as soon as its parent arrives.
/// Returns true if buffered, false if dropped. Post-flip the orphan supply is
/// itself gated by producer-signature validity (callers insert only AFTER
/// validate_block_from_peer), so refuse-on-full cannot be cheaply abused.
pub fn bounded_orphan_insert(
    pool: &mut HashMap<BlockHash, Vec<Block>>,
    parent: BlockHash,
    block: Block,
) -> bool {
    let total: usize = pool.values().map(|v| v.len()).sum();
    if total >= MAX_ORPHANS_TOTAL {
        return false;
    }
    let bucket = pool.entry(parent).or_default();
    if bucket.len() >= MAX_ORPHANS_PER_PARENT {
        return false;
    }
    bucket.push(block);
    true
}

/// [24]: bound producer_blocks. O(1) when under the cap. Once over it, drop every
/// entry at/below the applied `tip` (those heights can no longer equivocate our
/// chain); if the survivors still exceed the cap (a flood of validly-signed
/// future-height blocks), clear the map — equivocation detection is best-effort
/// observability and losing it is harmless to consensus.
pub fn prune_producer_blocks(map: &mut HashMap<(Address, u64), BlockHash>, tip: u64) {
    if map.len() <= MAX_PRODUCER_BLOCKS {
        return;
    }
    map.retain(|(_, h), _| *h > tip);
    if map.len() > MAX_PRODUCER_BLOCKS {
        map.clear();
    }
}

/// [24]: bound block_seen_times. O(1) when under the cap. A BlockHash cannot be
/// cheaply mapped back to a height, so once over the cap we clear the map;
/// propagation timing is pure observability (percentiles accumulate separately in
/// `propagation_delays`), so a periodic reset is harmless. `_tip` is reserved for
/// a future height-aware prune.
pub fn prune_block_seen_times(map: &mut HashMap<BlockHash, u64>, _tip: u64) {
    if map.len() > MAX_BLOCK_SEEN_TIMES {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::block::{BlockHeader, CURRENT_PROTOCOL_VERSION};

    fn blk(parent: u8) -> Block {
        Block {
            header: BlockHeader {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                height: 1,
                parent_hash: BlockHash([parent; 32]),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 0,
                producer: Address([0u8; 32]),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None,
            epoch_summary: None,
        }
    }

    #[test]
    fn orphan_per_parent_cap_holds() {
        let mut pool: HashMap<BlockHash, Vec<Block>> = HashMap::new();
        let parent = BlockHash([7u8; 32]);
        let mut buffered = 0;
        for _ in 0..(MAX_ORPHANS_PER_PARENT + 5) {
            if bounded_orphan_insert(&mut pool, parent, blk(7)) {
                buffered += 1;
            }
        }
        assert_eq!(buffered, MAX_ORPHANS_PER_PARENT);
        assert_eq!(pool.get(&parent).unwrap().len(), MAX_ORPHANS_PER_PARENT);
    }

    #[test]
    fn orphan_total_cap_holds() {
        let mut pool: HashMap<BlockHash, Vec<Block>> = HashMap::new();
        for p in 0u16..1024 {
            let b = (p % 251) as u8;
            let _ = bounded_orphan_insert(&mut pool, BlockHash([b; 32]), blk(b));
        }
        let total: usize = pool.values().map(|v| v.len()).sum();
        assert!(total <= MAX_ORPHANS_TOTAL, "total {total} exceeds cap");
    }

    #[test]
    fn producer_blocks_under_cap_is_noop() {
        let mut m: HashMap<(Address, u64), BlockHash> = HashMap::new();
        m.insert((Address([0u8; 32]), 3), BlockHash([1u8; 32]));
        prune_producer_blocks(&mut m, 100);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn producer_blocks_over_cap_drops_below_tip() {
        let mut m: HashMap<(Address, u64), BlockHash> = HashMap::new();
        let tip = MAX_PRODUCER_BLOCKS as u64;
        for i in 0..=(tip + 1) {
            m.insert((Address([0u8; 32]), i), BlockHash([0u8; 32]));
        }
        prune_producer_blocks(&mut m, tip);
        assert_eq!(m.len(), 1);
        assert!(m.keys().all(|(_, h)| *h > tip));
    }

    #[test]
    fn block_seen_times_over_cap_clears() {
        let mut m: HashMap<BlockHash, u64> = HashMap::new();
        for i in 0..=(MAX_BLOCK_SEEN_TIMES as u64 + 1) {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&i.to_le_bytes());
            m.insert(BlockHash(b), i);
        }
        prune_block_seen_times(&mut m, 0);
        assert!(m.is_empty());
    }
}
