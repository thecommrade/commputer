# Contributing to Commputer

## Build Instructions

```bash
# Install Rust (stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
cd Coin/src
cargo build --workspace
```

Requirements: Rust 1.75+, RocksDB development libraries (`librocksdb-dev` on Debian/Ubuntu).

## Run Tests

```bash
cd Coin/src
cargo test --workspace
```

Individual crate tests:
```bash
cargo test -p commputer-core
cargo test -p commputer-consensus
cargo test -p commputer-storage
cargo test -p commputer-network
cargo test -p commputer-proofs
cargo test -p commputer-validator
cargo test -p commputer-node
```

## Run a Node

```bash
cargo run -p commputer-node -- run --port 9000 --rpc-port 9944
```

Create a wallet first:
```bash
cargo run -p commputer-node -- wallet create
```

## Project Philosophy

Commputer is a communal supercomputer coordinated by a blockchain. Core principles:

1. **Anti-scale by design** -- A single desktop earns more than 100 datacenter nodes combined. Warehouse operators are nerfed, not banned.
2. **Gold standard hardware ceiling** -- Maximum rewarded hardware is pegged to what ~10 grams of gold buys. You cannot spend your way into an advantage.
3. **Own it or earn it** -- Hold 33 $COMME for permanent access, or contribute a full desktop for access while online.
4. **No stake, no hash power** -- Block production weight comes from real resource contribution (Composite Resource Score), not from tokens or specialized hardware.
5. **Deflationary by design** -- Fees are burned, burst compute is burned, milestone burns remove supply permanently.

## Code Style

- Use `rustfmt` defaults (run `cargo fmt --all` before committing)
- Use `clippy` (`cargo clippy --workspace`)
- Add `///` doc comments to all public items
- Write tests for new features -- place them in `#[cfg(test)] mod tests` at the bottom of each file
- Use `thiserror` for error types, `tracing` for logging
- Prefer `checked_*` or `saturating_*` arithmetic over raw operators for token amounts
- Keep functions short; extract helpers rather than writing 100-line functions

## Crate Organization

| Crate | Purpose |
|---|---|
| `commputer-core` | Types, crypto, token math |
| `commputer-consensus` | Snowball, DAG, emission, epochs |
| `commputer-storage` | Accounts, blocks, RocksDB |
| `commputer-network` | libp2p transport, gossipsub |
| `commputer-proofs` | CPU/GPU/RAM/bandwidth/storage provers |
| `commputer-validator` | Lifecycle, compliance checking |
| `commputer-node` | Event loop, RPC server, CLI |
| `commputer-sim` | Economic simulator |

## Pull Request Process

1. Create a feature branch from `main`
2. Write tests for new functionality
3. Run `cargo test --workspace` and `cargo clippy --workspace`
4. Keep commits focused -- one logical change per commit
5. Write clear commit messages: `feat(crate): description` or `fix(crate): description`
