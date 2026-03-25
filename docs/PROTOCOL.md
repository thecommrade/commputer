# Commputer Protocol Specification

## Block Format

A block consists of a header and body.

### BlockHeader

| Field | Type | Description |
|---|---|---|
| `protocol_version` | u32 | Consensus protocol version (currently 1) |
| `height` | u64 | Block height (0 = genesis) |
| `parent_hash` | [u8; 32] | SHA-256 hash of the previous block header |
| `tx_root` | [u8; 32] | Merkle root of transaction hashes |
| `proof_root` | [u8; 32] | Merkle root of proof summary hashes |
| `state_root` | [u8; 32] | Merkle root of all account states |
| `timestamp` | u64 | Unix timestamp in seconds |
| `producer` | Address ([u8; 32]) | Validator that produced this block |
| `epoch` | u64 | Current epoch number |
| `producer_public_key` | Vec\<u8\> | Ed25519 public key (32 bytes) |
| `signature` | Vec\<u8\> | Ed25519 signature over signable header bytes (64 bytes) |

**Block hash**: SHA-256 of the Borsh-serialized header.

**Signable bytes**: Borsh serialization of all header fields except `signature` and `producer_public_key`.

### Block Body

| Field | Type | Description |
|---|---|---|
| `transactions` | Vec\<Transaction\> | Up to 500 transactions |
| `proof_summaries` | Vec\<EpochProofSummary\> | Proof scores for this epoch |
| `compliance_summary` | Option\<ComplianceSummary\> | Network compliance snapshot |

### Block Limits

- Maximum transactions per block: 500
- Maximum serialized block size: 1 MB (1,048,576 bytes)

## Transaction Format

| Field | Type | Description |
|---|---|---|
| `from` | Address | Sender address |
| `nonce` | u64 | Replay-protection counter |
| `kind` | TxKind | Transaction type and payload |
| `fee` | u64 | Fee in raw units (minimum 100,000 = 0.001 COMME). Burned on inclusion. |
| `signature` | Vec\<u8\> | Ed25519 signature (64 bytes) |
| `public_key` | Vec\<u8\> | Sender's public key (32 bytes) |

**Transaction hash**: SHA-256 of the Borsh-serialized transaction.

**Signed bytes**: Borsh(from || nonce || kind || fee).

### TxKind Variants

| Variant | Description |
|---|---|
| `Transfer { to, amount }` | Standard token transfer |
| `ValidatorRegister { hardware_fingerprint_hash, contribution_percent }` | Register as validator |
| `ValidatorUpdate { contribution_percent }` | Update contribution level |
| `ValidatorExit` | Deregister as validator |
| `BurstCompute { channel, burn_amount, job_hash }` | Burn COMME for burst compute |
| `MilestoneBurn { milestone_id, burn_amount, description_hash }` | Protocol milestone burn |
| `CharitableVote { vote_epoch, proposal_hash }` | Annual charity vote |
| `CharitableDonation { vote_epoch, sell_amount, burn_amount, recipient_hash }` | Execute charity donation |
| `StorageWill { contact_hashes, options_hash }` | Configure storage will |
| `ComplianceAppeal { proof_hash }` | Appeal a compliance nerf |

## Proof Challenge Format

| Field | Type | Description |
|---|---|---|
| `channel` | ResourceChannel | One of: Processing, Gpu, Storage, Ram, Bandwidth |
| `challenge_id` | [u8; 32] | Deterministic ID from SHA-256(seed || target || channel_tag) |
| `epoch` | u64 | Epoch this challenge belongs to |
| `target` | Address | Validator being challenged |
| `payload` | Vec\<u8\> | Channel-specific challenge data |
| `deadline_block` | u64 | Block height deadline for response |

### Channel-Specific Payloads

- **Processing**: `[4-byte iterations (LE)] [32-byte seed]`
- **GPU**: `[0x02 marker] [32-byte seed]`
- **Storage**: `[0x03 marker] [4-byte offset] [4-byte length] [32-byte seed]`
- **RAM**: `[4-byte required_mb (LE)] [32-byte seed]`
- **Bandwidth**: `[4-byte data_size_kb (LE)] [32-byte seed]`

Challenge seeds are deterministic: `SHA-256(block_hash || epoch || validator_address)`.

## Consensus Rules

### Snowball Parameters
- Sample size (k): 20
- Quorum threshold (alpha): 14 (70%)
- Decision threshold (beta): 20 consecutive rounds
- Consensus timeout: 30 seconds (force-finalize)

### Block Production
- Block interval: ~10 seconds
- Anchor selection: VRF-weighted by Composite Resource Score
- Single-candidate blocks finalize immediately

### Epoch Rules
- Epoch duration: 3600 seconds (1 hour)
- Active validator set is snapshotted at epoch start
- Difficulty adjusts at epoch end: +10% if >80% pass rate, -10% if <40%
- Difficulty range: 0.2x to 5.0x

### Finality
- Finality depth: 10 blocks
- Checkpoint interval: 100 blocks (cannot reorg past checkpoints)
- Protocol version check: blocks with wrong version are rejected

### Equivocation
- Same validator signing two different blocks at the same height = slashed (zero rewards for the epoch)

### Composite Resource Score (CRS)
- Per-channel: score^0.7 (sub-linear, penalizes over-investment)
- Diversity bonus: up to 25% for contributing across all 5 channels
- Formula: `sum(channel_score^0.7) * (1 + diversity_bonus/200) * 100`
