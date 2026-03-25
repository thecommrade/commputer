# Commputer Architecture

## High-Level Overview

Commputer is a Layer 1 blockchain that coordinates a distributed supercomputer built from small contributions by regular people. The system uses multi-dimensional Proof of Work across five resource channels (CPU, GPU, Storage, RAM, Bandwidth) with Snowball consensus for block finalization.

```
┌─────────────────────────────────────────────────────┐
│                   commputer-node                     │
│  ┌──────────┐ ┌──────────────┐ ┌──────────────────┐ │
│  │ EventLoop│ │ConsensusManager│ │  ProofManager    │ │
│  │          │ │  (Snowball)    │ │  (5 channels)    │ │
│  └─────┬────┘ └──────┬───────┘ └────────┬─────────┘ │
│        │             │                   │           │
│  ┌─────┴─────────────┴───────────────────┴─────────┐ │
│  │                  RPC Server (:9944)              │ │
│  └──────────────────────────────────────────────────┘ │
└─────────────────────┬───────────────────────────────┘
                      │
    ┌─────────────────┼─────────────────┐
    │                 │                 │
┌───▼───┐      ┌─────▼─────┐    ┌──────▼──────┐
│network│      │ storage   │    │  consensus  │
│(libp2p)│     │(RocksDB)  │    │(Snowball+DAG)│
└───┬───┘      └─────┬─────┘    └──────┬──────┘
    │                │                 │
┌───▼───┐      ┌─────▼─────┐    ┌──────▼──────┐
│gossipsub│    │ accounts  │    │  emission   │
│kademlia │    │ blocks    │    │  epochs     │
│identify │    │ receipts  │    │  anchor     │
└─────────┘    └───────────┘    └─────────────┘
                      │
              ┌───────▼───────┐
              │  commputer-core │
              │ (types, crypto) │
              └───────────────┘
```

## Crate Responsibilities

### commputer-core
Foundation types shared by all crates. Zero external blockchain dependencies.

- **block.rs** -- `Block`, `BlockHeader`, `BlockHash`, merkle root computation, signature verification
- **transaction.rs** -- `Transaction`, `TxKind` (Transfer, ValidatorRegister, BurstCompute, etc.), fee constants
- **proof.rs** -- `ResourceChannel` (5 channels), `ProofChallenge`, `ProofResponse`, `EpochProofSummary`, sub-linear CRS formula (R^0.7)
- **identity.rs** -- `Address`, `HardwareFingerprint` (with GPU detection), `ResourceCapacity`, `ValidatorIdentity`
- **compliance.rs** -- `ComplianceStatus`, `NerfRate` (adaptive 80-100%), `ComplianceFlag`, multi-node exponential decay
- **token.rs** -- `Amount` type with checked arithmetic, 2B total supply, 10^8 units per COMME
- **tier.rs** -- `HolderTier` (None/Base/Storage/Compute/Full), thresholds (1/10/20/33), 51/49 split allocation
- **wallet.rs** -- ed25519 key generation, BIP39 seed phrases (24 words)
- **keystore.rs** -- AES-256-GCM encrypted keystore with Argon2id key derivation
- **signing.rs** -- Transaction and block signing/verification helpers
- **error.rs** -- `CommpError` enum with all error variants
- **merkle.rs** -- Merkle proof generation and verification for light clients

### commputer-consensus
Consensus engine and economic model.

- **snowball.rs** -- Snowball voter: probabilistic consensus with configurable sample size (k=20), quorum (alpha=14), and decision threshold (beta=20)
- **dag.rs** -- Multi-channel DAG: five parallel sub-DAGs with cross-channel references, tip tracking, finalization
- **epoch.rs** -- Epoch state (1 hour duration), proof summary aggregation, difficulty adjustment (10% up/down based on pass rates)
- **emission.rs** -- Hybrid emission curve: 0.09 COMME/day base rate, inverse sqrt scaling above 10K validators, 0.01 COMME/day floor; demand-weighted channel allocation with guaranteed floors
- **anchor.rs** -- VRF-weighted anchor selection: deterministic block producer election based on Composite Resource Score

### commputer-storage
Persistent chain state.

- **account.rs** -- `Account` (balance, nonce, tier, compliance, grace period), `AccountStore` with state root computation
- **blockstore.rs** -- In-memory `BlockStore` with height index and pruning
- **rocks.rs** -- `RocksStore` backed by RocksDB with column families (blocks, accounts, meta, archived), WAL recovery, schema migrations, atomic write batches
- **state.rs** -- `ChainState` combining accounts + blocks + supply tracking + receipts; block application logic, state diffs, snapshots, archival, garbage collection
- **receipt.rs** -- `TxReceipt` and `AccountHistoryIndex` for transaction history
- **traits.rs** -- `Storage` trait for pluggable backends

### commputer-network
P2P networking via libp2p.

- **transport.rs** -- `CommpNetwork` wrapping libp2p Swarm with TCP+Noise+Yamux, gossipsub, Kademlia DHT, identify protocol; seed node connection, DNS seeds, Kademlia bootstrap
- **message.rs** -- `NetworkMessage` and `MessageKind` enum (NewBlock, NewTransaction, SnowballQuery/Response, ProofChallenge/Response, Ping/Pong)
- **peer.rs** -- `PeerStore` with RTT tracking, stale peer pruning, random sampling for Snowball
- **gossip.rs** -- `GossipRouter` with message deduplication (nonce-based), priority ordering, fan-out peer selection
- **topics.rs** -- Gossipsub topic strings for blocks, transactions, consensus, and proofs
- **compress.rs** -- Deflate message compression with 1-byte prefix framing

### commputer-proofs
Multi-dimensional Proof of Work.

- **cpu.rs** -- `CpuProver`: iterative SHA-256 hashing puzzle (N rounds)
- **gpu.rs** -- `GpuProver`: 64x64 matrix multiplication + hash; GPU detection with CPU fallback (score capped at 50)
- **ram.rs** -- `RamProver`: memory-hard buffer fill + random read access (1MB-256MB)
- **bandwidth.rs** -- `BandwidthProver`: timed data generation + hash; paired validator cross-verification
- **storage_proof.rs** -- `StorageProver`: random chunk retrievability challenges against stored data
- **verifier.rs** -- `ProofVerifier`: unified verification dispatching to channel-specific verifiers, timing suspicion checks
- **challenge.rs** -- `ChallengeGenerator`: deterministic challenge generation from SHA-256(block_hash || epoch || validator_address), per-channel difficulty scaling

### commputer-validator
Validator lifecycle and compliance.

- **lifecycle.rs** -- `ValidatorState` state machine (Idle -> Active -> Idle), contribution percentage tracking
- **compliance_check.rs** -- `ComplianceChecker`: IP-based colocation detection (/24, /16 subnet, ASN), hardware fingerprint matching, datacenter IP detection (AWS/GCP/Azure/Hetzner/OVH/DO), VPN/proxy detection, resource spike cooldowns, behavioral analysis, sybil suspicion scoring, compliance history, trust whitelist

### commputer-node
Binary entrypoint and runtime.

- **main.rs** -- CLI (clap): `run`, `wallet create/recover/show/export`, `status`, `send`, `peers`, `balance`, `verify-chain`, `export-chain`, `version`
- **event_loop.rs** -- Main async loop: network event processing, block production (10s interval), epoch transitions, Snowball voting, proof challenge/response handling, RPC state updates, graceful shutdown
- **rpc.rs** -- Axum HTTP server on port 9944: `/tx`, `/status`, `/peers`, `/balance/:addr`, `/mempool`, `/block/:height`, `/receipt/:hash`, `/metrics`, `/proofs/status`, `/health`, `/compliance`, `/anti-scale`, `/network`, `/network/quality`, `/storage/metrics`
- **proof_manager.rs** -- Coordinates proof lifecycle: challenge generation for all 5 channels, challenge solving dispatch, epoch finalization with difficulty-weighted scoring
- **consensus_manager.rs** -- Manages Snowball voting per height: candidate tracking, response accumulation, single-candidate fast-path, consensus timeout (30s), equivocation detection and slashing

## Data Flow

1. **Transaction submission**: Client -> RPC `/tx` -> EventLoop mempool -> gossipsub broadcast
2. **Block production**: EventLoop timer (10s) -> collect mempool txs -> compute merkle roots -> sign block -> broadcast via gossipsub
3. **Block finalization**: Receive block -> ConsensusManager adds candidate -> Snowball voting rounds -> finalize -> apply to ChainState
4. **Proof challenges**: Every 300 blocks -> ProofManager generates challenges for all channels -> broadcast -> validators solve -> responses collected -> epoch finalization scores validators
5. **Epoch transition**: Every 3600s -> compute emission allocation -> distribute rewards (weighted by CRS) -> adjust difficulty -> snapshot active validator set
6. **Compliance**: On peer connect -> ComplianceChecker registers IP -> check subnet/fingerprint/datacenter -> nerf non-compliant validators' rewards
