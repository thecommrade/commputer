# Commputer Error Catalog

## Core Errors (`CommpError`)

| Variant | Description | Common Causes |
|---|---|---|
| `InvalidBlock(String)` | Block validation failed | Wrong protocol version, bad merkle roots, exceeds size limits, missing producer signature |
| `InvalidTransaction(String)` | Transaction validation failed | Bad signature, nonce mismatch, invalid TxKind fields |
| `InvalidProof(String)` | Proof verification failed | Wrong result, timed out, mismatched challenge ID |
| `ComplianceViolation(String)` | Anti-scale rule triggered | Colocated nodes, duplicate fingerprints, datacenter IP, VPN/proxy detected |
| `InsufficientBalance { have, need }` | Balance too low for operation | Transfer amount + fee exceeds balance |
| `UnknownValidator(String)` | Validator not registered | Operating before ValidatorRegister tx is confirmed |
| `Serialization(String)` | Borsh/JSON serialization failed | Corrupt data, schema mismatch |
| `Crypto(String)` | Cryptographic operation failed | Wrong password (keystore), invalid seed phrase, AES decryption failure, Argon2 error |
| `Storage(String)` | Storage I/O error | RocksDB failure, file not found, disk full |

## DAG Errors (`DagError`)

| Variant | Description |
|---|---|
| `UnknownParent(BlockHash)` | Vertex references a parent not in the DAG |
| `Duplicate(BlockHash)` | Vertex with this hash already exists |

## Proof Verdicts (`ProofVerdict`)

| Verdict | Meaning |
|---|---|
| `Valid` | Proof correct and timely |
| `Invalid` | Wrong result, wrong challenge ID, or verification failure |
| `TimedOut` | No response before deadline block |
| `Suspicious` | Correct result but timing suggests resource mismatch |

## Compliance Statuses

| Status | Reward Impact | Recovery |
|---|---|---|
| `Compliant` | 100% rewards | N/A |
| `NerfedIncidental` | 80%+ reward reduction | Resolve immediately (e.g., remove colocated node) |
| `NerfedAdversarial` | 80%+ reward reduction | Scale back to single compliant node |

## Compliance Flags

| Flag | Trigger |
|---|---|
| `ColocatedNodes` | Multiple nodes detected on same network via latency triangulation |
| `DuplicateFingerprint` | Hardware fingerprint hash matches another validator |
| `ExceedsReferenceCeiling` | Resource capacity above reference node maximum |
| `ResourceSpike` | RAM or CPU jumped by >100% between reports (3-epoch cooldown) |
| `DatacenterPattern` | >99.5% uptime or flat resource variance |
| `TimingAnomaly` | Challenge response faster than expected for reported hardware |
| `SameSubnet16` | Another validator on the same /16 subnet |
| `SameAsn` | Another validator on the same ASN |
| `DatacenterIp` | IP belongs to known cloud provider (AWS, GCP, Azure, Hetzner, OVH, DO) |
| `VpnProxy` | >3 validators behind the same IP address |
| `GeographicProximity` | Same /16 subnet or same ASN as another validator |

## RPC Error Responses

| Endpoint | Status | Meaning |
|---|---|---|
| `POST /tx` | 400 | Signature verification failed |
| `POST /tx` | 503 | Transaction queue full |
| `POST /tx` | 500 | Node shutting down |
| `GET /balance/:addr` | 404 | Account not found |
| `GET /block/:height` | 404 | Block not found |
| `GET /receipt/:hash` | 404 | Receipt not found |
