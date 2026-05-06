# ADR-0001: Multi-Channel Proof of Work (CPU / GPU / Storage / RAM / Bandwidth)

## Status

Accepted. Active since genesis.

## Context

Every prior PoW chain validates a single resource: Bitcoin hashes SHA-256
on whatever silicon is cheapest, Chia plots disk, Filecoin proves storage,
Monero is CPU-leaning but still single-axis. The economic equilibrium of any
single-axis PoW is a hardware monoculture: the cheapest joule-per-unit-work
machine wins, everyone else is priced out, and the network ends up running
on ASICs in industrial parks.

Commputer's thesis (whitepaper §1, §2, §6) is the inverse: the economics
should favor a regular person running one well-rounded desktop at home. A
"well-rounded desktop" is not a single number — it is the *combination* of
CPU, GPU, storage, RAM, and an internet connection. We needed PoW that
matches that shape.

## Decision

Validate proof-of-work across five parallel resource channels, with each
channel verified independently and rewards split across all five:

```rust
// src/core/src/proof.rs:8-19
pub enum ResourceChannel {
    Processing,  // CPU
    Gpu,
    Storage,
    Ram,
    Bandwidth,
}
```

Each channel has a dedicated prover/verifier (`src/proofs/src/cpu.rs`,
`gpu.rs`, `storage_proof.rs`, `ram.rs`, `bandwidth.rs`). Emission per epoch
is split across channels with protocol-enforced floors (10/10/10/5/5%) and
a demand-weighted float for the remaining 60% (whitepaper §7, "Demand-
Weighted Allocation"). A diversity bonus rewards validators that contribute
across all five channels (`src/proofs/src/cross_channel.rs`).

## Consequences

### Positive

- Specialized hardware (GPU farms, plotter rigs) earns less per dollar than
  a balanced machine, because the dollars sunk into the strong axis are
  wasted on the weak ones.
- The network self-balances composition: if storage is scarce, the
  demand-weighted emission shifts toward storage proofs without any
  governance vote.
- The network can actually *use* what validators contribute — every channel
  maps to a real product on the roadmap (storage, AI compute, communication
  bandwidth). The PoW is not waste heat.

### Negative

- Five provers and five verifiers is roughly 5× the implementation surface
  of a single-channel chain. Each channel is a separate attack surface.
- Verification cost compounds: an epoch tick verifies every (validator ×
  channel) pair, which is what made the event-loop blocking work in
  ADR-0005 necessary.
- Calibrating the relative weights between channels is a permanent
  governance problem. We picked floors at genesis and let demand float the
  rest, but the floors themselves are a value judgement.

### Known Limitations

- A sufficiently sophisticated adversary can still build a balanced rig
  cheaply by sourcing used parts, defeating the diversity-bonus economic
  signal at the margin.
- Some channels (bandwidth) are inherently harder to verify than others
  (CPU). A motivated cheater will find the weakest channel first.

## Alternatives Considered

- **Single-channel CPU PoW (Monero-style).** Rejected: still converges to
  whoever has the cheapest electricity. Doesn't reward owning a real desktop.
- **Single-channel storage PoW (Filecoin/Chia).** Rejected: collapses the
  network to a plotter farm; the resulting machines are useless for the
  products on the roadmap (AI, communication).
- **Stake-only (PoS).** Rejected on principle (whitepaper §2, §11): the free
  path must never close. Stake-gated participation contradicts the project's
  reason for existing.

## References

- Whitepaper §4 "Multi-Dimensional Proof of Work", §6 "Scale Hurts",
  §7 "Demand-Weighted Allocation"
- `src/core/src/proof.rs:5-41`
- `src/proofs/src/lib.rs:1-47`
- `src/proofs/src/cross_channel.rs` (diversity bonus)
- Related: ADR-0003 (anti-scale), ADR-0005 (verification offloading)
