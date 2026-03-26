# Snowstorm Consensus Protocol (Feature 136)

## Overview

Commputer uses **Snowstorm**, an Avalanche-inspired consensus protocol adapted for
proof-of-useful-work. Unlike traditional Avalanche which operates over stake-weighted
validators, Snowstorm weights validators by their **Composite Resource Score (CRS)**,
which measures actual computational contributions across five resource channels.

## Protocol Family

Snowstorm belongs to the Snow* protocol family:
- **Slush**: Single-round probabilistic sampling
- **Snowflake**: Multi-round with consecutive counter
- **Snowball**: Multi-round with confidence counters (what we use)
- **Snowstorm**: Snowball adapted for DAG-structured blocks with CRS weighting

## Parameters

### Snowball Core Parameters

| Parameter | Symbol | Default | Range | Description |
|-----------|--------|---------|-------|-------------|
| Sample size | k | 20 | 3-50 | Number of peers polled each round |
| Quorum threshold | alpha | 14 | k/2+1 to k | Minimum agreeing peers |
| Decision threshold | beta | 20 | 3-100 | Consecutive rounds to finalize |

### Operational Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| Consensus timeout | 30s | Force-finalize after this duration |
| View change timeout | 10s | Replace offline producer |
| Block interval | 2s | Minimum time between blocks per validator |
| Finality depth | 100 blocks | No reorg past this depth |
| Checkpoint interval | 1000 blocks | State root commitment frequency |
| Max timestamp drift | 15s | Reject blocks with excessive timestamp skew |

### Parameter Selection Rationale

**k=20**: Provides 99.99% agreement probability with alpha=14. Larger k increases
network overhead linearly but improves security exponentially.

**alpha=14 (70% of k)**: The quorum must be strictly greater than k/2 for liveness.
70% provides strong consistency while tolerating up to 30% Byzantine nodes in a sample.

**beta=20**: Each round takes approximately 500ms (network RTT). beta=20 means
finalization in approximately 10 seconds under normal conditions. This balances
speed with security against adaptive adversaries.

## Protocol Flow

### 1. Block Production

Block production rights are determined by CRS-weighted anchor selection:
1. Compute `round_seed = Hash(previous_block_hash || round_number)`
2. For each validator: `ticket = Hash(round_seed || validator_address)`
3. Weight each ticket by `ticket_value / composite_resource_score`
4. Lowest weighted ticket wins the right to produce

This ensures validators contributing more compute resources produce more blocks,
creating a natural incentive alignment.

### 2. Candidate Propagation

Candidate blocks are broadcast via gossipsub on the consensus topic.
Each candidate includes the full block with header, transactions, and proof summaries.

### 3. Snowball Voting

For each height with multiple candidates:
1. Node selects k random peers
2. Sends `SnowballQuery { height, preference }`
3. Receives `SnowballResponse { height, preference }` from each
4. Tallies responses per block hash
5. If any hash reaches alpha votes: increment confidence, track consecutive rounds
6. If current preference holds for beta consecutive rounds: finalize
7. If no hash reaches alpha: reset consecutive counter

### 4. Finalization

A block is finalized when:
- Single candidate: immediately (no contention)
- Multiple candidates: after beta consecutive rounds of quorum agreement
- Timeout: after 30s, force-finalize on current preference (liveness guarantee)

### 5. Finality Gadget

After Snowball finalization, the finality gadget provides additional safety:
- Validators submit finality votes with their CRS weight
- When 2/3+ of total validator weight confirms a block, it is final
- No reorg is permitted past a final block
- This provides economic finality on top of probabilistic Snowball finality

## Attack Resistance

### 51% Attack

In traditional PoW, controlling 51% of hash power enables chain rewriting.
In Snowstorm:
- An attacker needs to control alpha/k (70%) of randomly sampled peers
- With k=20, this requires controlling 14+ nodes in random samples
- The probability of success drops exponentially with beta
- After finality (2/3+ weight), rewriting is impossible

### Equivocation Attack

**Attack**: Validator signs two different blocks at the same height.

**Defense**:
- ConsensusManager tracks (validator, height) -> BlockHash
- If a second different hash is seen, the validator is immediately slashed
- Slashed validators earn zero epoch rewards
- Equivocation evidence is permanently recorded

### Long-Range Attack

**Attack**: Attacker creates an alternate chain history from a point far in the past.

**Defense**:
- Blocks targeting heights deeper than `finality_depth` from the tip are rejected
- Checkpoint commitments every 1000 blocks anchor the state
- Finalized blocks (2/3+ weight) cannot be reverted
- See also: docs/nothing-at-stake.md

### Time Warp Attack

**Attack**: Manipulate timestamps to artificially speed up or slow down the chain.

**Defense**:
- Block timestamps are validated against the median of recent blocks
- Maximum allowed drift is 15 seconds from the network median
- Blocks with timestamps too far in the future or past are rejected
- The block interval minimum (2s per validator) prevents timestamp rushing

### Sybil Attack

**Attack**: Create many fake validators to gain disproportionate voting weight.

**Defense**:
- Validators must pass hardware verification (CPU, GPU, RAM, storage, bandwidth)
- Each resource channel requires actual computational work (proof-of-useful-work)
- Creating fake validators means actually provisioning real hardware
- Diversity bonus caps rewards for identical hardware configurations

### Nothing-at-Stake

See docs/nothing-at-stake.md for detailed analysis.

### Network Partition

**Defense**:
- Partition detection: if peer count drops below MINIMUM_PEERS, block production pauses
- When partition heals, nodes sync and Snowball voting resumes
- Checkpoints prevent divergent chains from growing too far apart
- Consensus timeout forces progress even with degraded connectivity

## Comparison to Other Protocols

| Property | Snowstorm | Tendermint | Nakamoto PoW | Avalanche |
|----------|-----------|------------|--------------|-----------|
| Finality | Probabilistic + Gadget | Deterministic | Probabilistic | Probabilistic |
| Throughput | ~500 tx/block | ~100 tx/block | ~2000 tx/block | ~500 tx/block |
| Latency | ~10s | ~6s | ~600s | ~2s |
| Weight basis | CRS (compute) | Stake | Hash power | Stake |
| Sybil resistance | Hardware proofs | Capital | Energy | Capital |

## Configuration

Consensus parameters can be configured via `ConsensusConfig`:

```rust
use commputer_consensus::config::ConsensusConfig;

// Production defaults
let config = ConsensusConfig::production();

// Testing (faster finality)
let config = ConsensusConfig::testing();

// Custom
let config = ConsensusConfig {
    sample_size: 20,
    quorum: 14,
    decision_threshold: 20,
    min_block_interval_secs: 2,
    consensus_timeout_secs: 30,
    view_change_timeout_secs: 10,
    finality_depth: 100,
    max_timestamp_drift_secs: 15,
    checkpoint_interval: 1000,
};
assert!(config.validate().is_ok());
```
