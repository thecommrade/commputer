# Commputer Glossary

## Core Roles

**Validator** — A node operator registered with the protocol to produce blocks, earn rewards, and validate proofs. Validators run the `commputer-node` software and submit `ValidatorRegister` transactions. A single person can run only one compliant validator; additional nodes face exponential reward decay (see Exponential Decay).

**Contributor** — A node operator actively running proof challenges across one or more of the five proof channels (CPU, GPU, storage, RAM, bandwidth). All validators are contributors, but the term emphasizes the resource-submission role. Contributions are measured and rewarded each epoch.

**Holder** — Anyone who owns $COMME tokens, regardless of whether they run a node. Holders vote on charitable donations, access features based on their tier, and can submit compute jobs. A holder may or may not be a validator.

## Protocol Terms

**Commputer** — A Layer 1 blockchain that coordinates a distributed supercomputer built from small contributions by regular people.

**$COMME** — The native token of the Commputer network. Fixed supply of 2,000,000,000. Used for access, ownership, compute credits, and governance.

**The Commrade** — The communal conscience of the protocol. Judges hoarders. Not a person — a principle.

**$RAD (Friend of the Rad Point)** — A non-tradeable, worthless reputation token. 1 per wallet per month. Used only for ranking and service suggestions. The people vote on enforcement via $RAD.

**Snowstorm** — Commputer's recommended consensus mechanism. Avalanche-style Snowball probabilistic sampling over a multi-channel DAG. Desktop-friendly, ~800ms-2s finality.

## Proof Channels

**Multi-Dimensional Proof of Work** — Commputer's system of five parallel proof channels, each verifying a different resource type. Replaces traditional single-axis PoW.

**Proof of Processing** — Verifies CPU cycle contributions via deterministic function execution with cryptographic proof.

**Proof of GPU** — Verifies GPU compute via memory-hard matrix operations and ML micro-benchmarks that only real GPUs can complete within the time window.

**Proof of Storage** — Verifies data held on disk via Proof of Retrievability. The network challenges random chunks at random times.

**Proof of RAM** — Verifies memory allocation via memory-hard challenges. Latency-verified to ensure RAM is genuine and not swapped to disk.

**Proof of Bandwidth** — Verifies network throughput via timed data transfer challenges between nodes.

**Proof Channel Floor** — The guaranteed minimum percentage of epoch emission allocated to each proof channel, regardless of demand weighting. Processing: 10%, GPU: 10%, Storage: 10%, RAM: 5%, Bandwidth: 5%.

## Anti-Scale Terms

**Reference Node** — The baseline hardware profile used to calibrate maximum rewards. Pegged to what 0.3225 troy ounces (10.03 grams) of gold in 2026 would buy at the median global currency value. Evolves with technology over time.

**Gold Standard** — The mechanism that pegs the reference node's hardware ceiling to the purchasing power of ~10 grams of gold. Prevents spending your way into an advantage. Measured against the median of all available currencies to account for exchange rate variance.

**Nerf** — An 80%+ reduction in rewards or capabilities applied to non-compliant validators or wallet exploiters. Not punishment — compliance enforcement through economics.

**Adaptive Nerf** — The protocol-level nerf percentage that can only increase, never decrease. Starts at 80%, auto-scales upward based on the number of non-compliant IPs detected. Long-term target: 100%.

**Exponential Decay** — The reward curve for multi-node operators. Node 1: 100%, Node 2: 25%, Node 3: 6%, Node 4: ~1.5%, Node 5+: effectively zero.

**Diversity Bonus** — A reward multiplier for validators contributing across all five proof channels. Rewards well-rounded home machines over specialized farms.

**Buffer Pool** — A reserve of compute held aside to prevent a single large node going offline from impacting the network. Shrinks toward zero as the honest user base grows.

## Tokenomics Terms

**Hybrid Curve** — Commputer's emission schedule. Starts at ~0.09 $COMME/day per maxed reference node. Adjusts downward as the network grows on a published, deterministic curve. Floor rate: 0.01/day.

**Floor Rate** — The minimum emission rate per validator per day (0.01 $COMME). Mining always produces something, no matter how large the network gets.

**Milestone Burn** — Protocol-triggered burn when the network crosses capacity, adoption, or utility thresholds. Three tiers: capacity (hardcoded), adoption (seasonal), utility (organic).

**Usage Burn** — Permanent burn of $COMME spent on burst compute beyond a holder's tier allocation. Priced dynamically based on network demand, tied to the gold standard for one year of usage.

**Annual Charitable Burn** — Once per year, holders vote on a cause. Protocol sells $COMME for the charity AND burns a matching amount. Restricted categories only.

**Demand-Weighted Emission** — The protocol's mechanism for allocating rewards across proof channels based on what the network needs, above the guaranteed floor percentages.

## Access Terms

**Own It** — Hold 33 $COMME for permanent, unconditional access to the full product. Your deed of ownership.

**Earn It** — Dedicate 1 desktop at 100% for full access while contributing. Same product, no coins needed. Access stops when contribution stops.

**Grace Balance** — A time bank for "Earn It" contributors. Equals total contribution time (max 10 years). Drains day-by-day when offline. Refills at 2:1 when back online (5 days online restores 10 days).

**Will Function** — A protocol feature allowing users to designate email addresses and phone numbers that the blockchain will contact if their storage grace period expires. Customizable execution options. In the event of death, listed persons can download all personal files at no cost.

## Holder Tiers

**Tier 1 (1 $COMME)** — Full flagship product access.

**Tier 2 (10 $COMME)** — Communal storage allocation. Comes with 2-year grace period for retrieval if holder falls on hard times.

**Tier 3 (20 $COMME)** — Communal processing power allocation.

**Tier 4 (33 $COMME)** — Full personal computer + AI/LLM access when developed.

**51/49 Split** — 51% of all network resources always serve the flagship product. 49% is split equally among qualifying holders per tier. Protocol-enforced.

## Charitable Categories

What the annual burn may fund:
- Feed the hungry
- Cure disease
- Improve the environment
- Provide healthcare
- House the houseless
- Expand mental health availability
- Rehabilitate the drug addicted and incarcerated
- Improve access to education for any person of any age
- Care for the elderly
- Fund animal shelters
- Provide assistance and accessibility for the physically or mentally disabled
- Fund civil servants (fire, EMS, social workers)

What it may **never** fund:
- War
- Politics
- Any venture that intends to make a profit, even if they claim to be doing good

## Emergency Provisions

**Sub-1M Emergency Access** — Should fewer than 1,000,000 $COMME remain in existence, any amount of contribution gains full access to the L1 and every product built on it. L2 developers must agree to this before building on Commputer.

**120-Year Wallet Expiry** — Wallets inactive for 120 years are considered nonexistent.

**Cryptographic Breach Provision** — Should computation reach a point where wallets can be breached, the product becomes free for anyone who contributes at half the level described by the gold standard.

## Architecture Terms

**DAG (Directed Acyclic Graph)** — The data structure underlying Snowstorm consensus. Allows five proof channels to produce blocks at their natural rates without bottlenecking on the slowest channel.

**Resource Orchestration Layer** — The protocol layer between the chain and actual compute work. Matches jobs to resources, respects the 51/49 split, decomposes large tasks into desktop-sized pieces.

**Gossip Protocol** — The networking layer for block propagation and consensus messages. Each node talks to a handful of peers; information propagates through the network.

**DHT (Distributed Hash Table)** — The networking layer for data storage and job routing. Nodes organize into a structured overlay so data and compute can be located efficiently.

**Humanities Archive** — A permanent, decentralized repository of human knowledge. Academic papers, history, science, art, photographs. Free to anyone. No login, no token, no contribution required. Hosted on reserved flagship compute.

**L2** — Layer 2 applications and services built on top of the Commputer L1 chain. The founder's crypto analytics platform is one such L2. The mechanism through which developers (including the founder) earn revenue.

## Implementation Terms

**Epoch** — A 1-hour (3600 second) time window over which proof scores are aggregated and emission is distributed.

**Anchor** — The validator selected to produce a block for a given round. Selected via VRF-weighted lottery based on Composite Resource Score.

**Composite Resource Score (CRS)** — A validator's aggregate score across all five proof channels, using the sub-linear R^0.7 formula with diversity bonus.

**Finality Depth** — 10 blocks. Blocks older than this cannot be reorganized.

**Checkpoint** — Every 100 blocks, a checkpoint is created that cannot be reorganized past.

**Keystore** — An AES-256-GCM encrypted file storing the wallet's seed phrase. Key derived from password via Argon2id.

**Mempool** — The pool of pending transactions waiting to be included in a block. Fee-sorted with size limits.

**State Root** — A merkle root hash of all account states, included in each block header for light client verification.

**Merkle Proof** — A cryptographic proof that a transaction is included in a block, using sibling hashes along the path from leaf to root.

**Cooldown** — A 3-epoch period of zero rewards triggered by a resource spike detection. Prevents hot-swapping hardware.

**Suspicion Score** — A 0-100 score computed from multiple anti-scale signals (subnet, fingerprint, behavior, geography). Higher means more suspicious.

**Schema Version** — RocksDB schema version number, auto-migrated on database open.

**Write Batch** — An atomic RocksDB operation that commits multiple key-value writes in a single transaction.

**Column Family** — A logical partition within RocksDB. Commputer uses: blocks, block_heights, accounts, meta, archived_accounts.

**Hot Storage** — Accounts kept in memory for fast access. Active accounts are hot.

**Cold Storage** — Accounts archived to RocksDB only (not in memory). Inactive accounts after 100 epochs.

**Archival** — Accounts with zero balance and no activity for 1000 epochs are archived to cold storage.

**State Diff** — A record of all account balance and nonce changes caused by a single block. Used for rollback and debugging.

**Retention Policy** — Configuration for how long different data types are kept. Proof results: 100 epochs. Snapshots: last 10.

**Will Event** — A notification triggered when a contributor's grace period is about to expire, alerting designated contacts.
