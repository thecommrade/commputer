# Commputer RPC Examples

All examples assume the node is running with `--rpc-port 9944`.

## Health Check

```bash
curl -s http://127.0.0.1:9944/health | jq .
```
```json
{
  "healthy": true,
  "height": 1234,
  "epoch": 34,
  "peers": 5,
  "pending_txs": 0
}
```

## Chain Status

```bash
curl -s http://127.0.0.1:9944/status | jq .
```
```json
{
  "height": 1234,
  "total_supply": 200000000000000000,
  "emitted": 5000000000,
  "burned": 100000000,
  "circulating": 4900000000,
  "remaining": 199999995000000000,
  "accounts": 42,
  "epoch": 34,
  "pending_txs": 2
}
```

## Get Balance

```bash
curl -s http://127.0.0.1:9944/balance/a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2 | jq .
```
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

## List Peers

```bash
curl -s http://127.0.0.1:9944/peers | jq .
```
```json
[
  {
    "peer_id": "12D3KooWExample...",
    "ip": "192.168.1.50",
    "validator_address": "comme:a1b2c3d4",
    "compliance_status": "Compliant"
  }
]
```

## Submit Transaction

```bash
curl -s -X POST http://127.0.0.1:9944/tx \
  -H "Content-Type: application/json" \
  -d '{
    "from": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1],
    "nonce": 0,
    "kind": {"Transfer": {"to": [1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1], "amount": 100000000}},
    "fee": 100000,
    "signature": [/* 64 bytes */],
    "public_key": [/* 32 bytes */]
  }' | jq .
```
```json
{
  "accepted": true,
  "tx_hash": "f1e2d3c4b5a6..."
}
```

## Get Block by Height

```bash
curl -s http://127.0.0.1:9944/block/100 | jq .
```
```json
{
  "height": 100,
  "hash": "b1a2c3d4...",
  "parent_hash": "a0b1c2d3...",
  "producer": "comme:d4e5f6a7",
  "timestamp": 1700001000,
  "epoch": 2,
  "tx_count": 1,
  "transactions": [...]
}
```

## Get Transaction Receipt

```bash
curl -s http://127.0.0.1:9944/receipt/f1e2d3c4b5a6... | jq .
```
```json
{
  "tx_hash": "f1e2d3c4b5a6...",
  "block_hash": "b1a2c3d4...",
  "block_height": 100,
  "tx_index": 0,
  "success": true
}
```

## View Mempool

```bash
curl -s http://127.0.0.1:9944/mempool | jq .
```
```json
[
  {
    "tx_hash": "a1b2c3...",
    "from": "comme:f1e2d3c4",
    "nonce": 3,
    "fee": 100000,
    "kind": "Transfer"
  }
]
```

## Node Metrics

```bash
curl -s http://127.0.0.1:9944/metrics | jq .
```
```json
{
  "uptime_secs": 86400,
  "height": 1234,
  "epoch": 34,
  "peers_connected": 5,
  "peers_banned": 0,
  "blocks_produced": 120,
  "pending_txs": 0,
  "seen_tx_count": 500
}
```

## Proof Status

```bash
curl -s http://127.0.0.1:9944/proofs/status | jq .
```
```json
{
  "epoch": 34,
  "height": 1234,
  "channels": ["Processing", "Gpu", "Storage", "Ram", "Bandwidth"],
  "challenge_interval_blocks": 300
}
```

## Compliance Dashboard

```bash
curl -s http://127.0.0.1:9944/compliance | jq .
```
```json
{
  "total_validators": 50,
  "compliant_count": 48,
  "nerfed_count": 2,
  "current_nerf_percentage": 8040,
  "suspicious_count": 1
}
```

## Anti-Scale Metrics

```bash
curl -s http://127.0.0.1:9944/anti-scale | jq .
```
```json
{
  "total_warehouse_detections": 3,
  "total_nerfed_rewards": 100000000,
  "nerf_percentage_history": [[100, 8000], [200, 8040]],
  "largest_detected_clusters": [[2, "192.168.1.0"]]
}
```

## Network Health

```bash
curl -s http://127.0.0.1:9944/network | jq .
```
```json
{
  "peer_count": 12,
  "unique_subnets": 10,
  "avg_latency_ms": 35,
  "partition_risk": "low"
}
```

## Peer Connection Quality

```bash
curl -s http://127.0.0.1:9944/network/quality | jq .
```
```json
{
  "12D3KooWExample...": {
    "avg_latency_ms": 25,
    "messages_received": 10000,
    "messages_dropped": 5,
    "connected_since": 1700000000
  }
}
```

## Storage Metrics

```bash
curl -s http://127.0.0.1:9944/storage/metrics | jq .
```
```json
{
  "db_size_bytes": 52428800,
  "total_reads": 25000,
  "total_writes": 5000,
  "avg_read_us": 12,
  "avg_write_us": 85
}
```
