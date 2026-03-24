# Decentralized Compute Marketplaces: Comprehensive Research for Commputer

**Date**: March 22, 2026
**Purpose**: Research the economics, mechanics, successes, and failures of decentralized compute/infrastructure networks to inform Commputer's design -- a distributed supercomputer built from regular people's idle desktops, powering products that help everyone.

---

## Table of Contents

1. [Helium: Rise, Lessons, and Warnings](#1-helium-rise-lessons-and-warnings)
2. [Filecoin: Storage Economics and the Centralization Trap](#2-filecoin-storage-economics-and-the-centralization-trap)
3. [Render Network: Distributed GPU Rendering](#3-render-network-distributed-gpu-rendering)
4. [io.net: Aggregating Heterogeneous GPU Compute](#4-ionet-aggregating-heterogeneous-gpu-compute)
5. [Akash Network: The Reverse Auction Model](#5-akash-network-the-reverse-auction-model)
6. [The Cold Start Problem: How Networks Bootstrapped](#6-the-cold-start-problem-how-networks-bootstrapped)
7. [Earning Potential for Small Contributors](#7-earning-potential-for-small-contributors)
8. [What If You Gave Away the Product?](#8-what-if-you-gave-away-the-product)
9. [What Commputer Can Learn From All of This](#9-what-commputer-can-learn-from-all-of-this)

---

## 1. Helium: Rise, Lessons, and Warnings

### The Rise (2019-2023)

Helium launched in 2019 with an elegant premise: regular people buy a $200-$500 hotspot, plug it in at home, and earn HNT tokens for providing wireless IoT coverage. No technical knowledge required. The setup was genuinely simple -- download the app, plug in the device, confirm your location, start earning.

**Growth trajectory:**
- Jan 2021: ~15,000 hotspots
- July 2021: 88,000 hotspots across 8,000+ cities
- End of 2021: 450,000+ hotspots (3,114% annual growth)
- March 2022: 600,000+ hotspots
- End of 2022: ~975,000 hotspots
- Q1 2023 (peak): ~1,000,000 hotspots
- 2025: ~236,000 active hotspots (76% decline from peak)

**What drove adoption:**
- **Simplicity**: Buy device, plug in, earn tokens. The $10-40 blockchain onboarding fee was bundled into the retail price so users never saw it.
- **Early earnings were spectacular**: In 2020-2021, some operators earned thousands of dollars per month per hotspot. This created FOMO and viral word-of-mouth.
- **Hardware waitlists created scarcity psychology**: 8-month waitlists for hotspots made people want them more. People bought in bulk.
- **Community identity**: "Helium miner" became a social identity. Subreddits, Discord servers, and YouTube channels flourished.

### What Went Wrong

**The supply-demand mismatch was catastrophic.** Despite deploying nearly a million hotspots, almost nobody was actually *using* the IoT network to transmit data. The network had massive supply and near-zero demand. Hotspot operators were being paid in token inflation, not from real revenue.

**Insider token concentration.** A Forbes investigation revealed that insiders (employees, family, friends) controlled at least 30 wallets that mined 3.5+ million HNT in the first three months. Insider accounts comprised almost half of all HNT in circulation at the time, and more than a quarter of all HNT had been mined by insiders within six months -- worth roughly $250 million at peak prices. The "people's network" was making executives rich first.

**The earnings collapse was brutal.** HNT fell from $50/token (2021) to under $3 within months. Hotspot operators who bought devices at $300-$500 watched their monthly earnings drop from hundreds of dollars to single digits. By 2024-2025, a typical IoT hotspot earned $4-$8/month. Many operators couldn't even cover electricity costs.

**The pivot to 5G/Mobile.** Helium pivoted away from IoT toward mobile telecom with Helium Mobile (launching a $20/month unlimited phone plan in May 2023). The pivot to CBRS 5G spectrum was then redirected again to Wi-Fi hotspots, which integrate more easily with existing technology. This serial pivoting eroded trust.

**Token turmoil.** The MOBILE token plunged 87% from its December 2024 high after Coinbase suspended trading in June 2025.

### What Actually Worked (Eventually)

The Helium Mobile rollout changed demand-side dynamics. A real consumer product ($20/month unlimited phone plan) created actual usage. In Q4 2024, Helium offloaded 576+ TB of data -- a 555% increase over the previous quarter. The migration to Solana (April 2023) enabled the scalability needed for this growth.

5G hotspot operators in boosted urban zones report meaningful earnings: median ~$38/day ($1,142/month) for well-placed 5G radios, though these require more expensive hardware and optimal placement.

### Lessons for Commputer

1. **Supply without demand is a Ponzi dynamic.** If contributors are being paid from token inflation rather than real revenue, the system will collapse when enthusiasm wanes.
2. **Simplicity of onboarding was Helium's superpower.** "Plug in and earn" is the gold standard. Anything more complex than a Wi-Fi router setup loses the mass market.
3. **Insider enrichment destroys community trust permanently.** Even years later, the Forbes investigation haunts Helium's reputation.
4. **Serial pivoting signals a lack of product-market fit.** IoT to 5G to Wi-Fi -- each pivot lost believers.
5. **Real consumer products (Helium Mobile) saved the network.** The lesson: have a demand-side product from day one, not "build it and they will come."

---

## 2. Filecoin: Storage Economics and the Centralization Trap

### How Filecoin Prices Storage

Filecoin operates a marketplace where storage clients post deals and storage providers compete. The system uses two proof mechanisms:
- **Proof-of-Replication**: Proves data is stored uniquely
- **Proof-of-Spacetime**: Proves data remains stored over time

Providers earn through:
- Block rewards (inflationary, decreasing over time)
- Storage deal fees (from actual clients)
- Filecoin Plus verified deal multipliers (10x block reward multiplier for storing verified useful data vs. empty sectors)

### What Providers Actually Earn

Network-level revenue in Q3 2025: $792,900 in total fees (+14.3% QoQ). For context, this is the *entire network's* fee revenue split across thousands of providers. The increase was driven almost entirely by penalty fees, not storage payments.

The economics are heavily tilted toward block rewards over deal revenue, meaning providers are still primarily paid through inflation, not market demand for storage.

### The Minimum Hardware to Participate

Filecoin's hardware requirements are **enterprise-grade** -- this is not a "regular person" network:

- **CPU**: AMD Epyc (Rome/Milan/Genova) or Intel Ice Lake+ Xeon with specific instruction set extensions
- **RAM**: 1 TB for PC1 sealing operations
- **Storage**: NVMe SSDs for sealing scratch space (~450 GiB per parallel sealing process), plus massive bulk storage
- **Minimum committed storage**: 10 TiB
- **Collateral**: Substantial FIL tokens must be pledged and locked (lost if you fail uptime requirements)
- **Reference architecture** (1 PiB operation): 32+ CPU cores, 1 TB RAM, enterprise SSDs, and rack-scale storage

**Estimated total investment**: The official docs state costs are "significant and will likely require financing from investors, venture capitalists, or banks." Community estimates for a minimal competitive setup start at $50,000-$100,000+ depending on scale, not counting FIL collateral.

### How Centralized It Became

Filecoin has centralized dramatically:

- Storage capacity dropped 8% in 2025 (from 4.2 to 3.8 exbibytes) as smaller providers exited
- The Filecoin Plus (Fil+) program prioritizes large verified data clients, resulting in fewer but larger deals
- Verified storage now dominates, while small short-term regular deals have "nearly disappeared"
- The Network v25 upgrade (March 2025) raised participation standards with faster settlement requiring more responsive (read: expensive) infrastructure
- Miner consolidation is ongoing, with "many smaller operators exiting amid tighter efficiency standards and rising collateral requirements"

Filecoin is now an enterprise storage network in practice. The dream of "anyone can provide storage from home" is dead.

### Lessons for Commputer

1. **Enterprise-grade hardware requirements killed decentralization.** If regular people can't participate with what they already own, you don't have a "people's network."
2. **Collateral requirements create barriers and anxiety.** Asking people to lock up capital they might lose is a non-starter for mass adoption.
3. **The pull toward centralization is gravitational.** Every efficiency optimization and quality improvement pushes toward larger, more professional operators. This must be actively resisted by design.
4. **Block reward dependence is a ticking clock.** If your providers are earning from inflation, you need a plan for when that runs out.

---

## 3. Render Network: Distributed GPU Rendering

### How Jobs Are Distributed

Render Network connects GPU owners (node operators) with people who need rendering compute (originally 3D artists, now expanding into AI inference). The distribution system works through:

- **OctaneBench (OB) scoring**: Each node's GPU is benchmarked. Higher scores get priority for jobs.
- **Reputation system**: Job completion success, speed, and verification results build a reputation score. Between two similar-OB nodes, higher reputation wins.
- **Uptime tracking**: Measured as active time / total possible time per epoch. Nodes with 23+ hours daily uptime get priority allocation.
- **Orchestrator nodes**: Separate from rendering nodes, these assess job requirements and match them with capable node operators.

### Home GPU vs. Datacenter Earnings

The earnings gap is real but not disqualifying:

- **Consumer GPUs**: $600-$1,200/month (30-50% less than datacenter)
- **Datacenter bare metal (A100/H100)**: $2,400-$4,400/month
- **High-end consumer (RTX 5090)**: Claims of $150-$180/day at peak, processing 35-45 jobs/day (the 5090 processes ~40% faster than 4090)

**Critical caveat**: These are peak/optimal numbers. Actual earnings depend heavily on job availability, which is variable. The network migrated from Ethereum to Solana in 2023-2024, dropping transaction fees to $0.00025 (making micro-payments for small jobs viable).

### How They Handle Home Hardware Reliability

- **Reputation scoring**: Unreliable nodes get fewer jobs over time -- a natural economic punishment
- **Redundancy**: Jobs can be re-routed if a node goes offline
- **Minimum requirements**: 100 Mbps download, 75 Mbps upload bandwidth
- **Tier system**: Different job tiers match different hardware capabilities
- **The key insight**: They don't try to make home hardware as reliable as datacenters. They use the scoring system to route important/time-sensitive work to proven nodes, and less critical work to newer/less proven ones.

### Lessons for Commputer

1. **Reputation/scoring systems are the right approach to unreliable home hardware.** Don't try to guarantee reliability -- instead, let reliability build trust and earn priority.
2. **Home GPUs are genuinely useful.** The 30-50% earnings discount vs. datacenter reflects real value, not charity.
3. **Benchmarking at onboarding sets expectations.** Node operators know their score and roughly what to expect.
4. **The migration to Solana (low fees) was essential** for making small jobs economically viable. Transaction costs matter.

---

## 4. io.net: Aggregating Heterogeneous GPU Compute

### The Heterogeneity Problem

io.net's central challenge: how do you combine an RTX 3060 in someone's bedroom with an A100 in a datacenter into a useful compute cluster?

**Their approach:**
- **Ray-based distributed computing**: Uses the Ray framework (from Anyscale) for clustering, task orchestration, and parallelized workloads. Ray handles the abstraction of distributing work across different hardware.
- **Workload-appropriate routing**: GPU compute is split into training (needs high parallel FLOPS, best on A100/H100) and inference (cost-sensitive, excellent on consumer 4090s). The system routes work to appropriate hardware.
- **Heterogeneous clustering**: Different GPU types can work together on inference tasks, with the system managing the differences. Claims of 75% cost reduction for LLM inference queries via heterogeneous clusters.
- **Verification**: Each GPU is verified for capabilities at registration.

**Scale**: Grew from 60,000 verified GPUs (March 2024) to 327,000 (March 2025), with 5,350 cluster-ready across 138 countries.

### Pricing Model

- Up to 90% cheaper than traditional cloud providers
- Specific comparison: AWS charges $98.32/hr for 8-GPU H100 instance; io.net alternatives offer the same for $3.35/hr
- Consumer GPUs earn less than enterprise GPUs but participate in the inference tier where cost efficiency matters more than raw power

### Token Economics

- 500 million $IO released at TGE (June 2024)
- 300 million reserved for supplier rewards, distributed hourly over 20 years
- Disinflationary model: starts at 8% annual inflation in year one
- Suppliers are compensated even when idle (to maintain on-demand supply)
- Revenue milestone: Broke $20M in annualized on-chain revenue

### Lessons for Commputer

1. **The heterogeneity problem is solvable.** Ray-based orchestration is a proven approach. Don't try to make every machine equivalent -- route appropriate work to appropriate hardware.
2. **Inference is the sweet spot for consumer hardware.** Training needs datacenter GPUs; inference works great on gaming cards.
3. **Paying for idle time is expensive but necessary** to maintain on-demand supply. This is a bootstrapping cost.
4. **The 90% cost reduction vs. AWS is the demand-side hook.** Real savings attract real customers.

---

## 5. Akash Network: The Reverse Auction Model

### How the Reverse Auction Works

1. **Tenant creates a deployment**: Writes an SDL (Stack Definition Language) file describing needed resources (CPU, RAM, storage, GPU, bandwidth)
2. **Order broadcasts on-chain**: The deployment becomes visible to all providers
3. **Providers submit bids**: Competing on price to fulfill the request. Each bid requires a deposit (returned when bid closes).
4. **Tenant selects winner**: Can choose lowest bid or factor in provider reputation/audited attributes
5. **Lease is created**: On-chain record of the agreement. All bids recorded on-chain for transparency.

This is a true marketplace: deployers name their price ceiling, and providers compete to undercut each other.

### Does It Actually Work?

The numbers are mixed but trending positive:

**Lease activity (volatile but growing):**
- Q2 2024: 27,000 new leases (record, +44% QoQ)
- Q3 2024: 16,000 new leases (-41% QoQ)
- Q4 2024: 61,000 new leases (+274% QoQ)
- Q1 2025: 46,000 new leases (-24% QoQ)

**Revenue:**
- Q2 2024: $176,000 provider revenue (+8% QoQ)
- Q3 2024: $304,000 (+73% QoQ)
- Q4 2024: $742,000 (+144% QoQ, +565% YoY)
- Q1 2025: $1,000,000 (+38% QoQ)
- Full year 2024: ~$2.5 million across all revenue streams

**GPU pricing:**
- A100: $0.76/hr
- H200: $1.93/hr
- 1,000+ GPUs on network, 73% classified high-density (H100, A100, H200)

**Provider count**: 63 active providers as of Q3 2025 (down from 70, first contraction after multiple quarters of growth). This is a small number -- Akash is not a mass-participation network.

### Small Operator Experience

- Provider yields of 30-100%+ APY are possible with optimized pricing and uptime
- GPU-focused leases are significantly more profitable than CPU/storage leases
- Rising GPU prices and operational expenses have pushed some smaller providers out
- Average lease fee jumped from $6.42 (Q2 2024) to $18.75 (Q3 2024) as AI workloads grew
- The network plans to enable home computer participation for AI workloads in the future, but this is aspirational, not current reality

### Lessons for Commputer

1. **The reverse auction creates genuine price competition** -- it's a real market mechanism, not arbitrary pricing.
2. **63 providers is not decentralization.** Akash is more like "discount cloud hosting by small datacenter operators" than a people's network.
3. **AI workloads are driving all the growth.** CPU/storage leases are an afterthought economically.
4. **Revenue is real but small.** $1M/quarter across 63 providers is ~$16K/provider/quarter. Not bad for small datacenter operators, but not "passive income for regular people."

---

## 6. The Cold Start Problem: How Networks Bootstrapped

Every two-sided marketplace faces the chicken-and-egg problem. For compute networks: without providers, no product to sell; without customers, no revenue for providers. Here's how each network attacked it:

### Strategy 1: Token Inflation as Bootstrap (Used by All)

The universal DePIN approach: pay providers in tokens before real demand exists. The "flywheel" theory:
1. Token rewards attract providers (supply)
2. Growing supply makes the network useful
3. Usefulness attracts paying customers (demand)
4. Customer revenue creates token value
5. Rising token value attracts more providers
6. Repeat

**The problem**: Steps 3-4 rarely materialize fast enough. Most DePIN projects burn through token supply paying providers while real usage generates less than $1/month per user. When token inflation stops or token price crashes, the system collapses.

### Strategy 2: Hardware Scarcity (Helium)

Helium's 8-month waitlist for hotspots was partially intentional. Scarcity created desire and urgency. Combined with high early token rewards, this created viral demand for the *supply side* even without demand-side customers.

**Result**: Built a massive network. But the demand side never showed up for IoT, and the network was essentially a token-inflation machine for years.

### Strategy 3: Ride Existing Demand (Render)

Render targeted an existing market (3D rendering) where demand was well-understood and willing to pay. They didn't need to create demand -- they just offered a cheaper, distributed alternative to existing render farms.

**Result**: Slower growth but more sustainable. Real customers paying real money from early on.

### Strategy 4: Price Disruption (io.net, Akash)

Offer compute at 70-90% less than AWS/Azure/GCP. The demand exists (every AI startup is GPU-starved); you just need to be cheap enough to overcome the switching friction.

**Result**: Works for the demand side, but you still need to solve provider economics. If you're selling at 90% discount, how do you pay providers enough to participate?

### Strategy 5: Mission-Driven Volunteering (BOINC/Folding@home)

No financial incentive at all. Pure altruism + gamification + community.

Before COVID: ~30,000 devices running Folding@home. During COVID (April 2020): 4+ million devices -- a 133x surge. The network achieved 2.43 exaflops, exceeding the combined power of the world's top 500 supercomputers.

**What drove it**: A clear, urgent, emotionally resonant mission (fighting COVID). Points/leaderboards for gamification. Team competition. The sense that your computer was doing something meaningful while you weren't using it.

**What killed it**: Mission urgency faded. Post-COVID, participation dropped dramatically. Without sustained emotional motivation, people stop contributing.

### The Honest Truth About Cold Starts

Most DePIN projects solve the cold start by overpaying the supply side through token inflation and hoping demand catches up. The ones that survive long-term are those where:
- Real demand existed before the network (Render: rendering market; Akash: GPU-starved AI startups)
- The product was good enough that customers would pay (Helium Mobile: $20/month phone plan)
- The mission was compelling enough for free contribution (Folding@home during COVID)

Token incentives can bootstrap, but they're a loan against the future. If the future doesn't deliver paying customers, the loan defaults.

---

## 7. Earning Potential for Small Contributors

### The Honest Numbers

Here's what a regular person with a normal desktop can actually earn across these networks:

| Network | Hardware | Monthly Earning | Notes |
|---------|----------|----------------|-------|
| **Helium IoT** | $200-500 hotspot | $4-$8/month | Down from hundreds in 2021 |
| **Helium 5G** | $500-2000+ radio | $38/day median (well-placed) | Location-dependent; requires outdoor install |
| **Filecoin** | Enterprise server ($50K+) | Highly variable | Regular people cannot participate |
| **Render** | Gaming GPU (RTX 3070+) | $600-1,200/month (optimistic) | Requires 23+ hrs/day uptime; actual varies with demand |
| **io.net** | Consumer GPU | Unclear; token + USDC | Paid even when idle; specific rates not published |
| **Akash** | Server hardware | ~$16K/quarter (avg across 63 providers) | Not for home desktops currently |
| **Salad** | RTX 3060+ | $30-$200/month | RTX 3090/4090: up to $180/month; lower GPUs: $30-60 |
| **GAIMIN** | Gaming PC (4GB+ GPU min) | $30-$180/month estimate | Targets gamers specifically |

### Electricity Reality Check

A critical factor most platforms gloss over:

- **Desktop at idle**: 60-100W (~$7-12/month at US avg electricity rates)
- **GPU under compute load**: 200-450W depending on card (~$15-50/month additional)
- **Total 24/7 loaded operation**: $30-60/month in electricity for a gaming PC

For someone earning $30-60/month on Salad with a mid-range GPU, electricity eats 50-100% of earnings. Only high-end GPUs (RTX 3080+) consistently generate meaningful profit after electricity.

### The Psychological Threshold

Based on observed behavior across all these networks:

- **Under $10/month**: Not worth the mental overhead. People forget to check, then disable it.
- **$10-$30/month**: "Beer money." Some people stick with it, many don't. Not enough to change behavior.
- **$30-$100/month**: "Pays for a subscription." This is where retention starts to stick. It feels meaningful but not life-changing.
- **$100-$500/month**: "Side hustle." People actively optimize, tell friends, and stay engaged.
- **$500+/month**: "Real income." People buy dedicated hardware. This is where network effects compound.

**The uncomfortable truth**: Most regular people with normal desktops fall in the $10-$60/month range after electricity. This is below the psychological threshold where most people bother. The networks that achieved mass adoption (Helium 2021, Folding@home 2020) did so through either outsized early earnings or emotional mission -- not steady $30/month income.

---

## 8. What If You Gave Away the Product?

### The Core Question

Is there a model where compute is contributed for free (or for token rewards) and the resulting product is given away to everyone? How would this work economically?

### Precedent 1: BOINC / Folding@home (Pure Volunteer Model)

- **How it works**: People donate compute. Research results are published freely. Zero financial compensation.
- **Scale achieved**: 4 million devices during COVID, 2.43 exaflops
- **Economics**: Zero revenue, zero costs to volunteers beyond electricity. Funded by university grants and donations.
- **Why it worked**: Clear mission (cure diseases), gamification (points, teams, leaderboards), community identity, and critically -- the compute ran *when the computer was idle* so the perceived cost was near zero.
- **Why it's limited**: Participation is episodic and mission-dependent. When COVID urgency faded, participation dropped 90%+. You can't build a reliable service on volunteer compute that might disappear.

### Precedent 2: Wikipedia (Contributed Labor, Free Product)

- **How it works**: Volunteers write articles. Everyone reads for free. Funded by donations ($150M+ annual budget).
- **Scale**: 6th most visited website globally. ~100,000 active editors.
- **Economics**: 100% donation-funded. No advertising. The "product" (knowledge) is a public good.
- **Why it worked**: Strong ideological identity ("free knowledge for everyone"), clear contribution mechanism, visible impact of your contribution, community governance.
- **Limitation**: Relies on a relatively small number of dedicated contributors. Most people consume, few create.

### Precedent 3: Open Source Software (Contributed Labor, Free Product, Commercial Ecosystem)

- **How it works**: Developers write code freely. Anyone uses it. Companies build businesses on top (support, cloud hosting, enterprise features).
- **Economics**: The "free" layer creates massive value. Commercial layers extract a fraction of it. Linux is free; Red Hat (now IBM) was acquired for $34 billion.
- **Why it worked**: Contributors benefit directly (they use their own software), employers pay them to contribute, and reputation/career advancement motivates open-source work.

### Precedent 4: The Hybrid Model (Token Rewards + Free Product)

This is the unexplored territory most relevant to Commputer:

1. **Contributors donate idle compute** and receive token rewards (modest, not get-rich-quick)
2. **The aggregated compute powers a product** (AI inference, scientific computing, rendering, etc.)
3. **The product is free for end users** (or very cheap)
4. **Revenue comes from**: enterprise/API customers who need SLAs, priority access, or scale; donations/grants; token appreciation if the network grows

**The economic tension**: If the product is free, where does the value come from to pay contributors? Options:
- **Freemium**: Free tier for individuals, paid tier for businesses (like Linux/Red Hat)
- **Donation-funded**: Like Wikipedia, but compute contributors are the "editors"
- **Token appreciation**: Contributors are paid in tokens that appreciate as the network grows (this is the speculative DePIN model, and it's fragile)
- **Cross-subsidy**: A separate revenue stream (advertising, data, partnerships) funds contributor rewards
- **Altruism + token hybrid**: Most contribution is altruistic (like BOINC), but tokens provide a small bonus that makes it feel less like pure charity

### The Crucial Insight

The most successful "free product, contributed resources" models share one trait: **contributors don't feel like they're losing anything.** BOINC runs when your computer is idle. Wikipedia editing is intellectually rewarding. Open source developers build tools they personally use. The contribution doesn't feel like a sacrifice -- it feels like using something you already have (idle compute, knowledge, coding skills) for something worthwhile.

If Commputer's compute contribution runs invisibly in the background, consumes only truly idle resources, costs negligible electricity, and powers something the contributor personally values -- the economics might not need to be "worth it" financially. It just needs to not feel like a cost.

---

## 9. What Commputer Can Learn From All of This

### The Fundamental Design Choices

Based on everything above, here are the strategic insights for Commputer:

### 9.1. Don't Lead With Earnings -- Lead With Purpose

Every DePIN project that led with "earn money from your computer" eventually disappointed its contributors when earnings dropped. The projects with staying power had a *why* beyond money:

- Folding@home: "Your computer fights disease while you sleep"
- Wikipedia: "Free knowledge for humanity"
- Helium (at its best): "Build the people's network"

**For Commputer**: "Your idle computer powers [product] that helps everyone, including you" is far more sustainable than "earn $X/month from your desktop." The product must be something contributors personally use and value.

### 9.2. The Product Must Exist on Day One

Helium's catastrophic mistake was building massive supply with no demand. Commputer needs a compelling product *before* asking people to contribute compute. The product doesn't have to be huge -- but it must be real, useful, and visible.

**Best approach**: Build a useful product first (even running on centralized infrastructure). Once people love it, say "help us make this free/cheaper/better by contributing your idle compute." Now you have demand pulling supply, not supply hoping for demand.

### 9.3. The Onboarding Must Be Effortless

Helium's "plug in and earn" was its greatest achievement. Filecoin's enterprise requirements were its death as a people's network.

**Target**: Download an app, click "Start Contributing," done. No blockchain knowledge. No wallet setup (handle it invisibly). No configuration. No command line. If it takes more than 2 minutes, you've lost 80% of potential contributors.

### 9.4. Only Use Truly Idle Resources

The electricity math kills most compute-sharing economics for regular people. If running Commputer costs $30-50/month in electricity for a loaded GPU, most people won't do it regardless of rewards.

**The BOINC insight**: Use *actually idle* resources. CPU cycles when the computer is doing nothing. GPU when it's not gaming. Never compete with what the user is doing. This means:
- Detect idle state intelligently (no mouse/keyboard for X minutes, no fullscreen app, etc.)
- Back off instantly when the user needs their machine
- Consume minimal additional electricity (run at power-efficient settings, not full blast)
- The incremental electricity cost should be nearly imperceptible on their bill

### 9.5. Solve Heterogeneity With Smart Routing

io.net proved that heterogeneous hardware can work together. The key: don't try to make every machine equivalent. Instead:
- **Benchmark each machine at onboarding** (like Render's OctaneBench)
- **Route appropriate work to appropriate hardware** (inference to gaming GPUs, lightweight tasks to older machines)
- **Build a reputation/reliability system** (like Render's scoring)
- **Accept that some machines contribute more than others** and design incentives accordingly

### 9.6. The Token Question

Every DePIN project uses tokens. The honest assessment:

**Tokens help with**: bootstrapping initial supply, creating community ownership, providing flexible incentive mechanisms, building speculative interest that drives awareness.

**Tokens hurt with**: regulatory complexity, attracting mercenary participants who leave when prices drop, creating the appearance (and sometimes reality) of a Ponzi dynamic, insider enrichment optics.

**For Commputer**: If using a token, design it to avoid Helium's mistakes:
- No insider pre-mine or pre-allocation beyond transparent team vesting
- Emissions tied to actual network utility (data processed, products served) not just uptime
- Token burns from real revenue so the economics aren't purely inflationary
- Clear, public, auditable token distribution from day one

Alternatively, consider whether a token is necessary at all. Salad pays in USD (redeemable for gift cards/cash). BOINC uses points and leaderboards. A non-token model avoids massive regulatory and reputational headaches.

### 9.7. Plan for the Centralization Gradient

Every network trends toward centralization over time. Filecoin's enterprise providers squeezed out small operators. Akash has 63 providers. Even Helium's hotspot deployment concentrated in cities.

**Design against this**:
- Keep hardware requirements consumer-grade and enforce a ceiling (no datacenter bonus)
- Weight rewards to favor *number of unique contributors* over *total compute volume*
- Geographic distribution bonuses (don't let one city dominate)
- Maximum contribution caps per individual/entity to prevent whale concentration

### 9.8. The Magic Number is Zero Perceived Cost

The reason Folding@home got 4 million volunteers during COVID was not because the financial incentive was great (it was zero). It was because:
1. The mission was emotionally compelling
2. The perceived cost was zero (runs while computer is idle)
3. The setup took 2 minutes
4. You could see your contribution (points, completed work units)
5. There was a community (teams, leaderboards)

If Commputer can make contributing feel like it costs nothing (truly idle resources, invisible operation, negligible electricity impact) and powers something the contributor personally values -- the financial incentive becomes a bonus, not the reason. This is the most resilient model.

### 9.9. The Revenue Model That Might Work

Based on all the precedents:

**Layer 1 (Free)**: The product powered by contributed compute. Free for everyone. This is the mission.

**Layer 2 (Revenue)**:
- Enterprise API access with SLAs and guaranteed capacity
- Priority/premium tiers for heavy users
- Partnerships with organizations that need distributed compute
- Grants from foundations/governments for public-good applications

**Layer 3 (Contributor Rewards)**:
- Small token/cash rewards proportional to contribution (enough to be noticed, not enough to be the primary motivation)
- Gamification (leaderboards, badges, milestones)
- Community status and recognition
- Direct access to the product with contributor-only features or priority

This is the "Wikipedia + Red Hat" model: free product for everyone, revenue from enterprise layer, contributor motivation from mission + community + modest rewards.

### 9.10. The Numbers That Matter

Based on this research, here are the benchmarks Commputer should target:

| Metric | Target | Why |
|--------|--------|-----|
| Time to contribute | <2 minutes | Helium's plug-and-play success |
| Electricity cost to contributor | <$5/month incremental | Must be imperceptible |
| Minimum useful network size | 10,000 nodes | Below this, can't serve meaningful product |
| Contributor retention at 6 months | >40% | BOINC/Helium both saw massive churn |
| Real revenue (not token inflation) | >$0 from month 1 | Helium's fatal mistake was zero demand |
| Product NPS | >50 | People must love the product independent of contribution |
| Insider token allocation | <10% with full transparency | Helium's insider scandal is the cautionary tale |

---

## Sources

### Helium
- [Helium: From Hype to Fundamentals (Medium)](https://medium.com/@hilary.h.brown/from-hype-to-fundamentals-helium-depin-4bc466e868d4)
- [Case Study: Technical Deep Dive on Helium (Solana)](https://solana.com/news/case-study-helium-technical-guide)
- [Helium Network - Wikipedia](https://en.wikipedia.org/wiki/Helium_Network)
- [How Much Can You Earn with Helium Hotspots in 2025 (AMBCrypto)](https://eng.ambcrypto.com/how-much-can-you-really-earn-with-helium-hotspots-in-2025/)
- [Helium Insiders Owned Majority of Tokens (Protos)](https://protos.com/helium-insiders-owned-majority-of-crypto-tokens-forbes-reveals/)
- [Crypto Startup Helium: Executives Got Rich (Slashdot/Forbes)](https://slashdot.org/story/22/09/26/1833256/crypto-startup-helium-promised-a-peoples-network-instead-its-executives-got-rich)
- [State of Helium Q4 2024 (Messari)](https://messari.io/report/state-of-helium-q4-2024)
- [Helium Redefined: IoT Dreams to Mobile Reality (ByteTree)](https://www.bytetree.com/research/2025/12/helium-redefined-from-iot-dreams-to-mobile-reality/)
- [Helium Protocol Report (Helium Foundation)](https://www.helium.foundation/protocol-report)
- [Helium Network Expands Despite Controversies (DePIN Scan)](https://depinscan.io/news/2025-03-23/helium-network-expands-despite-token-decline-and-past-controversies)

### Filecoin
- [Filecoin ROI Documentation](https://docs.filecoin.io/storage-providers/filecoin-deals/return-on-investment)
- [Filecoin Hardware Requirements](https://docs.filecoin.io/storage-provider/hardware/hardware-requirements)
- [The Economics of Storage Providers (Filecoin Blog)](https://filecoin.io/blog/posts/the-economics-of-storage-providers/)
- [State of Filecoin Q3 2025 (Messari)](https://messari.io/report/state-of-filecoin-q3-2025)
- [State of Filecoin Q1 2025 (Messari)](https://messari.io/report/state-of-filecoin-q1-2025)
- [Five Years of Filecoin (Filecoin Foundation)](https://fil.org/blog/five-years-of-filecoin-what-we-ve-built-and-what-s-next)
- [State of Filecoin 2025 (Filecoin TLDR)](https://filecointldr.io/article/state-of-filecoin-2025)

### Render Network
- [Understanding Render Network (Messari)](https://messari.io/report/understanding-the-render-network-a-comprehensive-overview)
- [Render Network Node Operators](https://rendernetwork.com/participate-node-operators)
- [RENDER Pricing of Compute Work](https://know.rendernetwork.com/basics/how-much-does-rndr-cost)
- [Compute Client Node Reward Mechanism Update (Medium)](https://medium.com/render-token/compute-client-node-reward-mechanism-update-6b867e348030)
- [Render Network Earnings Guide (BiaCryptoTrading)](https://www.biacryptotrading.com/2026/01/earn-passive-income-gpu-render-network-2026%20.html)

### io.net
- [Understanding io.net (Messari)](https://messari.io/report/understanding-io-net-a-comprehensive-overview)
- [Building the Internet of GPUs (Multicoin Capital)](https://multicoin.capital/2024/03/05/building-the-internet-of-gpus/)
- [io.net Decentralized Computing 2025](https://io.net/blog/decentralized-computing)
- [io.net Breaks $20M in Annualized Revenue](https://io.net/blog/io-net-20m-in-annualized-on-chain-revenue)
- [IO.NET: Does It Have What It Takes? (Nansen)](https://research.nansen.ai/articles/ionet-does-it-have-what-it-takes)

### Akash Network
- [Akash Bids and Leases Documentation](https://akash.network/docs/getting-started/intro-to-akash/bids-and-leases/)
- [State of Akash Q3 2025 (Messari)](https://messari.io/report/state-of-akash-q3-2025)
- [State of Akash Q1 2025 (Messari)](https://messari.io/report/state-of-akash-q1-2025)
- [Akash Provider Earn Calculator](https://akash.network/pricing/provider-calculator/)
- [Akash Network Review 2025 (Coin Bureau)](https://coinbureau.com/review/akash-network-review)

### Cold Start & DePIN Economics
- [Challenges Facing DePIN Networks (SpotLite)](https://thespotlite.net/challenges-facing-depin-networks-why-decentralized-infrastructure-isn-t-easy)
- [Tokens to Bootstrap Network Effects (Medium)](https://ferrenbacha.medium.com/tokens-models-to-bootstrap-network-effect-in-multi-sided-platforms-the-chicken-and-egg-problem-49d1140ed652)
- [Economic Incentives in DePIN (Kaisar)](https://kaisar.io/blog/economic-incentives-in-depin/)
- [DePIN Tokenomics (Frontiers)](https://www.frontiersin.org/journals/blockchain/articles/10.3389/fbloc.2025.1644115/full)
- [Why DePIN Matters (a16z)](https://a16zcrypto.com/posts/listicles/why-depin-matters/)
- [DePIN Token Economics Report (DePINed)](https://depined.xyz/report)
- [DePIN Growth Projections 2025-2030 (Nadcab)](https://www.nadcab.com/blog/depin-growth-projections)
- [State of DePIN Sector 2025 (iExec)](https://www.iex.ec/academy/depin-sector-trends-market-cap)

### Volunteer Computing & Free Product Models
- [Folding@home - Wikipedia](https://en.wikipedia.org/wiki/Folding@home)
- [Over 4 Million Computers Joined Folding@home (HPCwire)](https://www.hpcwire.com/off-the-wire/over-4-million-computers-worldwide-joined-foldinghome-to-aid-in-coronavirus-research/)
- [Patterns of Participation in Folding@home (Citizen Science Journal)](https://theoryandpractice.citizenscienceassociation.org/articles/10.5334/cstp.109)
- [BOINC - Berkeley](https://boinc.berkeley.edu/)

### Earning Platforms
- [Salad.com - How Much Can I Earn](https://support.salad.com/article/60-how-much-can-i-earn-with-salad)
- [Salad - Distributed GPU Cloud](https://salad.com/)
- [GAIMIN Platform](https://www.gaimin.io/blog/turning-your-idle-gaming-power-into-a-money-making-tool-with-gaimin)
- [GPU Passive Income 2026 (ShareAI)](https://shareai.now/blog/insights/gpu-passive-income-rtx-4090-2025/)

### Power Consumption
- [How Much Electricity Does a Gaming PC Use (SolarTech)](https://solartechonline.com/blog/how-much-electricity-does-gaming-pc-use/)
- [PC Electricity Cost Calculator](https://computerinfobits.com/tools/hardware/pc-electricity-cost/)
