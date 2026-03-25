# Changelog

All notable changes to the Commputer project.

## Features

### Core
- Add wallet key generation and signing (`5c9ca17`)
- Add BIP39 seed phrase generation and recovery (`4a2e98a`)
- Add encrypted keystore with Argon2 + AES-256-GCM (`0b10ff2`)
- Add transaction signing and verification (`6444380`)
- Add public key to transactions and verify signatures on receipt (`077cf55`)
- Add merkle roots and full block validation (`72c665d`)
- Add block size limits (`66ec321`)
- Add transaction fees -- burned, not paid to validators (`a4f5aaa`)
- Add block producer signing and verification (`fe1ac90`)
- Implement sub-linear CRS formula R^0.7 per channel (`14399e4`)
- Add shared test fixtures library (`0ead767`)
- Implement features 111-120: hardware detection, proof improvements, challenge system (`1a35ccd`)

### Consensus
- Add consensus engine and storage layer (`e495d43`)
- Implement consensus hardening features 121-135 (`1d1d7bb`)

### Storage
- Add RocksDB persistent storage -- chain survives restarts (`1f9d501`)
- Add apply_block_validated with signature checks for network blocks (`c4e6af6`)
- Add block pruning to reduce memory usage (`931a336`)
- Add RocksDB fallback for pruned block retrieval (`ad447d3`)
- Log tier changes on balance updates (`7ecffda`)
- Add account state merkle tree for state_root (`d50d365`)
- Add automatic state snapshots every 100 blocks (`dcbf382`)
- Add transaction receipt storage and RPC endpoint (`cbd5c88`)
- Add account history index (address -> tx hashes) (`1587fe9`)
- Implement advanced storage & state features 181-195 (`08677ca`)

### Network
- Add P2P network layer (`c9b2315`)
- Add libp2p transport with gossipsub + kademlia + identify (`05bdb81`)
- Add gossipsub topics and seed node connection (`d84ff39`)
- Add idle connection timeout (`7d0a745`)
- Implement advanced networking features 166-180 (`a15e596`)

### Proofs
- Add proof system: challenge generation, CPU prover, verification (`51da118`)
- Add GPU, storage, RAM, and bandwidth proof channels (`62b8cb4`)
- Enhance RAM proof with dynamic buffer sizing and timing (`a27920b`)
- Add GPU detection for proof scoring (`05ce60d`)
- Add bandwidth proof timing enforcement and scoring (`46ac8ee`)

### Validator
- Add state machine, IP compliance detection, and nerf rate tests (`2c52627`)
- Implement anti-scale enforcement features 136-150 (`23d4cec`)

### Node
- Wire up node binary with genesis block and chain status (`8d7e5ec`)
- Add main event loop with network, consensus, epoch, and block production (`5bb57d8`)
- Add CLI subcommands: wallet create/recover/show/export, status, send (`d609148`)
- Wire Snowball consensus into live network (`669453a`)
- Add mempool tx validation and auto-register as validator on startup (`b428f29`)
- Distribute mining rewards to validators at epoch boundaries (`b64742f`)
- Add ProofManager and wire proof challenges into network event loop (`9fdf778`)
- Wire compliance checker into live peer connections (`27ca012`)
- Add RPC server for transaction broadcast from CLI (`33c9332`)
- Wire peer_validators map from ValidatorRegister transactions (`ef3efd8`)
- Add bad peer handling with ban list (`df46480`)
- Add CLI peers and balance subcommands with RPC endpoints (`9188082`)
- Add mempool nonce validation and double-spend prevention (`ea1142e`)
- Add mempool size limit with fee-based eviction (`ad5bdb3`)
- Add mempool RPC, health endpoint, and version command (`af71915`)
- Add protocol handshake via identify (`05d0cef`)
- Add graceful shutdown on SIGINT/SIGTERM (`369bea4`)
- Add block explorer RPC endpoint (`c1c2341`)
- Add emission exhaustion warnings and emergency access logging (`d00706b`)
- Add block time analysis logging (`9dc9a01`)
- Add proof timeout handling (`30a13d6`)
- Add per-peer message rate limiting (`5d05eb3`)
- Add peer reputation scoring (`0532953`)
- Add connection limit (max 50 peers) (`6b62db3`)
- Add block request/response protocol for sync (`760cc5c`)
- Add initial sync protocol (`3ba2ff7`)
- Add /metrics RPC endpoint for node statistics (`aa88c44`)
- Add proof status RPC, export-chain CLI command (`2e35086`)
- Add verify-chain CLI command (`3299b34`)
- Wire grace period tracking into live node (`8cf85e3`)
- Add startup config validation and proof status RPC (`a644fc0`)
- Wire real storage proof into ProofManager (`dde0643`)
- Add timestamp validation for received blocks (`3e22cbd`)

### Simulator
- Add commputer-sim economic simulator (features 151-165) (`9b4776f`)

### Testing
- Add token arithmetic invariant tests (`573874b`)
- Add burst compute burn and grace period protocol tests (`04a825e`)
- Add two-node integration test for gossipsub block propagation (`546e643`)
- Add RPC server unit tests (`0d49dbd`)
- Add fork resolution tests (`93fa4b6`)
- Add testing & quality features 196-220 (`2b94b92`)

## Bug Fixes

- Use saturating arithmetic for supply tracking (`ceed59c`)
- Gitignore RocksDB data directories (`f3929c0`)

## Documentation

- Add RESUME.md for session continuity (`583125e`)
- Add mining rewards, validation, and network proofs plan (`f605605`)
- Simplify launch scope to L1 only (`9109d52`)
- Add launch scope spec, update whitepaper and tweets (`98233e8`)
- Address spec review: key management, bootstrap, NAT honesty (`80b58e4`)
- Update implementation plan: address all review findings (`a020626`)

## Foundation

- Initial commit: Commputer L1 foundation (`1225857`)
