# Commputer Node Operator Guide

## Quick Start (5 Minutes)

Get a node running and mining $COMME immediately:

The fastest path is the installer — it fetches a verified binary for your platform:

```bash
curl -sSf https://commputer.xyz/install.sh | sh

# Create a wallet (encrypted keystore at ~/.commputer/keystore.json)
commputer wallet create
# You will be prompted for a password. Save your 24-word seed phrase securely.

# Start the node (testnet)
commputer run --testnet --port 9000 --rpc-port 9944 --contribution-percent 100
```

To build from source instead:

```bash
git clone https://github.com/thecommrade/commputer.git
cd commputer/src
cargo build --release -p commputer          # workspace root is src/

# The binary lands at src/target/release/commputer
./target/release/commputer wallet create
./target/release/commputer run --testnet --port 9000 --rpc-port 9944 --contribution-percent 100
```

**That's it.** Your node is now:
- Generating and validating blocks
- Running proof challenges across all five channels
- Accumulating $COMME in your wallet
- Visible on the network

Check your balance anytime:
```bash
curl -s http://127.0.0.1:9944/health | jq .
```

To stop, press `Ctrl+C`. Your wallet and chain data persist.

---

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
git clone https://github.com/thecommrade/commputer.git
cd commputer/src                 # the cargo workspace root is src/, not the repo root
cargo build --release -p commputer

# Binary at src/target/release/commputer
```

Or install a prebuilt, checksum-verified binary:

```bash
curl -sSf https://commputer.xyz/install.sh | sh
```

## Initial Setup

### 1. Create a Wallet
```bash
commputer wallet create
```
Save the 24-word seed phrase securely. The encrypted keystore is saved to `~/.commputer/keystore.json`.

### 2. Start the Node
```bash
commputer run \
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
The node already defaults to the public testnet seed, so this is only needed to
point at a different one:

```bash
commputer run --testnet --seeds "seed.commputer.xyz:9000"
```

A multiaddr works too, if you have one for a specific peer:

```bash
commputer run --testnet --seeds "/ip4/1.2.3.4/tcp/9000/p2p/12D3KooW..."
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
- **Chain data**: `~/.commputer/testnet/` (RocksDB). Mainnet would use
  `~/.commputer/mainnet/`. Both are derived from `$HOME`, NOT from the working
  directory — see `config.rs::data_dir`. If you run the node as a system user under
  systemd, set `Environment=HOME=` to a writable path or the node cannot create these
  (see `deploy/commputer.service`).

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
- On corruption, delete `~/.commputer/testnet/` and resync from peers

## Graceful Shutdown

Send `SIGINT` (Ctrl+C) or `SIGTERM`. The node will:
1. Stop accepting new transactions
2. Flush pending state to RocksDB
3. Close peer connections
4. Exit cleanly
