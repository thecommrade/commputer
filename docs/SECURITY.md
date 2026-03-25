# Commputer Security Model

## Threat Model

Commputer's security model assumes:
- Adversaries may control up to 33% of network validators
- Adversaries have access to datacenter-grade hardware
- Adversaries may attempt Sybil attacks (many fake identities)
- Individual private keys may be compromised
- Network partitions may occur

## Cryptographic Primitives

| Primitive | Usage | Library |
|---|---|---|
| Ed25519 | Transaction and block signing | `ed25519-dalek` |
| SHA-256 | Block hashing, challenge derivation, merkle trees | `sha2` |
| AES-256-GCM | Keystore encryption | `aes-gcm` |
| Argon2id | Password-to-key derivation | `argon2` |
| BIP39 | Seed phrase generation (24 words, 256-bit entropy) | `bip39` |
| Noise Protocol | P2P transport encryption | `libp2p-noise` |

## Attack Vectors and Mitigations

### 1. Sybil Attack (Many Fake Nodes)
**Vector**: Spin up thousands of validators to dominate block production.

**Mitigations**:
- Multi-node exponential decay: node 2 earns 25%, node 3 earns 6.25%, node 5+ earns 0%
- IP-based colocation detection (/24 and /16 subnet matching)
- Hardware fingerprint deduplication
- Adaptive nerf rate (80-100%) for non-compliant nodes
- 100 nerfed warehouse nodes earn less than 1 honest desktop

### 2. Warehouse Attack (Datacenter Farming)
**Vector**: Deploy high-powered hardware in datacenters to earn disproportionate rewards.

**Mitigations**:
- Gold standard hardware ceiling (pegged to ~10g of gold purchasing power)
- Sub-linear CRS formula (R^0.7) penalizes over-investment in any channel
- Datacenter IP detection (AWS, GCP, Azure, Hetzner, OVH, DigitalOcean)
- Behavioral analysis: >99.5% uptime flags as datacenter pattern
- VPN/proxy detection: >3 validators behind same IP flagged
- Resource spike cooldown: RAM/CPU jumping >100% triggers 3-epoch zero rewards

### 3. Equivocation (Double-Signing)
**Vector**: Validator signs two different blocks at the same height to fork the chain.

**Mitigations**:
- Equivocation detection in ConsensusManager
- Slashing: equivocating validators earn zero rewards for the epoch
- Snowball consensus naturally resolves forks (probabilistic finality)

### 4. Transaction Replay
**Vector**: Resubmit a valid transaction to double-spend.

**Mitigations**:
- Per-account nonce (must be strictly sequential)
- Transaction hash deduplication in mempool
- Seen transaction tracking across finalized blocks

### 5. Block Withholding
**Vector**: Anchor validator produces a block but withholds it.

**Mitigations**:
- Consensus timeout (30 seconds) force-finalizes on any available candidate
- Multiple candidates can exist at the same height
- Snowball voting converges without requiring the original producer

### 6. Network Partition
**Vector**: Network splits into disconnected segments.

**Mitigations**:
- Minimum peers threshold (2) before considering the network operational
- Kademlia DHT for peer discovery
- Gossipsub with fan-out (8 peers) for redundant propagation
- Block request/response protocol for catching up after partition heals

### 7. Key Compromise
**Vector**: Attacker obtains a validator's private key.

**Mitigations**:
- Keystore encrypted with AES-256-GCM + Argon2id password derivation
- 24-word BIP39 seed phrase for offline backup
- Transaction signatures include the public key for verification
- Address derived from public key via SHA-256 (one-way)

### 8. Proof Spoofing
**Vector**: Validator claims resources they don't have.

**Mitigations**:
- Deterministic proof challenges: all honest nodes agree on valid challenges
- Full recomputation verification for CPU proofs
- Timing enforcement: suspiciously fast responses flagged
- RAM proof: memory-hard buffer fill prevents shortcuts
- Storage proof: random chunk challenges against actual stored data
- GPU proof: CPU fallback detected and score capped at 50
- Bandwidth proof: cross-verification between paired validators

### 9. Denial of Service
**Vector**: Flood a node with invalid messages.

**Mitigations**:
- Per-peer message rate limiting
- Peer reputation scoring (starts at 100, decreases on bad behavior)
- Ban list for peers sending invalid blocks or signatures
- Connection limit (max 50 peers)
- Gossipsub message deduplication (nonce-based, 100K seen cache)

### 10. Long-Range Attack
**Vector**: Create an alternative chain from far in the past.

**Mitigations**:
- Finality depth of 10 blocks (cannot reorg deeper)
- Checkpoint interval of 100 blocks (hard finality)
- Cumulative CRS for fork choice (heaviest resource-score chain wins)
- Protocol version check rejects blocks with outdated versions

## Keystore Security

The keystore uses:
1. **Random 16-byte salt** per keystore file
2. **Argon2id** key derivation (memory-hard, resistant to GPU cracking)
3. **Random 12-byte nonce** for AES-GCM
4. **AES-256-GCM** authenticated encryption (ciphertext includes 16-byte auth tag)

The seed phrase is never stored in plaintext on disk.

## P2P Security

- All connections use the Noise protocol for authenticated encryption
- Yamux for multiplexed streams over a single TCP connection
- Identify protocol for peer capability exchange
- Gossipsub with signed message authentication
