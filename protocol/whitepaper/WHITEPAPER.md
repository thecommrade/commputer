# Commputer: A Communal Supercomputer

**$COMME — Whitepaper**

---

## Abstract

Commputer is a Layer 1 blockchain that coordinates a distributed supercomputer built from small contributions by regular people. Contributors lend idle resources — CPU, GPU, RAM, storage, bandwidth — from their existing computers and earn $COMME, a token with a fixed supply of 2 billion and no premine. The protocol enforces egalitarianism through math: scale is punished, not rewarded. A single desktop at full contribution earns maximum rewards. Warehouses and mining farms are economically destroyed by an adaptive penalty system that only gets harsher over time.

No other chain has multi-dimensional Proof of Work across five resource channels, demand-weighted emission that self-balances network composition, and anti-scale enforcement that makes datacenter mining economically suicidal. The L1 itself is the product.

This is not a meme coin. It is barely even a cryptocurrency. It is a human project that uses a blockchain because no other mechanism can trustlessly enforce fairness across millions of strangers for generations.

---

## 1. The Problem

Technology extracts from people. Cloud computing is rented, never owned. AI models were trained on the sum of human academic labor and then paywalled as subscriptions. Scientific papers funded by public money sit behind corporate gates. The compute required to participate in the modern economy is concentrated in the hands of a few companies, and access is sold back to the public at a premium.

Meanwhile, billions of computers sit mostly idle. The average desktop uses a fraction of its capacity. That unused compute, storage, and memory is wasted — not because people would not share it, but because no system exists to coordinate the sharing fairly.

## 2. The Solution

Commputer builds a supercomputer from those idle resources. Anyone with a computer can contribute a portion of their machine — as little as 2% — and earn $COMME. The protocol verifies contributions across five dimensions (CPU, GPU, storage, RAM, bandwidth) and rewards them proportionally.

The result is a communal computer that belongs to its participants. Not a company. Not a foundation. The network.

---

## 3. Core Principles

Three rules are enshrined in this whitepaper and will never change:

1. **51% of all network compute is reserved for building products that serve the users.** Shared storage, communication, AI, the Humanities Archive — whatever the network grows to support. This allocation also serves as an emergency safeguard: if the network suffers a sudden loss of resources, the 51% is deployed to protect user data — emails, photos, files — from being lost. Once capacity recovers, the 51% rebuilds to full and resumes creating. Protocol-enforced.

2. **The remaining 49% is split equally among qualifying holders per tier.** Pure equal division. No whale advantages.

3. **The free path never closes.** Contribute a full desktop and you have full access without owning a single coin. Always.

---

## 4. What Launches

Commputer launches as a working L1 blockchain. A Rust CLI node. Here is what it does differently.

### The Coin: $COMME

A Layer 1 token with a fixed supply of 2,000,000,000 earned by contributing idle computer resources. Five proof channels verified from day one: CPU, GPU, storage, RAM, and bandwidth. Burn mechanics active at launch. Full anti-scale protections enforced from block one.

### Multi-Dimensional Proof of Work

Every other PoW chain mines on a single axis. Commputer validates across five parallel channels — CPU, GPU, storage, RAM, and bandwidth — running asynchronously. A well-rounded home desktop contributing across all five channels earns a diversity bonus. Specialized farms earn less per unit than balanced machines. The protocol rewards the kind of computer real people actually own.

### Demand-Weighted Emission

Total emission per epoch is split across the five proof channels based on what the network actually needs, with guaranteed minimum floors so no channel ever goes to zero. If the network needs more storage, storage proofs pay more. If GPUs are scarce, GPU contributors earn more. The network self-balances its own resource composition without governance votes or manual intervention.

### Anti-Scale Enforcement

The ideal Commputer validator is a regular person running one desktop at home. That is who the economics favor. A second node from the same operator earns 25% per unit. A third earns 6%. A fifth earns effectively zero. Datacenter patterns are detected and penalized. The adaptive nerf starts at 80% and can only increase, never decrease. The long-term target is 100% — zero rewards for cheaters. Details in Section 6.

### Dual Burn Mechanics

Supply only goes down. Milestone burns trigger automatically when the network hits capacity thresholds. Burst compute burns let holders spend $COMME on temporary compute beyond their tier — permanently burned. Plus an annual charitable burn voted on by holders. Details in Section 7.

### P2P Networking

Gossip protocol for block propagation. DHT (Distributed Hash Table) for data storage and job routing. Both layers run simultaneously.

### Wallet

Seed phrase recovery. Your keys, your coins. The node software includes a built-in wallet with the CLI.

### The Pitch

This is a working chain with protocol properties no other chain has. Multi-dimensional Proof of Work. Demand-weighted emission that self-balances. Anti-scale enforcement that makes mining farms unprofitable by design. A gold-standard hardware ceiling tied to a universal commodity. Grace periods that account for the reality of human life. A will function for when life ends.

The L1 is the product. Everything else builds on top of it.

---

## 5. Roadmap

Everything below is the direction. Not promises with dates. Not vaporware used to pump a token price. Real goals that the network works toward as it grows.

### The Founder

The anonymous founder has no special treatment. No premine. No dev tax. No hidden wallet. No privileged access. The founder earns exactly the same way as everyone else: by running a node with the same limitations as any other single desktop, and by building on the chain like anyone else can. Zero advantage.

### Desktop App (Tauri GUI)

Commputer launches as a CLI node for people comfortable with a terminal. The desktop app comes next — single download, cross-platform, install it like any other application. Resource slider from 1% to 100%. Auto-throttles when you are using your machine. Clear dashboard showing contributions, earnings, compliance status, and grace balance. Dead simple.

### Communication Layer
- Personal email server
- Text, voice, and video communication
- No gas fees. No ads. No data harvesting.

### Personal Infrastructure
- Communal storage allocations
- Personal compute power
- AI and LLM access

### The Humanities Archive
A permanent, decentralized repository of human knowledge — academic papers, historical documents, scientific research, historically significant photographs, art, and literature. Hosted on Commputer's communal infrastructure from the flagship's 51% allocation.

**Free to anyone on earth. No login. No token. No contribution required.**

AI was trained on the sum of human academic labor and then sold back to us. Scientific knowledge sits behind paywalls. History belongs to whoever can afford access. The Humanities Archive is the answer: put it back. Permanently. On infrastructure that no one can acquire, censor, or shut down.

The stated mission: become the default repository people choose — not because they care about crypto, but because it is the most reliable, permanent, uncensorable place to store the record of human knowledge. The most obvious choice.

### Communal AI

The long-term goal of the 51% compute allocation — beyond storage, communication, and the Humanities Archive — is to train and run AI that belongs to the people who power it.

Not a company's AI. Not a government's AI. An AI trained on data the community votes to include, governed by the holders who contribute the resources that make it possible, and available to anyone the network can support.

**How governance works:**

Holders vote on the direction of the AI — what training data to use, what capabilities to prioritize, what guardrails to enforce. These votes happen on the main website alongside charitable votes. Holders vote proportionally, like shareholders in a company, except the company is a communal supercomputer and the shares are $COMME.

**The kill switch:**

If the community decides the AI is harmful — if it proves dangerous, if it's being misused, if the people who power the network simply don't want it anymore — they turn off their resource contributions and it stops. No board meeting. No corporate decision. The validators withdraw their compute and the AI goes offline. The power to create it and the power to end it live in the same hands.

**Accessibility:**

When the network is small, AI access requires holding 33 $COMME — the resources aren't sufficient to serve everyone. As the network grows and capacity increases, the threshold drops. The stated goal: eventually, anyone can use it. The timeline depends on the network, not a roadmap.

**Training data governance:**

The community votes on what data the AI is trained on. This is not a feature — it is a fundamental principle. The people who contribute the compute that trains the model decide what the model learns. No one else.

### $RAD
Details to come when the time is right.

### Ownership Tiers (Post-Launch)

As the roadmap delivers new capabilities, holding more $COMME unlocks more of the communal computer:

| Hold | Unlock |
|---|---|
| 1 $COMME | Full flagship analytics platform |
| 5 $COMME | Personal email server |
| 10 $COMME | Storage allocation |
| 20 $COMME | Processing power |
| 33 $COMME | Full personal computer + AI/LLM access |

Each tier's resources come from the 49% communal pool, split equally among qualifying holders. The math is always visible: here is the pool, here is how many people share it, here is yours.

In the beginning, your personal computer is a calculator. As the network grows, it becomes a Chromebook. Then a workstation. Then something nobody has today. The wine of technology. Patience will reward you.

---

## 6. Scale Hurts

Every other blockchain rewards scale. Commputer punishes it.

The ideal Commputer validator is a regular person running one desktop at home. That is who the economics favor.

### The Gold Standard

The reference node is not defined by fixed hardware specs. It is pegged to what **0.3225 troy ounces (0.3539 oz / 10.03 grams) of gold** would buy you in desktop hardware in 2026, measured at the median exchange rate across all available world currencies.

This means:
- The hardware ceiling evolves as technology advances. What 10 grams of gold buys today is different from what it buys in 2035.
- No one can spend their way into an advantage. The ceiling is tied to a universal, currency-neutral commodity, not to any specific hardware specification.
- As currencies fluctuate, the median measurement ensures no single economy's inflation or deflation distorts the standard.
- The protocol publishes the current gold-standard hardware profile transparently. How it is calculated, how it evolves, and what the current ceiling is — all visible, all auditable.

A desktop matching the current gold-standard profile and contributing 100% earns maximum rewards. Anything beyond that earns exponentially less.

### Anti-Scale Mechanisms

- **Exponential decay:** A second node from the same operator earns 25% per unit. A third earns 6%. A fifth earns effectively zero.
- **Diversity bonus:** Nodes contributing across all five proof channels earn a multiplier. Specialized farms earn less per unit than well-rounded home machines.
- **Hardware fingerprinting and behavioral analysis:** The protocol detects datacenter patterns — uniform hardware, flat uptime, identical configurations, near-zero inter-node latency — and flags them.
- **Resource spike detection:** Sudden jumps in claimed resources trigger verification cooldowns.
- **Network-wide concentration limit:** No single identity may represent more than 0.1% of total resources.

### Compliance, Not Punishment

If you are flagged — perhaps you added a second machine or upgraded your hardware — the protocol tells you exactly what happened and how to fix it. Fix it, and full rewards restore immediately. No probation. No scarlet letter.

For adversarial operators — warehouses, spoofed identities, sustained deception — the penalty is an 80% reward nerf. The only path back: scale down to a single compliant desktop. The protocol does not ban anyone. It makes the economics of scale so punishing that compliance is the only profitable strategy.

### The Adaptive Nerf

The nerf percentage is the one mutable variable in the protocol. It starts at 80% and **can only increase, never decrease.** As the network grows and gaming becomes more tempting, the penalty automatically gets harsher. The long-term target is 100% — zero rewards for cheaters.

This is not governance. It is math. Code, not votes.

### Environmental Design

Commputer does not create e-waste. There are no ASICs. No GPU farms. No warehouses burning electricity. The network is powered by machines that already exist, contributing resources they are not currently using. The marginal environmental cost of Commputer is near zero.

This extends to AI. Training and running large models today requires massive datacenters consuming megawatts of power, cooled by millions of gallons of water, housed in buildings that displace communities. Commputer's approach to AI eliminates the need for datacenters entirely. The compute comes from millions of ordinary machines each contributing a small slice. No new hardware manufactured. No new buildings constructed. No new power plants needed. The AI runs on what already exists — distributed, efficient, and accountable to the people who power it rather than the corporation that owns the building.

---

## 7. Tokenomics

### Supply

**2,000,000,000 $COMME. Fixed. Final.**

The maximum supply can only decrease through burns. It never increases.

### Emission — Hybrid Curve

The protocol targets ~0.09 $COMME per day per maxed reference node at launch. As the network grows, the per-node rate decays on a published, deterministic inverse square root curve:

| Network Size | Rate Per Node | Time to 33 $COMME |
|---|---|---|
| 1,000–10,000 validators | ~0.09/day | ~1 year |
| 10,000–100,000 validators | ~0.065/day | ~1.5 years |
| 100,000–1,000,000 validators | ~0.03/day | ~3 years |
| 1,000,000–10,000,000 validators | ~0.01/day (floor) | ~9 years |

The floor rate of 0.01 $COMME per day never changes. Mining always produces something. The curve is published, verifiable, and visible on the public dashboard in real time.

With 2B supply and the hybrid curve, mining stretches across 65+ years at mass adoption before burns are even factored in.

### Emergency Provisions

**Sub-1M Supply Rule:** Should total supply ever burn below 1,000,000 $COMME, any contribution — no matter how small — grants full access to the L1 and every product built on it. All L2s and dApps built on Commputer must agree to this condition before deployment. This is non-negotiable and enshrined in the protocol.

**Inactive Wallets:** Wallets that have been completely inactive for 120 years are considered nonexistent. Their coins are effectively removed from circulating supply.

**Quantum Resistance:** Should computation ever advance to the point where wallets can be breached, the full product becomes free for anyone who contributes at half the level described by the gold standard. The protocol adapts to protect users, not extract from them.

### Demand-Weighted Allocation

Total emission per epoch is split across five proof channels based on network demand, with guaranteed minimum floors:

| Channel | Floor | Range |
|---|---|---|
| Processing | 10% | 10–35% |
| GPU | 10% | 10–35% |
| Storage | 10% | 10–35% |
| RAM | 5% | 5–25% |
| Bandwidth | 5% | 5–25% |

Floors are protocol-enforced. The remaining 60% floats to where demand is highest. If the network needs more storage, storage proofs pay more. If GPUs are scarce, GPU contributors earn more. The network self-balances its own resource composition.

### Three Burn Mechanisms

**1. Milestone Burns** — Capacity milestones (total compute, storage, RAM thresholds) trigger automatic on-chain burns. Predictable, transparent, verifiable. Adoption milestones (validator counts, transaction volume) are announced as seasonal campaigns. Utility milestones (first ML job, performance benchmarks) are recognized as they emerge.

**2. Usage Burns** — Holders can spend $COMME on burst compute beyond their tier allocation. This is permanently burned. The price of burst compute in $COMME is tied to the gold standard of hardware described above for one year of usage. Pricing scales with network demand: cheap when the network has surplus, prohibitively expensive near capacity. Near capacity, the message is clear: stop buying, start recruiting validators.

**Storage protections:** Burst storage comes with a 2-year grace period to retrieve data should someone fall on hard times. All storage includes the ability to register email addresses and phone numbers that the blockchain will contact if the grace period is triggered.

**The Will Function:** In the event of a holder's death, the protocol provides customizable execution options for their stored data. Every attempt will be made to contact listed persons throughout the grace period. For those listed contacts, no payment is required to download photos, videos, media, or any personal data. This is infrastructure designed for life, not a project interested in profiting from misfortune.

**3. Annual Charitable Burn** — Once per year, holders vote on a charitable cause. The protocol sells $COMME to fund the charity and burns a matching amount.

**What it may fund** (restricted to these categories, enshrined in this whitepaper):
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
- Fund civil servants: fire, EMS, and social workers

**What it may never fund:**
- War, in any form, for any reason
- Politics, parties, campaigns, or lobbying
- Governments
- Any venture that intends to make a profit, even if it claims to be doing good

---

## 8. Two Paths to the Network

| Path | Requirement | What You Get |
|---|---|---|
| **Own It** | Hold $COMME | Access to the network and its products as tiers unlock on the roadmap. |
| **Earn It** | Dedicate 1 desktop at 100% | Full access to everything available while contributing. Same network. Same features. No coins needed. Ever. |

**The network is never priced out.** No matter what $COMME trades at, anyone with a desktop can participate for free today. This is what makes Commputer different from every token-gated project: the free path never closes.

### Grace Period

Life happens. For contributors on the "Earn It" path, the protocol provides a grace period proportional to their contribution history:

- 15 days contributing = 15 days grace
- 1 year = 1 year
- 10 years = 10 years (maximum)

Grace drains 1:1 when offline. It refills 2:1 when back online — 5 days online restores 10 days of grace. If you go offline for 10 days with 10 years banked, 5 days back online restores your full balance. Your dashboard shows the balance at all times.

If grace runs out, access stops — but **personal data (photos, music, files) is held for 10 years regardless.** Someone's family photos are not leverage. Come back anytime, plug in, and pick up where you left off.

Storage includes emergency contacts — email addresses and phone numbers the blockchain will reach out to if a grace period is triggered. In the event of death, the Will Function executes customizable instructions to ensure listed contacts can retrieve all personal data at no cost. Every attempt to reach those people will be made throughout the grace period. This is not a feature. This is infrastructure built for the reality of human life.

---

## 9. Network Architecture

### Implementation

The node software is written in Rust — the language the most battle-tested modern L1s converged on. The launch is a CLI node. The desktop GUI comes later on the roadmap.

### Layers

**Consensus:** Custom consensus built around multi-dimensional Proof of Work. Five parallel proof channels running asynchronously. Block production targets sub-second latency.

**Networking:** Gossip protocol for block propagation. DHT (Distributed Hash Table) for data storage and job routing. Both layers run simultaneously.

**Resource Orchestration:** Matches jobs to available resources. Respects the 51/49 split. Decomposes large tasks into desktop-sized pieces and reassembles results.

### Compute Jobs

The pooled resources are not theoretical. They are usable. Here is how:

**Submitting a job:** A holder spends $COMME to submit a compute job — train a model, process a dataset, run inference, render video, anything that requires compute. The spent $COMME is permanently burned. The job specification describes resource requirements (CPU, GPU, RAM, storage, bandwidth), maximum duration, and the work to be done.

**Routing:** The network matches the job to validators with the right resource profile. A GPU-intensive job routes to validators with GPUs. A storage-heavy job routes to validators with disk space. The protocol respects the 51/49 split: jobs tagged by the core development team for communal products (storage, communication, AI, the Humanities Archive) get priority access to 51% of capacity. All other jobs share the remaining 49%.

**Execution:** Jobs run in sandboxed environments on the validator's machine. The validator cannot see the job contents. The job cannot access the validator's system. Resource limits are enforced by the protocol.

**Verification:** When a job completes, random validators independently re-execute a portion of the work. If results don't match, the job is disputed. The majority result wins. Incorrect executors are penalized. Correct verifiers earn a share of the job's budget.

**Pricing:** Dynamic, based on network load. When the network has surplus capacity, jobs are cheap. When capacity is scarce, prices rise steeply. Near full capacity, the price becomes prohibitive — the protocol's way of saying "the network needs more validators, not more jobs."

**The 51% as safeguard:** In normal operation, the 51% allocation builds and powers communal products. In an emergency — sudden loss of validators, natural disaster, infrastructure failure — the 51% is redeployed to protect user data. Emails, photos, files, stored documents: the protocol prioritizes preservation over creation. Once the network recovers and capacity returns, the 51% rebuilds to full strength and resumes its role.

---

## 10. Transparency

### Public Dashboard (no login, day one)

- Total network resources — live
- Validators online
- Emission rates per channel
- Remaining supply with burns tracked in real time
- Non-compliance statistics and current nerf percentage
- Charitable donation history

### Holder Dashboard

- Your tier and unlocked features
- Exact resource allocation: total pool, number sharing it, your precise share
- Grace period balance
- No hiding. No vague numbers. The math is the product.

---

## 11. Founder

Anonymous. The L1 protocol has zero founder allocation. No premine. No dev tax. No hidden wallet. Every $COMME is earned through contribution.

The founder runs a node with the same limitations as any other single desktop. The founder earns by contributing resources and by building on the chain — the same opportunities available to every participant. Zero special treatment. Zero advantage. Verify it yourself in the code.

No VCs. No influencer deals. No paid listings. Word of mouth only. The product is the marketing.

---

## 12. What This Is

This is not a meme coin. It is almost not even a cryptocurrency. It is a human project that uses a blockchain because no other technology can trustlessly enforce egalitarian distribution at scale for generations.

This is an act of peaceful revolution — not against any company or government, but against the idea that knowledge, compute, and AI should be owned by a few and rented to the many.

Here is a working chain. Here is what it does differently: multi-dimensional Proof of Work, demand-weighted emission, anti-scale enforcement, a gold-standard hardware ceiling, grace periods for when life happens, and a will function for when it ends. Here is where we are going: a desktop app, communication tools, personal infrastructure, communal AI, and a permanent archive of human knowledge.

Scale hurts. Honesty is the default. Life happens and the protocol accounts for it. The code is the contract. And the free path never closes.

---

*Commputer. The wine of technology. Patience will reward you.*
