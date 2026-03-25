# Commputer Node Operator Guide

## Hardware Requirements

### Minimum (Reference Node)
- CPU: 4 cores, any modern x86_64 or ARM64
- RAM: 8 GB
- Storage: 50 GB SSD
- Network: 10 Mbps symmetric
- OS: Linux, macOS, or Windows

### Recommended
- CPU: 8 cores
- RAM: 16 GB
- Storage: 256 GB NVMe SSD
- GPU: Any discrete GPU (optional, increases proof scores)
- Network: 100 Mbps symmetric

Note: Hardware beyond the reference node ceiling provides diminishing returns due to the R^0.7 sub-linear scoring.

## Installation

```bash
# Build from source
cd Coin/src
cargo build --release -p commputer-node

# Binary at target/release/commputer-node
```

## Initial Setup

### 1. Create a Wallet
```bash
commputer-node wallet create
```
Save the 24-word seed phrase securely. The encrypted keystore is saved to `~/.commputer/keystore.json`.

### 2. Start the Node
```bash
commputer-node run \
  --port 9000 \
  --rpc-port 9944 \
  --contribution-percent 100
```

The node will:
- Generate a genesis block (testnet)
- Auto-register as a validator
- Start the P2P listener on port 9000
- Start the RPC server on port 9944
- Begin block production and proof challenges

### 3. Connect to Seeds
```bash
commputer-node run --seeds "/ip4/1.2.3.4/tcp/9000/p2p/12D3KooW..."
```

Or via DNS seeds:
```bash
commputer-node run --dns-seeds "seed1.commputer.network,seed2.commputer.network"
```

## Configuration

| Flag | Default | Description |
|---|---|---|
| `--port` | 9000 | P2P listen port |
| `--rpc-port` | 9944 | RPC server port |
| `--contribution-percent` | 100 | Resources to contribute (1-100) |
| `--testnet` | true | Run on testnet |
| `--log-level` | info | Log verbosity (trace/debug/info/warn/error) |
| `--relay` | false | Enable relay protocol for NAT traversal |
| `--seeds` | (none) | Comma-separated seed multiaddrs |
| `--dns-seeds` | (none) | Comma-separated DNS seed domains |

## Monitoring

### Health Check
```bash
curl http://127.0.0.1:9944/health
```

### Chain Status
```bash
curl http://127.0.0.1:9944/status
```

### Metrics
```bash
curl http://127.0.0.1:9944/metrics
```

### Peer Info
```bash
curl http://127.0.0.1:9944/peers
```

### Compliance Status
```bash
curl http://127.0.0.1:9944/compliance
```

### Storage Metrics
```bash
curl http://127.0.0.1:9944/storage/metrics
```

## Data Directories

- **Keystore**: `~/.commputer/keystore.json`
- **Chain data**: `commputer-testnet/` (RocksDB, relative to working directory)

## Troubleshooting

### Node won't start
- Check that ports 9000 and 9944 are not in use
- Ensure RocksDB development libraries are installed
- Check log output with `--log-level debug`

### No peers connecting
- Verify firewall allows inbound TCP on port 9000
- Use `--seeds` to specify known peer addresses
- Check `curl http://127.0.0.1:9944/peers` for connection status

### Sync stalling
- The node auto-syncs missing blocks via the block request protocol
- Check the height via `/status` endpoint
- Minimum 2 peers required for block production

### Low proof scores
- Ensure `--contribution-percent 100` for maximum scores
- GPU proofs cap at 50 without a detected GPU (set `COMMPUTER_GPU=1` to override)
- Check `/proofs/status` for channel details

### Compliance nerf
- Check `/compliance` for current compliance status
- Common causes: same subnet as another validator, datacenter IP, VPN
- Resolution: ensure single node per residential IP

### RocksDB corruption
- RocksDB uses WAL with PointInTime recovery
- On corruption, delete the `commputer-testnet/` directory and resync

## Graceful Shutdown

Send `SIGINT` (Ctrl+C) or `SIGTERM`. The node will:
1. Stop accepting new transactions
2. Flush pending state to RocksDB
3. Close peer connections
4. Exit cleanly
