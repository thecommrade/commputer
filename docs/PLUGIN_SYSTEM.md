# Commputer Plugin System Design

## Overview

The Commputer plugin system allows L2s, dApps, and external services to interface
with the Commputer blockchain without modifying the core node software.

## Architecture

### Plugin Interface

Plugins communicate with the node via three mechanisms:

1. **JSON-RPC over HTTP** (existing `/tx`, `/status`, `/block/{height}` endpoints)
2. **WebSocket Event Subscriptions** (`/ws` endpoint for real-time events)
3. **Unix Domain Socket IPC** (future: for co-located plugins with lower latency)

### Event Subscriptions

Plugins connect via WebSocket to `/ws` and receive real-time JSON events:

- `new_block` — emitted when a block is finalized (includes height, hash, tx_count)
- `new_transaction` — emitted when a transaction enters the mempool
- `epoch_change` — emitted when a new epoch begins
- `validator_registered` — emitted when a new validator registers

### State Queries

Plugins query chain state via existing RPC endpoints:

- `GET /status` — chain height, supply, epoch
- `GET /balance/{address}` — account info
- `GET /block/{height}` — full block data
- `GET /receipt/{tx_hash}` — transaction receipt
- `GET /mempool` — pending transactions
- `GET /metrics` — node metrics

### Transaction Submission

Plugins submit transactions via `POST /tx` with a signed transaction JSON body.

### Plugin Lifecycle

1. Plugin registers with the node by connecting to the WebSocket endpoint
2. Plugin subscribes to relevant event types
3. Plugin queries state as needed via RPC
4. Plugin submits transactions as needed
5. Plugin disconnects gracefully

## L2 Integration

L2 rollups can use the plugin system to:

- Monitor L1 blocks for anchor transactions
- Submit rollup state roots as L1 transactions
- Query L1 state for bridge operations
- Subscribe to L1 events for cross-chain messaging

### Recommended Pattern

```
L2 Sequencer -> WebSocket(/ws) -> Listen for anchor blocks
L2 Sequencer -> POST /tx -> Submit state root commits
L2 Prover   -> GET /block/{height} -> Verify L1 state
```

## dApp Integration

dApps interface via the same RPC/WebSocket endpoints:

- **DeFi**: Query balances, submit transfers, monitor confirmations
- **NFT/Storage**: Submit StorageWill transactions, query proof status
- **Governance**: Submit CharitableVote transactions, query vote results

## Security Considerations

- Plugins have read-only access to chain state via RPC
- Write access requires signed transactions (no privileged plugin API)
- WebSocket connections are unauthenticated (information is public)
- Rate limiting applies to all RPC endpoints
- Plugins cannot modify consensus or block production

## Future Extensions

- Plugin registry with versioned APIs
- Plugin-specific storage namespaces
- Cross-plugin messaging bus
- Sandboxed WASM plugin execution
- Plugin marketplace and discovery
