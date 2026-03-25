# Commputer v0.1 Launch Scope Design

**Date:** 2026-03-24
**Status:** Approved

## Overview

Commputer launches with two things: the $COMME token and a crypto analytics platform. Everything else is roadmap. This document defines exactly what ships, what doesn't, and how the pieces connect.

## What Ships

### 1. $COMME — The Token

- **Supply:** 2,000,000,000 fixed. Only goes down.
- **Emission:** Hybrid curve. ~0.09 COMME/day per maxed desktop at launch. Inverse sqrt decay as network grows. Floor rate: 0.01 COMME/day (mining always produces something).
- **Burns:** Two mechanisms from day one:
  - **Burst compute burns:** Holders spend $COMME for temporary extra compute. Spent coins are permanently destroyed.
  - **Milestone burns:** Protocol-triggered when the network crosses capacity thresholds.
- **Proof channels:** All 5 active at launch:
  - CPU: Iterative hash puzzles (production-ready)
  - GPU: Matrix operation benchmarks (basic)
  - Storage: Chunk retrievability challenges (basic)
  - RAM: Memory-hard challenges (basic)
  - Bandwidth: Timed transfer challenges (basic)
- **Demand-weighted emission:** Per-epoch allocation across channels based on network need. Guaranteed floors (10% CPU/GPU/Storage, 5% RAM/Bandwidth).
- **Anti-scale enforcement:** Reference node ceiling (pegged to gold standard hardware), exponential decay on multi-node, adaptive nerf (80%+ for non-compliance, can only increase), compliance/restoration system.

### 2. Analytics Platform — The Product

- Crypto ML/analytics platform (signals, models, dashboards, API)
- Powered by 51% of the network's pooled compute
- **Access:** Hold 1+ $COMME → click "Analytics" in the desktop app → you're in
- The founder's L2 contribution — how the anonymous founder earns
- Improves as the network grows (more contributors = more compute = better analytics)

## The User Experience

Single flow, no complexity:

1. Download the desktop app (Windows, Mac, Linux)
2. Install it. Wallet is generated automatically.
3. Slide the resource bar (1-100%): "how much of your computer do you want to contribute?"
4. App runs in the background. User earns $COMME.
5. At 1 $COMME, the "Analytics" button activates. Click it. Full platform access.
6. Continue mining. Spend $COMME on burst compute (burns). Trade on market. Hold for future tier unlocks.

## Architecture

### Desktop App

- **Technology:** Tauri (Rust backend + web frontend)
- The Commputer node IS the app's backend
- GUI is a thin layer: resource slider, wallet display, analytics link, compliance status, network stats
- Cross-platform: Windows, Mac, Linux
- Auto-throttles when user is actively using their machine

### Node (Rust)

Seven crates, already scaffolded:

| Crate | Role | Launch Status |
|-------|------|--------------|
| `core` | Protocol types, tokens, tiers, compliance | Done |
| `consensus` | Snowstorm engine (Snowball + DAG + emission) | Done |
| `storage` | Accounts, blocks, chain state | Done |
| `network` | P2P gossip + DHT (libp2p) | Needs libp2p integration |
| `proofs` | 5 proof channel implementations | CPU done, 4 basic |
| `validator` | Validator lifecycle management | Needs implementation |
| `node` | Binary, main event loop | Skeleton done |

### Analytics Platform Integration

- Desktop app signs requests with wallet private key
- Analytics platform verifies signature, checks chain for 1+ $COMME balance
- All crypto is invisible to user — they click a button
- Platform runs on founder infrastructure, powered by 51% of pooled compute

### P2P Network

- Gossip protocol for block propagation and consensus messages
- DHT for data/storage layer and job routing
- libp2p Rust implementation
- NAT traversal for home desktops behind routers

## Protocol Rules at Launch

- **51/49 split:** 51% of network resources to analytics platform, 49% to holder allocation / burst compute
- **Reference node:** Pegged to what 10.03g of gold buys in hardware (2026 baseline, median currency). Rewards capped at this ceiling.
- **Compliance:** Incidental non-compliance → immediate nerf, immediate restoration on fix. Adversarial gaming → nerf until single-node compliant. No bans, no blacklists, just math.
- **Nerf rate:** Starts at 80%, can only increase, auto-scales with non-compliant IP count.
- **Grace period:** Contribution time = grace time (capped at 10 years). Drains 1:1 offline, refills 2:1 online.
- **Emergency access:** Below 1M circulating $COMME, any contribution grants full access.
- **Inactive wallets:** 120 years inactive = considered nonexistent.

## What Does NOT Ship at Launch

All items below are roadmap, stated honestly in the whitepaper:

| Feature | Trigger |
|---------|---------|
| Email / communication (free) | Network maturity + infrastructure |
| Storage allocation (10 $COMME) | Sufficient network storage capacity |
| Compute allocation (20 $COMME) | Sufficient network compute capacity |
| AI access (33 $COMME) | Communal AI development or pooled purchasing |
| $RAD reputation token | Community governance readiness |
| Humanities Archive | Distributed RAID-like redundancy achievable |
| On-chain charitable voting | Governance infrastructure |
| Gas-free wallets | Free tier implementation |
| Storage will function | Storage tier launch |

## Success Criteria

- Two nodes can connect, gossip blocks, and reach consensus
- A user can download the app, contribute resources, and earn $COMME
- A user holding 1 $COMME can access the analytics platform through the app
- Burns are happening (burst compute purchases)
- Supply is visibly shrinking on a public dashboard
- Anti-scale enforcement catches and nerfs multi-node operators

## Non-Goals for v0.1

- Perfecting all 5 proof channels (basic is fine, they improve over time)
- Mobile app
- Exchange listings
- Marketing of any kind
- Feature completeness on the roadmap items
