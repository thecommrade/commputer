# Networking Layer Redesign

## Goal

Replace the ad-hoc networking in Commputer with a coherent, resource-managed layer that eliminates stream exhaustion, sync flooding, multi-producer forks, and unreliable consensus voting. Designed for 25 nodes on modern desktops at launch, upgradeable to thousands.

## Architecture

The networking layer is restructured around three principles: (1) every node is in exactly one state at all times, (2) every connection has an explicit resource budget, (3) broadcast and directed communication use different protocols. libp2p remains the transport -- the problem was never libp2p, it was using it without a design.

## Constraints

- **Network size at launch:** 25 nodes
- **Minimum hardware:** 16GB RAM, SSD, broadband (modern desktop)
- **Transport:** libp2p (TCP/QUIC, Noise/TLS, yamux)
- **Consensus:** Snowball (Avalanche-family), unchanged
- **Block time:** 2 seconds

---

## 1. Node States

A node is always in exactly one of three states:

### Syncing

The node has just joined or has fallen behind. It downloads blocks via the sync protocol (request-response). It does NOT:

- Produce blocks
- Vote on consensus proposals
- Publish to gossipsub topics

It DOES:

- Subscribe to gossipsub topics (silent listener)
- Buffer any BlockAnnounce messages it overhears (useful when sync completes)
- Respond to sync requests from other peers (it may have blocks they need)

**Entry conditions:**
- Node startup (always starts in Syncing)
- Fell behind by more than 10 blocks while Active

**Exit condition:**
- `our_height >= network_height` (verified by querying 3 peers, taking median)

### How Nodes Track network_height

Active nodes update `network_height` from two sources:
- **BlockAnnounce on gossipsub:** Every finalized block is announced. Parse the height.
- **Consensus proposals:** Incoming BlockProposals carry the height.

This is passive -- no polling. If `network_height - our_height > 10`, the node transitions to Stale/Syncing.

### Active

The node is caught up with the network. Normal operation:

- Participates in leader election
- Produces blocks when elected leader
- Votes on proposals from other leaders
- Gossips transactions and peer addresses
- Responds to sync requests

**Entry condition:**
- Sync complete (transitioned from Syncing)

**Exit conditions:**
- Falls behind by more than 10 blocks (transitions to Stale/Syncing)
- All peers disconnected (transitions to Syncing with partition flag)

### Stale

A transitional state. The node was Active but fell behind. It immediately transitions to Syncing. This state exists to log the event and clean up any in-progress consensus state before re-entering sync.

**Entry condition:**
- `network_height - our_height > 10` while Active

**Exit condition:**
- Immediately transitions to Syncing

---

## 2. Connection Manager

### One Connection Per Peer

QUIC preferred. TCP as fallback only. Today we open both TCP and QUIC to each peer, creating dual-stack churn. The new design attempts QUIC first (better NAT traversal, built-in multiplexing, no yamux overhead). Falls back to TCP + yamux only if QUIC fails.

When a peer is already connected via one transport and a second connection arrives, reject the second.

### Connection Limits

- **Max connections:** 30 (headroom above 25-node launch size for transient connections and relay, configurable)
- **Eviction policy:** When at max connections and a new peer connects, evict the peer with the lowest reputation score. Reputation is already implemented in the event loop: starts at 100, adjusted by peer behavior (bad blocks, invalid signatures, equivocation), peers below -50 are banned. See `peer_scores` in `event_loop.rs`.

### Stream Budgets Per Connection

Each connection gets a fixed stream budget, allocated by protocol:

| Protocol | Max Streams | Purpose |
|----------|-------------|---------|
| Gossipsub | 10 | Mesh maintenance, message delivery |
| Sync (request-response) | 4 | Block download batches |
| Consensus (request-response) | 4 | Block proposals, votes |
| Kademlia | 3 | Peer discovery |
| Identify | 1 | Protocol handshake |
| Reserve (relay, DCUtR) | 2 | NAT traversal |
| **Total** | **24** | |

This replaces the unconfigured yamux default of 256 streams. The total (24) is deliberately low -- each stream is used intentionally, not speculatively.

### Yamux Configuration

```
max_num_streams: 24
window_size: 256 KB (default)
```

For QUIC connections, the max concurrent bidirectional streams is set to 24 as well.

---

## 3. Sync State Machine

Sync is a proper state machine, not a timer-based loop.

### States

```
Idle -> QueryHeight -> Downloading -> Verifying -> Complete
                ^                        |
                |________________________|
                      (fell behind)
```

### QueryHeight

On entering Syncing state:

1. Send `GetHeight` to up to 3 connected peers
2. Wait up to 5 seconds for responses
3. Take the median reported height as `target_height`
4. If no peers respond, retry every 10 seconds
5. Transition to Downloading

### Downloading

1. Request blocks in batches of 10: `GetBlocks { start: our_height + 1, end: our_height + 10 }`
2. Send to ONE peer at a time
3. Wait for response (5 second timeout per batch)
4. On timeout: rotate to next peer, re-request same batch
5. On success: apply blocks, advance `our_height`, request next batch
6. **Backpressure:** Never more than 1 batch in flight. This caps sync to 4 request-response streams max (1 active + potential retries).

### Verifying

After reaching `target_height`:

1. Re-query heights from 3 peers
2. If `our_height >= median_height`: transition to Complete
3. If network advanced during sync: update `target_height`, return to Downloading

### Complete

Transition node state to Active. Clean up sync state. Begin participating in consensus.

### Peer Selection for Sync

- Prefer peers with highest reported height
- Track failed sync attempts per peer, deprioritize after 3 failures
- Never sync from a peer with reputation below 50

---

## 4. Leader Election

Deterministic round-robin. No communication needed -- all nodes independently compute the same result.

### Algorithm

```
fn leader_for_height(height: u64, validators: &[Address]) -> Address {
    let mut sorted = validators.to_vec();
    sorted.sort();  // Deterministic ordering by address bytes
    sorted[height as usize % sorted.len()]
}
```

### Why Round-Robin, Not VRF-Weighted

The codebase has a VRF-weighted `AnchorSelector` in `consensus/src/anchor.rs` that selects leaders based on Composite Resource Score. We are deliberately replacing this with round-robin for launch because: (1) with 25 nodes, fairness matters more than weighting -- every desktop contributes, every desktop gets equal turns; (2) round-robin is simpler to reason about and debug on a nascent testnet; (3) CRS-weighted selection can be reintroduced when the network grows large enough that contribution-proportional block production matters. The `AnchorSelector` code remains in the codebase for future use.

### Rules

- Only the elected leader creates a block candidate at each height
- Non-leaders wait for the leader's proposal
- The leader sends the full block directly to all peers via request-response (not gossipsub)

### View Change (Offline Leader)

If the leader doesn't produce within 6 seconds:

1. The next validator in sorted order becomes eligible
2. They produce a block and send it as a proposal
3. After another 6 seconds, the one after that becomes eligible
4. This continues until someone produces

All nodes independently compute the same fallback order:

```
fn fallback_leader(height: u64, validators: &[Address], seconds_elapsed: u64) -> Address {
    let mut sorted = validators.to_vec();
    sorted.sort();
    let primary = height as usize % sorted.len();
    let offset = (seconds_elapsed / 6) as usize;  // How many view changes
    sorted[(primary + offset) % sorted.len()]
}
```

### Clock Skew Tolerance

Desktop nodes may have 1-2 seconds of clock skew. To prevent disagreements about view change timing, peers accept blocks from a fallback leader if the proposal timestamp is within 3 seconds of their own expected view change boundary. In other words, if Node A thinks 6 seconds have passed but Node B thinks only 4.5 have passed, Node B still accepts the fallback proposal because it's within tolerance. The 6-second window (3 block times) provides enough margin for residential clock drift.

### Properties

- **Deterministic:** Same height + same validator set = same leader. No communication.
- **Fair:** Equal turns. Every validator produces the same number of blocks over time.
- **Fault tolerant:** Offline leader is skipped within 6 seconds. Chain never stalls unless all validators are offline.
- **Compatible with Snowball:** The leader proposes, but finalization still goes through Snowball voting. This preserves the Avalanche-family consensus safety guarantee.
- **Equivocation handling:** If a leader sends conflicting proposals (different blocks to different peers), the existing equivocation detection in `ConsensusManager` catches it. The leader is slashed (zero rewards) and the conflicting blocks are rejected. Snowball naturally resolves the conflict by converging on whichever block the majority saw first. See `validator_blocks` tracking in `consensus_manager.rs`.
- **Precedent:** Cosmos/Tendermint (round-robin proposer), Avalanche ProposerVM (deterministic proposer with fallback windows), Solana (pre-computed leader schedule).

---

## 5. Validator Set

The leader election and consensus voting require a known validator list. This is determined by the existing validator registry in `ChainState`:

- **Who is a validator:** Any node that has registered via `RegisterValidator` transaction and has `ValidatorStatus::Active`. Registration is already implemented in `event_loop.rs` (auto-registers on startup with 100% contribution).
- **When the set changes:** Validator set is evaluated at epoch boundaries (every 3600 seconds). Mid-epoch joins are registered but don't participate in leader rotation until the next epoch. This prevents the sorted validator list from changing mid-epoch, which would cause disagreements about who leads which height.
- **Bootstrap:** During early network (total_emitted < MINIMUM_VALIDATOR_STAKE), the stake requirement is waived. All registered nodes are validators.
- **Consensus quorum:** Votes are counted from the current epoch's validator set only. A vote from an unregistered peer is ignored.

---

## 6. Gossipsub Simplification

### Principle

Gossipsub is for broadcast (one-to-many). Request-response is for directed communication (one-to-one). Today we mix them. The redesign separates them cleanly.

### Gossipsub Topics (3, down from 5)

| Topic | Message | Purpose |
|-------|---------|---------|
| `commputer/blocks/0.1` | BlockAnnounce (hash, height, producer) | Notify network a block was finalized |
| `commputer/txs/0.1` | Transaction | Broadcast new transactions |
| `commputer/peers/0.1` | Peer addresses | Peer discovery |

### Moved to Request-Response

| Message | Old Transport | New Transport | Reason |
|---------|---------------|---------------|--------|
| Block proposals | gossipsub (BlockProposal) | request-response (direct to each peer) | Guaranteed delivery, no dedup issues |
| Consensus votes | gossipsub (VoteResponse) | request-response (direct to proposer) | Guaranteed delivery to specific peer |
| Proof challenges | gossipsub (ProofMessage) | request-response (direct to challenged peer) | Per-peer, not broadcast |

### Removed

| Topic | Reason |
|-------|--------|
| `commputer/consensus/0.1` | All consensus moves to request-response |
| `commputer/proofs/0.1` | Proof challenges are per-peer |

### Gossipsub Configuration

- **Mesh size:** 6 (appropriate for 25 nodes, every node is 1-2 hops from every other)
- **Heartbeat:** 1 second (unchanged)
- **Message signing:** Enabled (unchanged)
- **Rate limit:** 50 messages/sec per peer (unchanged, but less likely to trigger since consensus traffic is off gossipsub)

---

## 7. Consensus Protocol (Request-Response)

A new request-response protocol for consensus, separate from the sync protocol.

### Protocol ID

`/commputer/consensus/1`

### Message Types

```rust
enum ConsensusRequest {
    /// Leader sends full block proposal to each peer.
    BlockProposal { block: Block, height: u64 },
    /// Leader requests a vote from a peer that hasn't responded.
    VoteRequest { height: u64, block_hash: BlockHash },
}

enum ConsensusResponse {
    /// Peer validates and votes.
    Vote { height: u64, preference: BlockHash, accept: bool },
    /// Peer hasn't seen this height yet (still syncing).
    NotReady { height: u64 },
}
```

### Flow

1. Leader builds block, sends `BlockProposal` directly to all Active peers
2. Each peer validates the block:
   - Correct parent hash, valid signatures, valid timestamps
   - Proposer is the expected leader for this height
3. Peer responds with `Vote { accept: true, preference: block_hash }`
4. Leader feeds votes into Snowball voter
5. On Snowball finalization: leader broadcasts `BlockAnnounce` on gossipsub
6. All peers (including any that missed the proposal) apply the block
7. **Leader crash fallback:** If the leader finalizes but crashes before broadcasting BlockAnnounce, peers who voted still have the block. After 6 seconds with no announcement, any peer who has the finalized block may broadcast the BlockAnnounce themselves. Peers who missed the proposal entirely request the block via sync protocol when they see the announcement.

### Codec

Same pattern as sync protocol: JSON with 4-byte length prefix, 10 MB max. Reuses the existing `SyncCodec` pattern.

### Stream Budget

4 streams allocated for consensus request-response per connection. The leader sends proposals to N peers concurrently, but only needs 1 stream per peer (request + response on same stream). The budget of 4 handles view change scenarios where multiple heights overlap briefly.

---

## 8. Complete Message Flow

### Normal Block Cycle (2 seconds)

```
1. All nodes compute: leader for height 42 = Node B
2. Node B builds block, signs it
3. Node B sends BlockProposal to peers A, C, D... (request-response, direct)
4. Peers validate, send Vote back to Node B (request-response, direct)
5. Node B collects votes, Snowball finalizes
6. Node B publishes BlockAnnounce on gossipsub (broadcast)
7. All peers apply block 42
8. 2 seconds later: leader for height 43 = Node C. Repeat.
```

### Offline Leader

```
1. Leader for height 42 = Node B. Node B is offline.
2. All nodes wait 6 seconds. No proposal arrives.
3. All nodes independently compute: fallback leader = Node C.
4. Node C builds block, sends BlockProposal to peers.
5. Peers validate (check that 6 seconds elapsed, Node C is valid fallback).
6. Normal voting and finalization.
```

### New Node Joining

```
1. Node E starts. Enters Syncing state.
2. Connects to seed nodes. Queries heights from 3 peers.
3. Network is at height 500. Node E starts downloading batches of 10.
4. After ~10 seconds, Node E reaches height 500. Verifies with peers.
5. Transitions to Active. Enters leader rotation.
6. Next time it's Node E's turn: produces a block.
```

### Node Falls Behind

```
1. Node D is Active at height 500. Gets busy (slow disk, network hiccup).
2. Network advances to height 515. Node D is 15 blocks behind (>10 threshold).
3. Node D transitions to Stale, then Syncing.
4. Downloads missing blocks via sync protocol.
5. Catches up. Transitions back to Active.
```

---

## 9. Migration Path

This redesign touches four areas. Each can be implemented and tested independently:

1. **Node state machine** (Syncing/Active/Stale) -- replaces the `sync_complete` boolean
2. **Connection manager** (stream budgets, QUIC preference, single connection per peer) -- replaces libp2p defaults
3. **Leader election** (round-robin + view change) -- replaces "everyone produces"
4. **Consensus on request-response** (direct proposals + votes) -- replaces gossipsub consensus topic

The implementation order matters:

- **Leader election first:** Eliminates multi-producer forks immediately. Can be tested with current gossipsub voting.
- **Node state machine second:** Prevents sync flooding. Can be tested with current sync protocol.
- **Connection manager third:** Prevents stream exhaustion. Configuration change, minimal code.
- **Consensus on request-response last:** Largest change, most benefit. Eliminates gossipsub voting issues entirely.

Each step produces a testable, deployable improvement. The testnet gets better incrementally, not all-at-once.

---

## 10. What Doesn't Change

- **Snowball consensus algorithm:** Unchanged. Still the core finalization mechanism.
- **Block structure:** Unchanged. Same header, same transactions, same proofs.
- **Sync protocol messages:** `GetBlock`, `GetBlocks`, `GetHeight` unchanged. The state machine wraps them. (Note: `max_concurrent_streams` in `sync_behaviour()` will be reduced from 256 to match the stream budget. This is a configuration change, not a protocol change.)
- **Transaction format:** Unchanged.
- **State management:** ChainState, BlockStore, StateDiff all unchanged.
- **RPC server:** Unchanged.
- **Wallet/keystore:** Unchanged.
- **Epoch/proof system:** Unchanged.

---

## 11. Scaling Notes

This design targets 25 nodes. Here's what changes at scale:

| Scale | Change Needed |
|-------|---------------|
| **100 nodes** | Increase max connections to 50. Gossipsub mesh handles the rest. |
| **1000 nodes** | Leader sends proposals to a subset (e.g. 8 peers) who relay. Structured propagation. |
| **10000+ nodes** | Weighted leader election (CRS score instead of round-robin). Sharded gossip topics. |

The node state machine, sync protocol, connection manager, and consensus request-response protocol all remain the same. Only the fan-out strategy and leader selection weighting change.

---

## 12. Problem Resolution Matrix

| Problem | Root Cause | Fix |
|---------|-----------|-----|
| Yamux stream exhaustion | Unlimited streams, no budget | 24 streams per connection, allocated by protocol |
| Sync flooding on join | No backpressure, unlimited requests | Sync state machine: 1 batch of 10, wait before next |
| 2-node fork at height 920 | Both nodes produce, Snowball 1-1 tie | Round-robin: one leader per height |
| Dual-stack connection churn | TCP+QUIC both open per peer | QUIC preferred, one connection per peer |
| Optiplex vote asymmetry | Gossipsub mesh doesn't guarantee vote delivery | Votes via direct request-response |
| Stale consensus heights | Syncing nodes enter consensus for every block | Syncing nodes don't participate in consensus |
| Rate limit banning during sync | Gossipsub rate limiter hits sync traffic | Sync on request-response (not rate limited). Syncing nodes silent on gossipsub |
| Gossipsub dedup dropping queries | Same content = same hash = deduped | Consensus off gossipsub entirely |
| Consensus timeout scatter | Proposals for stale heights during sync | Consensus scoped to tip+1, syncing nodes excluded |
