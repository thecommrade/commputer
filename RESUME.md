# Commputer Development Resume Document

**Last updated:** 2026-03-25 (evening session)
**Last session test count:** 113 passing, 0 failing
**Commits:** 33
**Lines of Rust:** ~7,200

## What This Is

Commputer ($COMME) is a Layer 1 blockchain that builds a distributed supercomputer from regular people's idle desktop resources. Anonymous founder. Open source. Anti-scale by design (warehouses get punished, single desktops get rewarded).

Key documents:
- Whitepaper: `protocol/whitepaper/WHITEPAPER.md`
- Launch spec: `docs/specs/2026-03-24-launch-scope-design.md`
- Design spec: `docs/specs/2026-03-22-commputer-design.md`
- Research: `docs/research/` (consensus, Sybil resistance, Rust libs, market psychology, compute marketplace)
- Implementation plans: `docs/superpowers/plans/`
- Twitter drafts: `docs/community/twitter-snippets.md`

## The Vision (Short Version)

The founder is anonymous, disabled, in medical debt. Building this as a lifetime project to create communally owned compute/AI. Not for profit — for stability and to help people. The blockchain is just the mechanism for egalitarian distribution. "So long as one person holds 1 $COMME, I will be working toward the full vision."

## Launch Scope (Simplified — decided this session)

**What ships at launch: JUST the L1. No analytics platform, no desktop app.**
- Rust CLI node
- $COMME token with 2B fixed supply
- Multi-dimensional Proof of Work (5 channels: CPU, GPU, Storage, RAM, Bandwidth)
- Demand-weighted emission with channel floors
- Dual burn mechanics (milestone + burst compute)
- Anti-scale enforcement (reference node ceiling, compliance/nerf)
- P2P networking (gossip + DHT)
- Wallet with seed phrase recovery

**Everything else is roadmap:**
- Analytics platform (founder's L2)
- Desktop app (Tauri GUI)
- Email/communication, storage tiers, AI access
- $RAD reputation token
- Humanities Archive
- On-chain charitable voting

The L1's unique properties ARE the value proposition. No other chain has anti-scale PoW + multi-dimensional proofs + demand-weighted emission.

## Crate Architecture

All source code is in `src/` (Cargo workspace).

| Crate | Path | Status | What It Does |
|-------|------|--------|-------------|
| `commputer-core` | `src/core/` | Solid | Block (with merkle roots), Transaction (with public_key + verify()), Proof types, Wallet (Ed25519 + BIP39), Keystore (AES-256-GCM + Argon2), Signing, Token (2B supply), Tiers (1/10/20/33), Compliance/NerfRate, Identity, Error |
| `commputer-consensus` | `src/consensus/` | Solid | SnowballVoter (parameterized), Multi-channel DAG, Epoch management (1hr), EmissionSchedule (hybrid curve 0.09→0.01), ChannelAllocation (demand-weighted with floors), AnchorSelector (VRF-weighted) |
| `commputer-storage` | `src/storage/` | Solid | Account model, BlockStore, ChainState with full block validation (parent hash, merkle roots, ed25519 signatures), RocksDB persistence, Supply tracking |
| `commputer-network` | `src/network/` | Working | libp2p transport (gossipsub + kademlia + identify), 4 gossipsub topics (blocks/txs/consensus/proofs), Seed node infrastructure, GossipRouter, PeerStore with RTT tracking |
| `commputer-proofs` | `src/proofs/` | Working | All 5 channels: CPU (iterative SHA-256), GPU (matrix multiply), Storage (chunk retrievability), RAM (memory-hard), Bandwidth (timed hash). ChallengeGenerator, ProofVerifier |
| `commputer-validator` | `src/validator/` | Working | ValidatorState machine (Idle→Active→Idle), ComplianceChecker (same-IP and /24 subnet detection) |
| `commputer` (node) | `src/node/` | Working | CLI binary (run/wallet/status/send), EventLoop (swarm + epoch + block production + consensus + proofs + compliance), ConsensusManager (Snowball over network), ProofManager (challenge lifecycle) |

## What Works End-to-End

1. **Node boots** → creates/loads wallet → connects to network → registers as validator
2. **Block production** → every 2s, broadcasts as candidate with computed merkle roots
3. **Consensus** → Snowball voting over gossipsub, single-candidate fast-path finalization
4. **Epoch tick** (hourly) → finalize proof results → distribute mining rewards proportional to composite score → apply compliance nerf → persist to RocksDB
5. **Proof challenges** (every 5min) → issue challenges across all 5 channels → solve → verify → feed scores into epoch
6. **Transaction validation** → full ed25519 signature verification on receipt (mempool) AND in blocks (apply_block_validated). Transactions carry sender's public key.
7. **Block validation** → parent hash check, merkle root verification (tx_root + proof_root), signature verification on all transactions
8. **Anti-scale enforcement** → ComplianceChecker wired into live libp2p peer connections. IPs extracted from connection events, fed into checker. Same IP or /24 subnet triggers NerfedIncidental. Cleaned up on disconnect.
9. **CLI** → wallet create/recover/show/export, chain status, send (offline-only — broadcast is next task)
10. **Persistence** → RocksDB, chain survives restarts
11. **Two-node test** → integration test proves gossipsub block propagation works

## What Was JUST Completed (This Session — Mar 25 Evening)

Three features landed in quick succession:
1. **feat(core): add public key to transactions and verify signatures on receipt** (commit 077cf55)
   - Transaction struct now has `public_key: Vec<u8>` field
   - `Transaction::verify()` does full ed25519 verification (key length, address match, signature)
   - `sign_transaction()` populates public_key automatically
   - Event loop uses `tx.verify()` instead of length check
2. **feat(core,storage): add merkle roots and full block validation** (commit 72c665d)
   - `Block::compute_tx_root()` and `Block::compute_proof_root()` with real merkle tree
   - Block production sets merkle roots before broadcasting
   - `apply_block_validated` checks: height, parent hash, merkle roots, full signature verification
3. **feat(node): wire compliance checker into live peer connections** (commit 27ca012)
   - `peer_ips` and `peer_validators` maps on EventLoop
   - ConnectionEstablished extracts IP from multiaddr, registers with ComplianceChecker
   - ConnectionClosed cleans up tracking and deregisters from compliance

## What's Next (IMMEDIATE — Pick Up Here)

### 1. Transaction Broadcast from CLI (IN PROGRESS — was about to start)
The `send` command in `src/node/src/main.rs` creates and signs a transaction but doesn't broadcast it to the network. Need to either:
- Add an RPC endpoint (HTTP/JSON) to the running node that accepts transactions
- Or have the CLI briefly connect to the P2P network just to submit the tx
The RPC approach is cleaner — add a simple HTTP server (e.g., `axum` or `warp`) that listens alongside the P2P node.

### 2. Multi-Machine Testing
The founder has machines available. Test two nodes on different physical machines. This will surface real network issues (NAT, firewall, latency, state divergence).

### 3. Fork Resolution Testing
Test what happens when two validators produce competing blocks at the same height.

### 4. Error Recovery
Crash recovery, garbage peer handling, network partition behavior.

### 5. Longer Term
- Block producer validation (verify the producer was actually the anchor)
- Peer reputation / ban list for bad actors
- State pruning and snapshot/restore
- Wallet key import/export as files

## Known Issues / Warnings

- `candidates_at_height` in consensus_manager.rs has a dead_code warning (only used in tests)
- The `send` CLI command creates transactions offline — needs network broadcast (NEXT TASK)
- Proof scores are currently self-reported (node challenges itself) — needs cross-node validation
- No peer banning for bad behavior yet
- `peer_validators` map is declared but not yet populated from validator registration transactions — need to link incoming ValidatorRegister txs to the peer that sent them

## Completion Estimate

~25-30% to public testnet. The economic engine works (prove → earn → spend → burn). Next big gap is making it work across real machines with real adversarial conditions.

## Build & Run

```bash
# Build
cd ~/Coin/src && source ~/.cargo/env && cargo build --workspace

# Test (113 tests, all passing)
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
3. **Anti-scale** — one desktop at 100% is the ceiling. Exponential decay on multi-node. Nerf starts at 80%, can only increase, targets 100%.
4. **Demand-weighted emission** — floors per channel (10% CPU/GPU/Storage, 5% RAM/Bandwidth), surplus distributed by demand
5. **Grace period** — drains 1:1 offline, refills 2:1 online, capped at 10 years
6. **No founder allocation** — zero premine, zero dev tax. Founder earns through L2 products.
7. **Charitable burns** — annual, restricted to: feed hungry, cure disease, improve environment, provide healthcare, house houseless, mental health, rehabilitate addicted/incarcerated, education, elderly care, animal shelters, disability assistance, fund civil servants. NEVER: war, politics, profit ventures.
8. **Emergency access** — below 1M circulating COMME, any contribution = full access
9. **Reference node pegged to gold standard** — 0.3225 troy oz / 10.03g of gold in 2026, median currency
10. **Storage will function** — contacts notified on extended absence, free data download for listed persons on death
11. **120-year inactive wallets** considered nonexistent

## Git Identity (Local Only)

Name: The Commrade
Email: commrade@commputer.xyz

## Founder Context

- Anonymous, disabled, in medical debt
- Spending personal money on AI assistance to build this
- Not motivated by wealth — wants stability, healthcare, and to help others
- "I want to do one good thing for humanity"
- Has machines available for multi-node testing when ready
- Has contacts ready for word-of-mouth launch ("just a few phone calls")
- The Crow Show (at ~/Projects/The Crow Show/) is a separate crypto analytics platform that will become the founder's L2 — DO NOT modify it, read-only reference only
