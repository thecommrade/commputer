# Commputer

A communal supercomputer built from small contributions by regular people.

## Project Structure

```
Coin/
├── src/                        # Source code (Rust L1 blockchain)
│   ├── commputer-core/         # Block, transaction, proof types
│   ├── commputer-consensus/    # Consensus mechanism
│   ├── commputer-network/      # P2P gossip + DHT networking
│   ├── commputer-proofs/       # 5 proof channel implementations
│   ├── commputer-node/         # Main node binary + CLI
│   └── commputer-validator/    # Validator logic + resource monitoring
├── protocol/                   # Protocol specification
│   ├── whitepaper/             # Public whitepaper
│   ├── tokenomics/             # Emission curves, burn mechanics, gold standard
│   └── governance/             # Governance model (when defined)
├── docs/                       # Documentation
│   ├── specs/                  # Design specifications
│   ├── research/               # Research documents
│   ├── architecture/           # Architecture diagrams and decisions
│   ├── guides/                 # Developer and contributor guides
│   ├── community/              # Community-facing content (tweets, posts)
│   └── legal/                  # Legal considerations
├── tools/                      # Development tooling, scripts
├── tests/                      # Integration and end-to-end tests
├── deploy/                     # Deployment configs, systemd units
├── assets/                     # Logos, images, brand assets
├── CLAUDE.md                   # AI assistant project rules
├── TASK_LIST.md                # Current work items
└── README.md                   # This file
```

## Quick Links

- [Whitepaper](protocol/whitepaper/WHITEPAPER.md)
- [Design Spec](docs/specs/2026-03-22-commputer-design.md)
- [Compute Market Research](docs/research/decentralized-compute-market-research.md)
- [User Psychology Research](docs/research/user-psychology-go-to-market-research.md)
