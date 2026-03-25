# Commputer Development Resume Document

**Last updated:** 2026-03-26 (post-merge)
**Tests:** 184 passing, 0 failing
**Commits:** 90 on main
**Lines of Rust:** ~20,000
**Completion estimate:** ~50-60% to public testnet

## What This Is

Commputer ($COMME) is a Layer 1 blockchain that builds a distributed supercomputer from regular people's idle desktop resources. Anonymous founder. Open source. Anti-scale by design (warehouses get punished, single desktops get rewarded).

Key documents:
- Whitepaper: `protocol/whitepaper/WHITEPAPER.md`
- Launch spec: `docs/specs/2026-03-24-launch-scope-design.md`
- Design spec: `docs/specs/2026-03-22-commputer-design.md`
- Research: `docs/research/` (consensus, Sybil resistance, Rust libs, market psychology, compute marketplace)
- Architecture: `ARCHITECTURE.md`
- Protocol spec: `docs/PROTOCOL.md`
- Tokenomics: `docs/TOKENOMICS.md`
- API docs: `docs/API.md`
- Anti-scale docs: `docs/ANTI_SCALE.md`
- Security: `docs/SECURITY.md`
- Node operator guide: `docs/NODE_OPERATOR.md`
- Contributing: `CONTRIBUTING.md`

## The Vision (Short Version)

The founder is anonymous, disabled, in medical debt. Building this as a lifetime project to create communally owned compute/AI. Not for profit — for stability and to help people. The blockchain is just the mechanism for egalitarian distribution. "So long as one person holds 1 $COMME, I will be working toward the full vision."

## Launch Scope

**What ships at launch: JUST the L1. No analytics platform, no desktop app.**
- Rust CLI node with RPC server
- $COMME token with 2B fixed supply
- Multi-dimensional Proof of Work (5 channels: CPU, GPU, Storage, RAM, Bandwidth)
- Demand-weighted emission with channel floors
- Dual burn mechanics (milestone + burst compute)
- Anti-scale enforcement (reference node ceiling, compliance/nerf, behavioral analysis)
- P2P networking (gossip + DHT + sync protocol)
- Wallet with seed phrase recovery

**Everything else is roadmap** (analytics platform, desktop app, email/communication, storage tiers, AI access, $RAD, Humanities Archive, on-chain charitable voting).

## Crate Architecture

All source code is in `src/` (Cargo workspace with 8 crates).

| Crate | Path | Status | What It Does |
|-------|------|--------|-------------|
| `commputer-core` | `src/core/` | Solid | Block (with merkle roots, producer signing), Transaction (with public_key, verify(), fees, nonce), Proof types, Wallet (Ed25519 + BIP39), Keystore (AES-256-GCM + Argon2), Signing, Token (2B supply), Tiers, Compliance/NerfRate with exponential decay, Identity with hardware fingerprinting, Merkle tree, Test fixtures, Fuzz tests |
| `commputer-consensus` | `src/consensus/` | Solid | SnowballVoter, Multi-channel DAG, Epoch management with validator set rotation, EmissionSchedule (hybrid curve), ChannelAllocation (demand-weighted with floors + difficulty adjustment), AnchorSelector (VRF-weighted), CRS formula R^0.7 with diversity bonus, Finality depth, Fork choice rule, Checkpoint blocks |
| `commputer-storage` | `src/storage/` | Solid | Account model with history index, BlockStore with pruning + RocksDB fallback, ChainState with full block validation (parent hash, merkle roots, signatures, nonces, fees, timestamps, block size), RocksDB persistence, State snapshots every 100 blocks, Transaction receipts, Supply tracking with saturating arithmetic, Tier change logging, Grace period tracking, Write-ahead log |
| `commputer-network` | `src/network/` | Working | libp2p transport (gossipsub + kademlia + identify), 4 gossipsub topics, Seed nodes, GossipRouter, PeerStore with RTT tracking, Message compression (zstd), Idle connection timeout, Peer exchange |
| `commputer-proofs` | `src/proofs/` | Working | All 5 channels with real implementations: CPU (iterative SHA-256), GPU (detection + matrix multiply), Storage (real chunk retrievability), RAM (dynamic buffer sizing + timing), Bandwidth (timed transfer + scoring). Cross-node verification, difficulty scaling, timeout handling, challenge randomness |
| `commputer-validator` | `src/validator/` | Working | ValidatorState machine, ComplianceChecker (IP + subnet + fingerprint + behavioral analysis + geographic diversity), Exponential decay for multi-node, Adaptive nerf sliding scale, Resource spike detection, Datacenter IP detection, Compliance history |
| `commputer` (node) | `src/node/` | Working | CLI binary (run/wallet/status/send/peers/balance/version/export-chain/verify-chain), EventLoop (swarm + epoch + block production + consensus + proofs + compliance + RPC + sync + graceful shutdown), ConsensusManager (Snowball + reorg + finality + slashing), ProofManager (challenge lifecycle + cross-verification), RPC server (axum: /tx /status /peers /block /mempool /health /metrics /receipts /proofs /compliance), Hardware detection, Rate limiting, Bad peer banning, Peer reputation scoring |
| `commputer-sim` | `src/sim/` | Working | Standalone economic simulator: N validators over M epochs, emission simulation, burn simulation, supply curve output, warehouse attack simulation, network growth simulation, tier accessibility, grace period simulation |

## What Works End-to-End

1. **Node boots** → detects hardware → creates/loads wallet → connects to network → handshakes → syncs → registers as validator
2. **Block production** → every 2s, computes merkle roots, signs header, broadcasts as candidate
3. **Block validation** → parent hash, merkle roots, producer signature, tx signatures, nonces, fees, timestamps, block size limits
4. **Consensus** → Snowball voting, fork resolution, finality depth, checkpoint blocks, slashing for equivocation
5. **Epoch tick** → finalize proofs → distribute rewards (CRS R^0.7) → apply compliance nerf → adjust difficulties → rotate validator set → persist → snapshot
6. **Proof challenges** → all 5 channels with real implementations → cross-node verification → timeout handling → difficulty scaling
7. **Anti-scale** → IP/subnet detection, hardware fingerprinting, behavioral analysis, geographic diversity, exponential decay, adaptive nerf, resource spike detection, datacenter detection
8. **Transactions** → full signature verification, nonce tracking, fee validation (burned), double-spend prevention
9. **RPC server** → axum HTTP server with 10+ endpoints: tx submission, chain status, block explorer, mempool, health, metrics, proof status, compliance dashboard
10. **CLI** → wallet create/recover/show/export, chain status, send (broadcasts via RPC), peers, balance, version, export-chain, verify-chain
11. **Persistence** → RocksDB with block pruning, state snapshots, transaction receipts, account history index
12. **Networking** → libp2p with peer reputation, rate limiting, ban list, connection limits, sync protocol, block request protocol, message compression, graceful shutdown
13. **Economic simulator** → standalone binary modeling validators, emission, burns, supply curves, warehouse attacks, network growth

## What's Next (Priority Order)

### 1. Multi-Machine Testing (HIGHEST PRIORITY)
Founder has machines available. Run nodes on different physical machines. Surface: NAT/firewall issues, consensus under real latency, state divergence.

### 2. overnight-experiment-2 Integration
A second overnight agent is currently running with 200 more tasks (mainnet readiness, security hardening, performance, testnet infrastructure, wallet UX, L2 interface, protocol economics, future-proofing). Review and merge when complete.

### 3. Testnet Launch Preparation
- Testnet genesis with fast epochs
- Faucet for testnet COMME
- Block explorer web UI
- Monitoring dashboard
- Deployment scripts

### 4. Security Audit
- Review all overnight agent code for vulnerabilities
- Fuzz testing
- Integer overflow audit
- Signature malleability checks

### 5. Desktop App (Tauri)
After testnet is stable, wrap the node in a GUI: resource slider, wallet, analytics button.

## Known Issues

- `two_nodes_gossip_block` integration test is flaky (timing-dependent, passes on retry)
- Some compiler warnings: unused fields (hardware fingerprint, finalize_epoch)
- Proof scores still partially self-reported — cross-node verification exists but needs real network testing
- Sync protocol untested on real network
- No NAT traversal yet

## Overnight Agent Pattern

We run autonomous overnight agents on git branches:
- Branch from main or latest experiment
- Launch in tmux with `--dangerously-skip-permissions`
- Give massive task list (100-200 items)
- Agent commits frequently, runs tests
- Next day: review, merge good work into main, discard bad
- Current: `overnight-experiment-2` branch is active with 200-item task list

## Build & Run

```bash
# Build
cd ~/Coin/src && source ~/.cargo/env && cargo build --workspace

# Test (184 tests)
cd ~/Coin/src && cargo test --workspace

# Run node
cd ~/Coin/src && cargo run -p commputer -- run --testnet --port 9000 --rpc-port 9944

# Wallet
cd ~/Coin/src && cargo run -p commputer -- wallet create
cd ~/Coin/src && cargo run -p commputer -- wallet show --testnet
cd ~/Coin/src && cargo run -p commputer -- status --testnet

# Send (broadcasts to running node via RPC)
cd ~/Coin/src && cargo run -p commputer -- send <address_hex> <amount> --rpc-port 9944

# Economic simulator
cd ~/Coin/src && cargo run -p commputer-sim -- --validators 10000 --epochs 1000
```

## Key Design Decisions (Don't Change Without Discussion)

1. **2B supply** — fixed, only goes down via burns
2. **51/49 split** — 51% compute to flagship product, 49% to holders. Protocol-enforced.
3. **Anti-scale** — one desktop at 100% is ceiling. Exponential decay on multi-node. Nerf starts at 80%, can only increase, targets 100%.
4. **Demand-weighted emission** — floors per channel (10% CPU/GPU/Storage, 5% RAM/Bandwidth), surplus by demand
5. **Grace period** — drains 1:1 offline, refills 2:1 online, capped at 10 years
6. **No founder allocation** — zero premine, zero dev tax. Founder earns through L2 products.
7. **Charitable burns** — annual, restricted categories. NEVER: war, politics, profit ventures.
8. **Emergency access** — below 1M circulating COMME, any contribution = full access
9. **Reference node pegged to gold** — 0.3225 troy oz / 10.03g of gold in 2026, median currency
10. **Storage will function** — contacts notified on extended absence, free download on death
11. **120-year inactive wallets** considered nonexistent
12. **Transaction fees are burned** — not paid to validators
13. **CRS formula** — R^0.7 per channel with diversity bonus

## Git Identity (Local Only)

Name: The Commrade
Email: commrade@commputer.xyz

## Founder Context

- Anonymous, disabled, in medical debt
- Spending personal money on AI assistance
- Not motivated by wealth — wants stability, healthcare, and to help others
- "I want to do one good thing for humanity"
- Has machines available for multi-node testing
- Has contacts ready for word-of-mouth launch
