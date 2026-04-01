# Bootstrap Leader Design

**Date:** 2026-04-01
**Status:** Draft
**Scope:** Designate the seed node as bootstrap leader to prevent competing blocks at genesis

---

## Problem

When multiple nodes start simultaneously on a fresh chain, each sees < 2 validators (only knows about itself), produces its own block at height 1, and Snowball consensus can't converge because votes are split across competing blocks. The network stalls permanently at height 1.

Every top L1 solves this the same way: someone is designated as the bootstrap authority.

- **Bitcoin:** Satoshi mined the genesis block solo. Hardcoded.
- **Ethereum:** Genesis config defines initial validator set.
- **Solana:** Explicit `bootstrap_leader` in genesis config produces the first epoch.
- **Avalanche:** Beacon validators maintained by the Avalanche Foundation.
- **Tendermint:** Genesis file specifies initial validators, 2/3+ must agree.

The pattern is universal: accept initial centralization to bootstrap decentralization.

---

## Design

### Bootstrap Leader Identification

A node is the bootstrap leader if it was started with **no `--seeds` argument**. This is implicit -- no extra config needed. The seed node starts first, has no seeds itself, and is the natural bootstrap authority.

- `--seeds` absent or empty: `is_bootstrap_leader = true`
- `--seeds` present: `is_bootstrap_leader = false`

### Behavior During Bootstrap (< 2 on-chain validators)

**Bootstrap leader (no seeds):**
- Produces blocks freely at height 1+
- Self-finalizes via solo-node consensus path (peer_count == 0 self-vote, already implemented in fork recovery)
- Continues producing until a second validator registers on-chain

**Non-bootstrap nodes (have seeds):**
- Do NOT produce blocks during bootstrap
- Sync from the seed node
- Register as validators via transaction
- Wait for round-robin leader election to begin

### Transition to Normal Consensus

Once 2+ validators are registered on-chain, the existing `validators.len() >= 2` check in `handle_block_tick` activates normal round-robin leader election. The bootstrap leader flag becomes irrelevant -- it only gates the `validators.len() < 2` bypass.

### Implementation

One new boolean field on `EventLoop`:

```rust
pub is_bootstrap_leader: bool,
```

Set from CLI args at startup (true when `seeds` is empty/absent).

One code change in `handle_block_tick`. Current code (around line 2459):

```rust
if validators.len() >= 2 {
    // Strict leader election...
}
```

The implicit else (validators.len() < 2) currently allows ALL nodes to produce. Change to:

```rust
if validators.len() >= 2 {
    // Strict leader election (unchanged)...
} else if !self.is_bootstrap_leader {
    // Not the bootstrap leader and < 2 validators -- don't produce.
    return;
}
```

---

## What This Does NOT Change

- Round-robin leader election (untouched, takes over at 2+ validators)
- Snowball voting (untouched)
- Fork recovery (untouched)
- Sync machine (untouched)
- Solo-node self-finalization (untouched, bootstrap leader uses it)

## Edge Cases

**Solo node (0 peers, no seeds):** Bootstrap leader, produces blocks, self-finalizes. Correct -- this is the intended testnet/dev behavior.

**All nodes started with seeds:** No bootstrap leader. No blocks produced until someone manually starts a seedless node. This is correct -- a network needs a seed.

**Bootstrap leader goes offline after producing blocks:** Other nodes sync to its height. Once 2+ validators exist, round-robin leader election handles leader absence via 6s view change fallback.

**Second validator joins mid-bootstrap:** It syncs to the bootstrap leader's height, registers as validator. Once the registration transaction is in a block, `validators.len()` becomes 2 and normal leader election begins on the next block.

## Testing

- `test_bootstrap_leader_flag` -- node with no seeds has `is_bootstrap_leader = true`
- `test_non_bootstrap_no_produce` -- node with seeds and < 2 validators does not produce
- `test_bootstrap_produces` -- node with no seeds and < 2 validators produces
- Manual: start Optiplex (no seeds), wait for blocks, start Solarplexus + laptop (with seeds), verify they sync and transition to round-robin
