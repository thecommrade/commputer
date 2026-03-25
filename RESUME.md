# Commputer Development Resume Document

**Last updated:** 2026-03-25
**Last session test count:** 113 passing, 0 failing
**Commits:** 29
**Lines of Rust:** ~7,000

## What This Is

Commputer ($COMME) is a Layer 1 blockchain that builds a distributed supercomputer from regular people's idle desktop resources. Anonymous founder. Open source. Anti-scale by design (warehouses get punished, single desktops get rewarded).

Key documents:
- Whitepaper: `protocol/whitepaper/WHITEPAPER.md`
- Launch spec: `docs/specs/2026-03-24-launch-scope-design.md`
- Design spec: `docs/specs/2026-03-22-commputer-design.md`
- Research: `docs/research/` (consensus, Sybil resistance, Rust libs, market psychology)

## Crate Architecture

All source code is in `src/` (Cargo workspace).

| Crate | Path | Status | What It Does |
|-------|------|--------|-------------|
| `commputer-core` | `src/core/` | Solid | Block, Transaction, Proof types, Wallet (Ed25519 + BIP39 seed phrase), Keystore (AES-256-GCM + Argon2), Signing, Token (2B supply), Tiers (1/10/20/33), Compliance/NerfRate, Identity, Error |
| `commputer-consensus` | `src/consensus/` | Solid | SnowballVoter (parameterized), Multi-channel DAG, Epoch management (1hr), EmissionSchedule (hybrid curve 0.09→0.01), ChannelAllocation (demand-weighted with floors), AnchorSelector (VRF-weighted) |
| `commputer-storage` | `src/storage/` | Solid | Account model (balance, tiers, grace period, compliance), BlockStore, ChainState (apply_block, apply_block_validated), RocksDB persistence (open/flush), Supply tracking (emitted/burned/circulating) |
| `commputer-network` | `src/network/` | Working | libp2p transport (gossipsub + kademlia + identify), 4 gossipsub topics (blocks/txs/consensus/proofs), Seed node infrastructure, GossipRouter, PeerStore with RTT tracking |
| `commputer-proofs` | `src/proofs/` | Working | All 5 channels: CPU (iterative SHA-256), GPU (matrix multiply), Storage (chunk retrievability), RAM (memory-hard), Bandwidth (timed hash). ChallengeGenerator, ProofVerifier |
| `commputer-validator` | `src/validator/` | Working | ValidatorState machine (Idle→Active→Idle), ComplianceChecker (same-IP and /24 subnet detection) |
| `commputer` (node) | `src/node/` | Working | CLI binary with subcommands (run/wallet/status/send), EventLoop (swarm + epoch + block production + consensus + proofs), ConsensusManager (Snowball over network), ProofManager (challenge lifecycle) |

## What Works End-to-End

1. **Node boots** → creates/loads wallet → connects to network → registers as validator
2. **Block production** → every 2s if active validator, broadcasts as candidate
3. **Consensus** → Snowball voting over gossipsub, single-candidate fast-path finalization
4. **Epoch tick** (hourly) → finalize proof results → distribute mining rewards proportional to composite score → apply compliance nerf → persist to RocksDB
5. **Proof challenges** (every 5min) → issue challenges across all 5 channels → solve → verify → feed scores into epoch
6. **Transaction validation** → mempool rejects unsigned/null txs, apply_block_validated checks signatures on network blocks
7. **CLI** → wallet create/recover/show/export, chain status, send (offline-only for now)
8. **Persistence** → RocksDB, chain survives restarts
9. **Two-node test** → integration test proves gossipsub block propagation works

## What's Next (Priority Order)

### 1. Multi-Machine Testing (HIGHEST PRIORITY)
The two-node integration test runs on localhost. Need to test on real machines across a network. The founder has machines available. This will surface:
- Connection issues (NAT, firewalls)
- Consensus divergence under real latency
- Proof timing issues across different hardware
- RocksDB state divergence

### 2. Full Block Validation
Currently only checks signature length (64 bytes). Needs:
- Public key registry (map Address → VerifyingKey on-chain)
- Full ed25519 signature verification on every tx
- Block producer validation (is this node actually the anchor?)
- Merkle root computation and verification for tx_root and proof_root

### 3. Wire Compliance Into Live Node
ComplianceChecker exists and is tested but not connected to real peer IPs. The event loop needs to:
- Track peer IP addresses from libp2p connection events
- Feed IPs into ComplianceChecker when validators register
- Apply compliance status to reward distribution (already done in epoch tick)

### 4. Transaction Broadcasting from CLI
The `send` command creates and signs a transaction but doesn't broadcast it. Need to either:
- Start a minimal network connection just to submit the tx
- Or add an RPC endpoint the CLI can talk to on a running node

### 5. Fork Resolution
What happens when two validators produce blocks at the same height? Snowball should handle this, but it hasn't been tested with real competing blocks.

### 6. Error Recovery
- What if a node crashes mid-epoch?
- What if a peer sends garbage?
- What if the network partitions?

## Known Issues / Warnings

- `candidates_at_height` in consensus_manager.rs has a dead_code warning (only used in tests)
- `compliance` field on EventLoop has an unused warning (wired for detection but IP feeding not connected yet)
- The `send` CLI command creates transactions offline — needs network broadcast
- Proof scores are currently self-reported (node challenges itself) — needs cross-node validation
- No peer banning for bad behavior yet

## Build & Run

```bash
# Build
cd ~/Coin/src && source ~/.cargo/env && cargo build --workspace

# Test
cd ~/Coin/src && cargo test --workspace

# Run node
cd ~/Coin/src && cargo run -p commputer -- run --testnet --port 9000

# Wallet
cd ~/Coin/src && cargo run -p commputer -- wallet create
cd ~/Coin/src && cargo run -p commputer -- wallet show --testnet
cd ~/Coin/src && cargo run -p commputer -- status --testnet
```

## Key Design Decisions (Don't Change Without Discussion)

1. **2B supply** — fixed, only goes down via burns
2. **51/49 split** — 51% compute to flagship product, 49% to holders. Protocol-enforced.
3. **Anti-scale** — one desktop at 100% is the ceiling. Exponential decay on multi-node. Nerf starts at 80%, can only increase.
4. **Demand-weighted emission** — floors per channel (10% CPU/GPU/Storage, 5% RAM/Bandwidth), surplus distributed by demand
5. **Grace period** — drains 1:1 offline, refills 2:1 online, capped at 10 years
6. **No founder allocation** — zero premine, zero dev tax. Founder earns through L2 products.
7. **Charitable burns** — annual, restricted categories, never: war, politics, profit ventures
8. **Emergency access** — below 1M circulating COMME, any contribution = full access

## Git Identity (Local Only)

Name: The Commrade
Email: commrade@commputer.xyz
