# Commputer Development Resume Document

**Last updated:** 2026-03-26 (overnight session, continued)
**Last session test count:** 131 passing, 0 failing
**Commits:** 72
**Lines of Rust:** ~9,000

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
9. **CLI** → wallet create/recover/show/export, chain status, send (broadcasts via RPC), peers, balance, version
10. **Persistence** → RocksDB, chain survives restarts
11. **Two-node test** → integration test proves gossipsub block propagation works
12. **RPC server** → axum on port 9944: submit tx, chain status, peers, balance, mempool, block explorer, health
13. **Transaction fees** → minimum fee enforced, fees burned on block inclusion
14. **Block validation** → size limits (500 tx / 1MB), timestamp checks, producer signature verification
15. **Mempool protection** → nonce validation, double-spend prevention, size cap with fee-based eviction
16. **Bad peer handling** → invalid block senders are banned and disconnected
17. **Graceful shutdown** → SIGINT/SIGTERM flushes state to RocksDB

## What Was JUST Completed (This Session — Mar 25-26 Overnight)

Massive overnight session. 19 new commits, 12 new tests, ~1,400 new lines of Rust.

### RPC Server & CLI (commits 33c9332..af71915)
1. **RPC server for transaction broadcast** — axum HTTP server on port 9944. POST /tx submits signed transactions, GET /status returns chain info. CLI `send` command broadcasts via RPC.
2. **RPC tests** — 4 unit tests: signed tx accepted, unsigned rejected, bad signature rejected, status endpoint works.
3. **CLI peers command** — GET /peers shows connected peers, IPs, validator addresses, compliance status.
4. **CLI balance command** — GET /balance/{address} shows balance, tier, nonce, validator status.
5. **CLI version command** — prints protocol version, network, supply, consensus params.
6. **Mempool RPC** — GET /mempool returns pending transactions.
7. **Health endpoint** — GET /health returns node health status.
8. **Block explorer RPC** — GET /block/{height} returns full block data for last 100 blocks.

### Protocol Hardening (commits ef3efd8..3e22cbd)
9. **Wire peer_validators map** — ValidatorRegister transactions now link sender Address to the PeerId, feeding into compliance checker.
10. **Bad peer handling** — peers sending blocks with bad merkle roots or invalid tx signatures are banned and disconnected. Messages from banned peers are dropped.
11. **Block producer signing** — producers sign block headers with ed25519 wallet key. BlockHeader gains producer_public_key field. verify_producer_signature() method.
12. **Nonce validation & double-spend prevention** — mempool validates nonce matches expected next nonce (on-chain + pending). Tracks seen tx hashes to reject duplicates.
13. **Transaction fees** — fee field on Transaction, MINIMUM_FEE = 0.0001 COMME. Fees are burned on block inclusion.
14. **Block size limits** — MAX_TRANSACTIONS_PER_BLOCK = 500, MAX_BLOCK_SIZE_BYTES = 1MB. Block production caps txs; received blocks validated.
15. **Timestamp validation** — reject blocks >30s in future or before parent timestamp.
16. **Mempool size limit** — capped at 5000 txs, lowest-fee evicted when full.

### Networking & Infrastructure (commits 05d0cef..c1c2341)
17. **Protocol handshake** — identify protocol checks /commputer/ prefix, disconnects incompatible peers.
18. **Connection timeout** — idle connections closed after 60 seconds.
19. **Graceful shutdown** — SIGINT/SIGTERM handler flushes chain state to RocksDB before exit.

### Storage (commit 931a336)
20. **Block pruning** — blocks older than 1000 heights pruned from memory, remain in RocksDB.

### Testing (commits 0d49dbd..93fa4b6)
21. **Fork resolution tests** — unit tests for two ConsensusManagers converging, minority block losing. Integration test for competing block propagation via gossipsub.

### Continuation Session (commits 0532953..0ead767)
22. **Peer reputation scoring** — numeric scores (-100 to 200), auto-ban below -50, reward valid blocks.
23. **Connection limit** — max 50 peers, new connections rejected when at capacity.
24. **Block request/response protocol** — BlockRequest/BlockResponse messages for sync.
25. **Initial sync protocol** — on startup with peers, request next 10 blocks to catch up.
26. **Node metrics RPC** — GET /metrics returns height, epoch, peers, pending txs, banned count.
27. **Proof status RPC** — GET /proofs/status returns channel info.
28. **Export-chain CLI** — commputer export-chain dumps state to JSON.
29. **Verify-chain CLI** — walks entire chain, verifies merkle roots and signatures.
30. **Grace period tracking** — refill on epoch tick, drain on peer disconnect.
31. **RocksDB block fallback** — ChainState::get_block_by_height falls back to RocksDB for pruned blocks.
32. **Tier change logging** — log when transfers cause tier threshold crossings.
33. **Integer overflow audit** — saturating_add for total_emitted/total_burned.
34. **Config validation** — port collision check, privileged port warnings.
35. **Test fixtures library** — commputer_core::testutil with shared helpers.

## What's Next (IMMEDIATE — Pick Up Here)

### 1. Genesis Configuration File (JSON)
Define total supply, emission curve, channel floors, epoch duration, reference node specs in a JSON config file. Load on first boot instead of hardcoding.

### 2. Chain Reorganization
If a node receives a valid chain that's longer than its current chain, switch to it. Handle orphaned transactions.

### 3. Multi-Machine Integration Test
Two data dirs, two ports, two wallets, connect, verify same chain state after 10 blocks.

### 4. Sync Protocol
When a node starts behind the network, sync missing blocks from peers before consensus.

### 5. Block Request Protocol
If a node is behind, request specific blocks by height from peers.

### 6. Proof Channel Improvements
- Cross-node proof verification
- Proof difficulty scaling per epoch
- Real bandwidth/storage challenges

### 7. State & Storage
- Account state merkle tree for state_root
- State snapshots for fast bootstrap
- Grace period tracking in live node

### 8. Economic Refinement
- Demand-weighted emission live calculation
- Adaptive nerf percentage
- Burst compute pricing
- Milestone burn triggers

## Known Issues / Warnings

- `candidates_at_height` in consensus_manager.rs has a dead_code warning (only used in tests)
- Proof scores are currently self-reported (node challenges itself) — needs cross-node validation
- No chain reorganization yet — longer-chain switching not implemented
- No sync protocol — nodes that start behind can't catch up from peers
- No block request protocol — can't ask peers for specific blocks
- Genesis config is hardcoded — should be loaded from JSON file
- `seen_tx_hashes` grows unbounded — needs periodic pruning
- Block explorer RPC cache only holds last 100 blocks

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
