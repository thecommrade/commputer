# ADR-0004: Bootstrap-Leader Topology (Node 1 Has No `--seeds`)

## Status

Accepted. Enforced at `src/node/src/event_loop.rs:2632-2673` and assumed
by every smoke / stress harness in `scripts/`.

## Context

When the chain starts from genesis, there are zero blocks. Every node has
the same `genesis.json` but no `tip`. Block production requires a leader —
under Snowball with view-change, leader election is round-robin over the
known on-chain validator set, and that set is bootstrapped *by* the first
few blocks.

This is a chicken-and-egg problem. With N nodes coming up simultaneously,
all of them holding the same genesis and all of them eligible to produce,
the network has two failure modes:

1. **Race**: every node produces block 1 from genesis simultaneously,
   leading to N forks at height 1, and Snowball converges only after a
   bandwidth-burning round of vote exchange.
2. **Stall**: every node defers to "the leader" but no leader exists yet,
   and the chain never starts.

We need a mechanism to designate one node as "you go first" without
introducing on-chain trust roles, founder addresses, or genesis-allocated
authority.

## Decision

Use the *absence of `--seeds`* as the bootstrap-leader signal:

```rust
// src/node/src/event_loop.rs:2669-2673  (handle_block_tick)
} else if self.is_seed_connector {
    // Bootstrap phase (< 2 validators): only the seed node
    // (bootstrap leader) produces.
    // Nodes started with --seeds defer to the seed to prevent
    // competing blocks.
    return;
}
```

`is_seed_connector` is true iff the node was started with one or more
`--seeds` arguments. The node *without* seeds (the bootstrap leader) is
the only node permitted to produce blocks while the on-chain validator
set has fewer than 2 entries. Once the chain has registered ≥2 validators,
the gate disappears and normal round-robin leader election with view-change
takes over.

Operationally, every smoke / stress harness in `scripts/multinode_smoke.sh`
and `scripts/sync_recovery_smoke.sh` starts node 1 with no `--seeds` and
points all subsequent nodes at node 1.

## Consequences

### Positive

- Zero on-chain state. The bootstrap-leader role is purely a startup-flag
  decision; the chain itself has no concept of a privileged genesis node
  after height 2.
- Eliminates the height-1 fork race deterministically: there is exactly
  one node permitted to produce while validators < 2, so there is exactly
  one block 1.
- Trivial to operate: "the first node has no seeds, every other node lists
  the first node" is a one-line ops rule. Founder runs the bootstrap
  leader at launch; once others register, the role evaporates.

### Negative

- Single point of failure during the *first few seconds* of chain life.
  If the bootstrap leader crashes before block 2, no other node can
  produce, because all of them have `is_seed_connector = true`. They will
  spin until the bootstrap leader returns or an operator manually
  restarts one of them without `--seeds`.
- The mechanism is not enforced cryptographically — it is a peer policy.
  A malicious node can simply omit `--seeds` and try to compete with the
  legitimate bootstrap leader. At genesis with zero on-chain validators
  this would produce two competing height-1 blocks; Snowball would
  resolve, but the resolution is not free.

### Known Limitations / Failure Modes

- **Violation: two nodes start without `--seeds`.** Both attempt to
  produce block 1 from genesis. Snowball converges, but until it does
  every honest node sees two competing tips at height 1 and may flap.
  Mitigation: operational discipline + `chain_health_monitor` warnings.
- **Violation: every node starts with `--seeds`.** The chain never
  starts. Every node defers to "the seed" but the seed is also deferring.
  This was observed in early smoke runs and is the reason the harnesses
  now hardcode the asymmetry.
- **Violation: bootstrap leader is restarted *with* `--seeds` after
  block 1**. Now no one is the bootstrap leader, but ≥2 validators may
  not yet be registered, so block production stops until registration
  catches up. Recoverable but operationally surprising.

## Alternatives Considered

- **Genesis-allocated leader address.** Rejected: violates
  whitepaper §11 — the founder has zero special on-chain authority.
- **VRF-based leader for height 1.** Rejected: VRF requires a known
  validator set, which we don't have at height 1 because validators
  register via on-chain transactions in early blocks.
- **Multi-node consensus from genesis with fork resolution.** Rejected:
  works but burns 1-2 seconds and a few rounds of bandwidth on every
  cold start. The asymmetric topology gives the same outcome for free.
- **Configuration flag `--bootstrap-leader=true`.** Rejected as
  redundant: the absence of `--seeds` already perfectly identifies the
  bootstrap leader, and adding a separate flag invites operators to set
  both at once and confuse themselves.

## References

- `src/node/src/event_loop.rs:2632-2673` (the gate)
- `src/node/src/event_loop.rs:176-179` (`is_seed_connector` field)
- `scripts/multinode_smoke.sh:11-20, 155-185` (operational embodiment)
- `scripts/sync_recovery_smoke.sh:10` ("Node 1 is bootstrap leader")
- Commit `c926f1a` `feat(scripts): multi-node smoke harness with
  bootstrap-leader topology`
- Related: ADR-0002 (Snowball — handles the post-bootstrap consensus)
