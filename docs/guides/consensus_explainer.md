# Consensus Mechanism — Plain English Guide

*Commputer uses Snowball consensus with round-robin leader election.*

---

## What Is Snowball?

Snowball is part of the Avalanche family of consensus protocols. Unlike Bitcoin (longest chain wins) or Tendermint (2/3 majority vote per round), Snowball works by repeated sampling:

- A node repeatedly queries random subsets of the network
- If enough responses agree, the node's "confidence" in that answer increases
- Once confidence reaches a threshold, the node considers the answer final

**Why we chose it:**
- Works with small networks (3-25 nodes) without requiring all nodes to be online
- Tolerant to slow or temporarily offline nodes
- Simple to implement correctly
- No complex BFT machinery or certificate chains

**Trade-off:** It's probabilistic (not instant finality). At the current configuration, finality takes 2-4 rounds (1-2 seconds at 500ms consensus interval).

---

## How Round-Robin Leader Election Works

```
Validators sorted by address bytes (deterministic):
  [addr(1), addr(2), addr(3), addr(4), addr(5)]

Height 0 → leader = validators[0 % 5] = addr(1)
Height 1 → leader = validators[1 % 5] = addr(2)
Height 2 → leader = validators[2 % 5] = addr(3)
Height 3 → leader = validators[3 % 5] = addr(4)
Height 4 → leader = validators[4 % 5] = addr(5)
Height 5 → leader = validators[5 % 5] = addr(1)  ← back to start
```

The validator list is sorted by their 32-byte address in ascending order. This makes the assignment deterministic regardless of what order the validators appear in any individual node's data.

**Fairness guarantee:** With N validators, each one is the leader for exactly 1/N of all blocks. No validator can dominate block production.

---

## How View Change Handles Offline Leaders

When the expected leader doesn't produce a block within 6 seconds, the network advances to the next leader via "view change":

```
Timeline for Height H:

  T+0s:  Expected leader is addr(2). Waiting for block.
  T+6s:  No block seen. View change: now addr(3) can produce.
  T+12s: Still no block. View change: now addr(4) can produce.
  T+18s: Still no block. View change: now addr(1) can produce.
         (wraps around)
```

A 3-second clock skew tolerance is applied: if your clock is slightly off, you'll still accept a block from a validator who is "one view ahead" according to your clock.

This means:
- An offline validator costs at most 6 seconds of block delay
- The next validator picks up immediately after
- The offline validator's rewards are simply not collected (no slashing for downtime)

---

## How Direct Request-Response Voting Works

Unlike gossipsub (broadcast), consensus uses direct peer-to-peer request-response:

```
         ┌─────────────┐
         │   Leader    │  (produces block, sends proposals directly)
         └──────┬──────┘
                │ BlockProposal (to each validator)
         ┌──────┼──────┐
         ▼      ▼      ▼
      [Val1]  [Val2]  [Val3]
         │      │      │
         └──────┴──────┘
                │ Vote (each validator replies directly to leader)
         ┌──────┴──────┐
         │   Leader    │  (collects votes, finalizes if >= threshold)
         └─────────────┘
```

**Why direct request-response instead of gossipsub:**
- Gossipsub fans out to ALL nodes — with 25 validators, a proposal would hit all 25 even though only validators need to vote
- Direct sends only go to validators — much more efficient
- Gossipsub has rate limiting that can throttle consensus; request-response has no artificial limits
- Provides per-peer accountability (we know exactly who voted and who didn't)

---

## What Happens During Sync

When a new node joins (or a node falls behind), it enters `Syncing` state:

```
New Node                    Existing Nodes
    │                            │
    │─── GetHeight ─────────────▶│  (ask 3 peers their height)
    │◀── Height(229) ────────────│  (collect responses, take median)
    │                            │
    │─── GetBlocks(1..10) ──────▶│  (batch of 10 blocks)
    │◀── Blocks([b1..b10]) ──────│
    │    apply b1..b10           │
    │─── GetBlocks(11..20) ─────▶│  (next batch)
    │◀── Blocks([b11..b20]) ─────│
    │    apply b11..b20          │
    │    ...                     │
    │─── GetHeight ─────────────▶│  (verify: am I caught up?)
    │◀── Height(229) ────────────│
    │    our_height=229 >= 229   │
    │    → SYNC COMPLETE         │
    │    → state: Syncing→Active │
```

While syncing, the node:
- **Does not** produce blocks
- **Does not** vote on consensus proposals
- **Does** respond to sync requests from other peers (it may have blocks they need)
- **Does** subscribe to gossipsub (silent listener — learns about new blocks)

---

## ASCII Art: Full Message Flow (3 nodes, 1 block)

```
Time →

addr(1) is leader at height H

[addr(1)/Leader]          [addr(2)/Validator]       [addr(3)/Validator]
       │                         │                         │
       │ produce block H         │                         │
       │                         │                         │
       │──── BlockProposal ─────▶│                         │
       │──── BlockProposal ───────────────────────────────▶│
       │                         │                         │
       │                    validate                  validate
       │                         │                         │
       │◀─── Vote(accept=true) ──│                         │
       │◀─── Vote(accept=true) ──────────────────────────────
       │                         │                         │
       │ count votes: 2/2 accept │                         │
       │ → finalize block H      │                         │
       │                         │                         │
       │──── BlockAnnounce ─────▶│ (gossipsub)             │
       │                    apply block H                   │
       │                         │──── BlockAnnounce ─────▶│
       │                         │                    apply block H
       │                         │                         │
   [height=H+1]              [height=H+1]             [height=H+1]

Next round: addr(2) is leader at height H+1
```
