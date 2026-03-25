# Commputer Configuration Reference

## CLI Commands

### `commputer-node run`
Start the node.

| Flag | Type | Default | Description |
|---|---|---|---|
| `--testnet` | bool | `true` | Run on testnet |
| `--log-level` | string | `info` | Log level: trace, debug, info, warn, error |
| `--port` | u16 | `9000` | P2P listen port (TCP) |
| `--rpc-port` | u16 | `9944` | JSON RPC server port (HTTP) |
| `--contribution-percent` | u8 | `100` | Percentage of hardware to contribute (1-100) |
| `--relay` | bool | `false` | Enable relay protocol for NAT traversal |
| `--seeds` | string[] | (empty) | Comma-separated seed node multiaddrs |
| `--dns-seeds` | string[] | (empty) | Comma-separated DNS seed domains |

### `commputer-node wallet create`
Generate a new wallet. Prompts for a password and saves an encrypted keystore.

### `commputer-node wallet recover`
Recover a wallet from a 24-word BIP39 seed phrase.

### `commputer-node wallet show`
Display the wallet address from the keystore.

### `commputer-node wallet export`
Export the seed phrase (requires password).

### `commputer-node status`
Print chain status from a local data directory.

| Flag | Type | Default | Description |
|---|---|---|---|
| `--testnet` | bool | `true` | Which data directory to read |

### `commputer-node peers`
Query a running node's peers via RPC.

| Flag | Type | Default | Description |
|---|---|---|---|
| `--rpc-port` | u16 | `9944` | RPC port of the running node |

### `commputer-node balance`
Query an account balance via RPC.

| Flag | Type | Default | Description |
|---|---|---|---|
| `--rpc-port` | u16 | `9944` | RPC port |
| `--address` | string | (required) | Hex address to query |

### `commputer-node send`
Send a transfer transaction.

| Flag | Type | Default | Description |
|---|---|---|---|
| `--to` | string | (required) | Recipient address (hex) |
| `--amount` | u64 | (required) | Amount in whole COMME |
| `--rpc-port` | u16 | `9944` | RPC port |

### `commputer-node verify-chain`
Verify the integrity of the local chain data.

| Flag | Type | Default | Description |
|---|---|---|---|
| `--testnet` | bool | `true` | Which data directory to verify |

### `commputer-node export-chain`
Export the chain to a JSON file.

### `commputer-node version`
Print version and protocol info.

## Protocol Constants

| Constant | Value | Location |
|---|---|---|
| `CURRENT_PROTOCOL_VERSION` | 1 | core/block.rs |
| `MAX_TRANSACTIONS_PER_BLOCK` | 500 | core/block.rs |
| `MAX_BLOCK_SIZE_BYTES` | 1,048,576 (1 MB) | core/block.rs |
| `MINIMUM_FEE` | 100,000 raw units | core/transaction.rs |
| `TOTAL_SUPPLY` | 200,000,000,000,000,000 raw | core/token.rs |
| `UNITS_PER_COMME` | 100,000,000 | core/token.rs |
| `EPOCH_DURATION_SECS` | 3,600 (1 hour) | consensus/epoch.rs |
| `FINALITY_DEPTH` | 10 blocks | storage/state.rs |
| `CHECKPOINT_INTERVAL` | 100 blocks | storage/state.rs |
| `CONSENSUS_TIMEOUT_SECS` | 30 | node/consensus_manager.rs |
| `MINIMUM_PEERS` | 2 | node/event_loop.rs |

## Environment Variables

| Variable | Description |
|---|---|
| `COMMPUTER_GPU` | Set to any value to force GPU detection as present |
| `RUST_LOG` | Override tracing log filter (e.g., `RUST_LOG=commputer_node=debug`) |

## Network Defaults

- P2P protocol: `/commputer/0.1.0`
- Transport: TCP + Noise encryption + Yamux multiplexing
- Gossipsub heartbeat: 1 second
- Idle connection timeout: 60 seconds
- Max peers: 50
- Message rate limit: per-peer, tracked per window
- Peer reputation: starts at 100, adjusted by behavior

## RocksDB Configuration

- Recovery mode: PointInTime
- Column families: `blocks`, `block_heights`, `accounts`, `meta`, `archived_accounts`
- Schema version: 1 (auto-migrated on open)
- WAL: enabled (default)
- In-memory block retention: 1000 blocks (older pruned from memory, kept in RocksDB)
