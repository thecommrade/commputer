# Commputer RPC API

The RPC server runs on port 9944 (configurable via `--rpc-port`). All endpoints return JSON.

## POST /tx

Submit a signed transaction.

**Request:**
```json
{
  "from": [0,0,0,...],
  "nonce": 0,
  "kind": {
    "Transfer": {
      "to": [1,1,1,...],
      "amount": 1000000000
    }
  },
  "fee": 100000,
  "signature": [/* 64 bytes */],
  "public_key": [/* 32 bytes */]
}
```

**Response (200):**
```json
{
  "accepted": true,
  "tx_hash": "a1b2c3d4..."
}
```

**Response (400 -- invalid signature):**
```json
{
  "accepted": false,
  "tx_hash": "",
  "error": "Signature verification failed"
}
```

## GET /status

Chain status snapshot.

**Response:**
```json
{
  "height": 12345,
  "total_supply": 200000000000000000,
  "emitted": 500000000000,
  "burned": 100000000,
  "circulating": 499900000000,
  "remaining": 199999500000000000,
  "accounts": 1024,
  "epoch": 342,
  "pending_txs": 5
}
```

## GET /peers

Connected peer list.

**Response:**
```json
[
  {
    "peer_id": "12D3KooW...",
    "ip": "192.168.1.50",
    "validator_address": "comme:a1b2c3d4",
    "compliance_status": "Compliant"
  }
]
```

## GET /balance/:address

Account balance for a hex-encoded address.

**Response:**
```json
{
  "address": "a1b2c3d4...",
  "balance": 3300000000,
  "tier": "Full",
  "nonce": 5,
  "is_validator": true,
  "total_mined": 3300000000
}
```

## GET /mempool

Pending transactions in the mempool.

**Response:**
```json
[
  {
    "tx_hash": "f1e2d3...",
    "from": "comme:a1b2c3d4",
    "nonce": 6,
    "fee": 100000,
    "kind": "Transfer"
  }
]
```

## GET /block/:height

Block data at the given height.

**Response:**
```json
{
  "height": 100,
  "hash": "b1a2c3...",
  "parent_hash": "a0b1c2...",
  "producer": "comme:d4e5f6",
  "timestamp": 1700000000,
  "epoch": 28,
  "tx_count": 3,
  "transactions": [...]
}
```

## GET /receipt/:tx_hash

Transaction receipt (inclusion proof).

**Response:**
```json
{
  "tx_hash": "f1e2d3...",
  "block_hash": "b1a2c3...",
  "block_height": 100,
  "tx_index": 0,
  "success": true
}
```

## GET /metrics

Node operational metrics.

**Response:**
```json
{
  "uptime_secs": 86400,
  "height": 12345,
  "epoch": 342,
  "peers_connected": 8,
  "peers_banned": 1,
  "blocks_produced": 50,
  "pending_txs": 3,
  "seen_tx_count": 1200
}
```

## GET /proofs/status

Proof system status.

**Response:**
```json
{
  "epoch": 342,
  "height": 12345,
  "channels": ["Processing", "Gpu", "Storage", "Ram", "Bandwidth"],
  "challenge_interval_blocks": 300
}
```

## GET /health

Health check endpoint.

**Response:**
```json
{
  "healthy": true,
  "height": 12345,
  "epoch": 342,
  "peers": 8,
  "pending_txs": 3
}
```

## GET /compliance

Network-wide compliance statistics.

**Response:**
```json
{
  "total_validators": 100,
  "compliant_count": 95,
  "nerfed_count": 5,
  "current_nerf_percentage": 8100,
  "suspicious_count": 2
}
```

## GET /anti-scale

Anti-scale enforcement metrics.

**Response:**
```json
{
  "total_warehouse_detections": 12,
  "total_nerfed_rewards": 500000000,
  "nerf_percentage_history": [[100, 8000], [200, 8100]],
  "largest_detected_clusters": [[3, "192.168.1.0"]]
}
```

## GET /network

Network health dashboard.

**Response:**
```json
{
  "peer_count": 15,
  "unique_subnets": 12,
  "avg_latency_ms": 45,
  "partition_risk": "low"
}
```

## GET /network/quality

Per-peer connection quality metrics.

**Response:**
```json
{
  "12D3KooW...": {
    "avg_latency_ms": 32,
    "messages_received": 5000,
    "messages_dropped": 2,
    "connected_since": 1700000000
  }
}
```

## GET /storage/metrics

Storage layer performance metrics.

**Response:**
```json
{
  "db_size_bytes": 104857600,
  "total_reads": 50000,
  "total_writes": 10000,
  "avg_read_us": 15,
  "avg_write_us": 120
}
```
