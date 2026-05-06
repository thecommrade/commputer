# ADR-0002: Snowball Consensus (Avalanche Family)

## Status

Accepted. Production parameters `k=20, α=14, β=20`; testing parameters
`k=3, α=2, β=5`.

## Context

The chain needs Byzantine-fault-tolerant agreement on which block to accept
at each height. The two obvious families were Nakamoto-style longest-chain
(Bitcoin / Ethereum-pre-merge) and BFT (PBFT, Tendermint, HotStuff).

Nakamoto needs probabilistic finality measured in many block-times and is
energy-bound to the underlying PoW; with multi-channel PoW (ADR-0001) the
"longest chain" calculation across five resource axes is ill-defined.
Classical BFT needs a known validator set, all-to-all messaging, and gives
deterministic finality at O(n²) communication — fine at 25 validators, ugly
at 10,000 (whitepaper §7 cites 10,000-validator scaling targets).

Snowball (Avalanche family) sits between the two: each validator repeatedly
samples a *small random subset* of peers, adopts the majority preference if
a quorum agrees, and commits after the same preference holds for β
consecutive rounds. Communication is O(k) per node per round regardless of
network size, and finality is sub-second on healthy networks.

## Decision

Use Snowball as the per-height block-acceptance protocol, parameterized by
three integers `(k, α, β)`:

- `k` = sample size (peers polled per round)
- `α` = quorum threshold (sampled peers that must agree)
- `β` = decision threshold (consecutive successful rounds before commit)

Two parameter profiles ship in the codebase:

```rust
// src/consensus/src/config.rs:51-78
production() -> { sample_size: 20, quorum: 14,  decision_threshold: 20 }
testing()    -> { sample_size:  3, quorum:  2,  decision_threshold:  5 }
```

The runtime `ConsensusManager` defaults to the testing profile
(`src/node/src/consensus_manager.rs:179-183`) and scales `k` upward as the
peer set grows (`update_params_for_network_size`).

Validity invariant: `α > k/2` (enforced at
`src/consensus/src/config.rs:104-110`). Less than that and a single
adversarial response can flip the network's preference round to round —
liveness collapses.

## Consequences

### Positive

- O(k) per-round bandwidth: a 10,000-validator network polls the same 20
  peers as a 50-validator network. Scales horizontally without redesign.
- Sub-second median finality on healthy networks; β=20 rounds at ~50ms
  per round = ~1s, well below block time.
- No leader cliff. A wedged or offline leader does not stall the chain
  (combined with view-change in `event_loop.rs`).

### Negative

- Snowball is probabilistic, not deterministic. There is a vanishingly
  small probability (≈ (1-α/k)^β) of a wrong commitment per round; β=20
  pushes that below realistic thresholds but it is not zero.
- Sampling assumes a roughly honest peer set. If >α/k of peers are
  Byzantine, safety degrades. Real-world Avalanche papers assume <20%
  byzantine; we inherit that assumption.
- Two parameter profiles is a footgun. The testing profile (k=3, α=2)
  is *fine* for smoke harnesses but unsafe at scale: a 3-peer poll
  with α=2 is one corrupted response away from indecision. The runtime
  default is currently the testing profile pending wire-up of network-size
  scaling and genesis configuration.

### Known Limitations

- We have not run an adversarial-network simulation to empirically validate
  β=20 at production size. The values are taken from the Avalanche
  whitepaper's recommended ranges, not from our own measurements.
- Eclipse attacks (an attacker controlling all of one node's sampled peers)
  defeat Snowball entirely. Mitigated separately by
  `eclipse_detector_tests.rs` and seed diversity, not by the consensus
  algorithm itself.

## Alternatives Considered

- **Nakamoto longest-chain.** Rejected: probabilistic finality is too slow
  (~1 hour for Bitcoin-grade confidence) and "longest chain" across five
  PoW channels has no canonical definition.
- **Tendermint / HotStuff BFT.** Rejected: O(n²) messaging makes the
  10,000-validator target prohibitive; deterministic finality is a nice
  property but not worth that scaling cliff.
- **Plain Avalanche (DAG-form).** The DAG variant is more bandwidth-
  efficient but harder to reason about under partition; Snowball on a
  linear chain is simpler to validate and audit.

## References

- `src/consensus/src/snowball.rs` (algorithm)
- `src/consensus/src/config.rs:49-122` (parameter profiles + validation)
- `src/node/src/consensus_manager.rs:174-220` (runtime wiring, network-size
  scaling)
- Avalanche whitepaper: Rocket et al., "Scalable and Probabilistic
  Leaderless BFT Consensus through Metastability" (2019)
- Related: ADR-0004 (bootstrap leader handles the n=1 degenerate case)
