# Commputer North Star — whitepaper × code × prior art

**Date:** 2026-08-01 · **Status:** DRAFT for founder review · **Method:** 8 parallel recon
units + adversarial verifiers + a whole-paper coverage critic (17 agents, 145 items, 34
verdicts), each finding traced to `file:line` on the live path.

**Governing rules for this document**

1. **The whitepaper is the spec.** Where code and paper disagree, the question is "how do we
   build the paper," not "how do we water down the paper." (Founder, 2026-08-01.)
2. **Build known things first.** DONE → BUILD-NOW → BUILD-BESPOKE → **R&D LAST**.
3. **Fair means "for the people, not for the wealthy."** (Founder, verbatim.) Chain lifetime
   20–60 years.
4. **Decide per mechanism.** No standing rule that intent beats the paper's words. Each
   mechanism-level conflict gets its own founder ruling — the Decision Queue in §4.
5. **Unit of fairness = one machine, capped**, bundled with identity binding. (Ruled
   2026-08-01.) Stated as Douceur's inequality: `cost(fake identity) > NPV(its rewards)`.

---

## 1. The headline: there are two Commputers in this repo

The gap between the paper and the code is **not** mostly unbuilt work. The whitepaper's
economic model *exists in Rust, is tested, and is orphaned* — while a different, simpler
model runs the live chain.

| The paper's chain — written, tested, **zero callers** | The live chain — actually running |
|---|---|
| `AnchorSelector` picks producers by composite resource score, its own doc comment saying *"not stake, not hash power"* (`src/consensus/src/anchor.rs`) | Stake-weighted proposer schedule (`weighted_schedule.rs`, `schedule_epoch.rs`) — **live on all 3 nodes** |
| `ChannelAllocation::from_demand` splits emission across five channels with the paper's exact 10/10/10/5/5 floors (`consensus/emission.rs`, `core/proof.rs:32-40`) | One flat halving reward, **100% to the single block producer** (`state.rs:807-837`) |
| `multi_node_multiplier` — 1.0 / 0.25 / 0.0625 / 0.0156 / 0 per-operator decay (`core/compliance.rs:114-123`) | No operator concept exists anywhere; keypairs are free |
| `cap_at_reference` gold-standard ceiling + sub-linear `R^0.7` scoring (`core/token.rs:65-87`, `proof.rs:121-150`) | Nothing caps anything; stake is uncapped and purchasable |
| `NerfRate::increase_to` monotonic ratchet (`compliance.rs:34-38`) | `NerfRate` frozen at 8000 bps forever; no reward is ever multiplied by it |
| `TierAllocation::calculate` — 49% split equally among holders (`tier.rs:146-162`) | No equal division anywhere; the 49% job pool is ordered by **fee priority** |
| `AccessPath::resolve` — Earn It / Own It / emergency access (`tier.rs:52-131`) | Zero call sites; no access is granted or denied by anything |
| `HardwareBenchmark`, `DifficultyCalibrator`, `CrossChannelAnalyzer`, `FairScheduler`, `BurstPriceCalculator`, `check_milestone`, `split_fee`, `FinalityGadget`, `dag.rs`, `ommer.rs` | none of it wired |

**This is better news than it sounds and more dangerous than it looks.** Better, because much
of the paper is already written as working code and needs wiring, not invention. Dangerous,
because a reader — including a future contributor or auditor — greps the repo, finds the
paper's model, and concludes it ships. It does not.

### The fairness inversion is live and compounding

The single most important finding. Today:

- Block production is allocated **in proportion to bonded stake** (the flip, `6c28c1c`).
- The producer receives **100% of the block reward** (`credit_block_reward`, `state.rs:822-833`).
- Therefore **mining income ∝ wealth**, uncapped.
- Every §6 anti-scale mechanism is bypassed wholesale: they all key on *hardware*, and the
  live economics key on *stake*. **You do not need a warehouse. You buy COMME.**

The paper says the opposite in §7 ("split among all active validators for that epoch"). Per-node
income looks equal today **only by coincidence** — the three founder nodes hold equal bonds.
The moment stakes differ, the inversion becomes visible.

This is the precise negation of "for the people not for the wealthy," it is running right now,
and it is the thing this plan exists to fix.

---

## 2. Scorecard

145 promises extracted and traced. Buckets after verification:

| Bucket | Count | Meaning |
|---|---|---|
| DONE | 26 | delivered by code actually called on the live path |
| BUILD-NOW | 35 | not delivered; named prior art exists to borrow |
| BUILD-BESPOKE | 36 | no direct prior art; tractable with known techniques |
| R&D — LAST | 14 | genuinely unsolved anywhere |
| DOC-ERROR | 34 | the paper's statement is factually wrong as written |

**Only 14 of 145 promises are genuine research problems.** Under the "build known things
first" rule, ~71 items (BUILD-NOW + BUILD-BESPOKE) are buildable with today's techniques.

### What is genuinely DONE and good

- **The PoUW compute-job pipeline** — the best-built subsystem in the project, and the one
  place the paper *under*-claims (§9 is marked 📋 Planned while it runs live and
  consensus-enforced). Escrow into a state-root-hashed map; permissionless bonded claim race
  with `bond = max(budget, param)`; deterministic stake-weighted committee that
  **re-executes** and commit-reveals its own result hash; 85/10/5 executor/verifier/burn
  settlement with conservation checks; dispute → refund + slash with a 20% honest-verifier
  bounty; escalation second panel. This is the Truebit/iExec-PoCo family done properly.
- **WASM sandboxing** — `wasmi` pinned as consensus-critical, zero-import ABI, floats
  gated out, non-growable 64 MB memory, 100 M fuel, 10 MB I/O caps. Exactly what the
  prior-art research prescribes.
- **Fixed supply and burns** — 2 B cap enforced at the single mint site; 100% fee burn real
  on the apply path; all burn arms debit real balances.
- **Wallet** — BIP39, encrypted keystore, zeroization, first-run wiring.
- **The flip itself** — smooth weighted round-robin recomputed as a pure function from a
  2-epoch-lagged on-chain snapshot, with shadow-mode digests and byte-equivalence tests
  against the old scheduler. Deployed one node at a time without breaking quorum. The
  engineering is excellent; §4 argues the *policy* is wrong.
- **Grace period accrual** — and here the paper is **right** and an early reading of the code
  was wrong: `refill_grace` clamps to `.min(cumulative_uptime_secs)`, so steady-state accrual
  is exactly the promised 1:1 (15 days contributing = 15 days grace), with 2:1 applying only
  when refilling a drained balance back to the history cap. Paper vindicated.
- **RPC security posture** — real rate limiting; X-Forwarded-For honored only from loopback;
  CORS/security headers; all three write endpoints now await the real mempool verdict.
- **Unpromised hardening worth crediting** — libp2p key written `O_CREAT|O_EXCL` 0600 with a
  planted-symlink refusal test; decompression-bomb-safe incremental readers on all three
  request-response codecs; a live per-account mempool quota that is currently the *only*
  anti-whale enforcement running on the chain.

### The five findings that should change what we do next

1. **The fairness inversion** (§1 above). Rewards ∝ stake, uncapped.
2. **The premine is real.** `ALPHA_FAUCET_ALLOCATION = 100_000 COMME` is credited at genesis
   to a compiled founder-controlled address (`testnet_genesis.rs:92-98`, applied at
   `main.rs:472-473` and `1081-1082`). The paper says "No premine" in **three** places,
   including §11's *"Verify it yourself in the code."* The code currently verifies the
   opposite. *(The function's own doc comment claims "INERT: nothing calls this" — the comment
   is stale; the protected commit landed. Four of five recon units flagged this; the fifth
   was refuted on verification, and I confirmed the call sites by hand.)*
   It also silently truncates ~99,973 COMME of scheduled tail reward at era 14.
3. **We are in io.net's pre-collapse posture.** `auto_register_validator` and every epoch tick
   self-assert a perfect all-100s five-channel proof summary with `diversity_bonus = 50`,
   overwriting whatever the proof system measured; each node only ever challenges **itself**
   (`event_loop.rs:4739-4741`, comment: *"in a real multi-node network, challenge all known
   validators"*). io.net's 327 k registered devices collapsed to ~6,700 real — 98% fake — from
   exactly this root cause: self-reported hardware metadata.
4. **Consensus has five open critical/high defects and has forked live at least three times.**
   Quorum sized from raw libp2p sockets (QC-001); vote intake unauthenticated (QC-009); beta
   counted cumulatively rather than consecutively, so two honest nodes can finalize different
   blocks with **zero** Byzantine actors (QC-003); vote dedup keyed wrong (QC-004); self-vote
   can endorse a foreign-parent candidate (QC-005). Fork recovery is wipe-and-resync only.
   Snowball's own comment concedes its guarantees "mean nothing at this size."
5. **The transparency surfaces are fake in the one place that matters most.** The homepage
   "NERF FLOOR %" tile is a hardcoded `80` in static HTML; `/compliance` and `/anti-scale`
   return `Default` zeros forever because nothing ever writes them; and both routes are
   **admin-gated**, contradicting §10's "no login, day one." A dashboard that certifies
   0 nerfed / 0 suspicious regardless of reality is worse than no dashboard.

### Two systemic patterns to internalize

- **Dead code that looks live.** Roughly 5,000+ lines across `src/proofs/` (14 advanced
  provers), `src/consensus/` (a museum: `finality.rs`, `dag.rs`, `anchor.rs`, `optimistic.rs`,
  `ommer.rs`), `src/network/` (`gossip.rs`, `eclipse.rs`, `peer.rs`, `validation.rs`), and six
  whole node modules. Two eclipse implementations exist and **the one that could refuse a
  connection is the dead one.**
- **Doc-rot points both ways.** `executor_loop.rs`, `verifier_loop.rs`, `da_publisher.rs`, the
  rate limiters, and `alpha_genesis_accounts` all carry prominent "INERT / not wired yet"
  headers **and are live**. `src/staging/` is *not* a safe synonym for "not integrated" —
  `pouw-onchain` and `da` are real path dependencies of the live node and storage crates, and
  consensus settlement money-paths execute from there (QC-019). **Trust call chains, never
  comments or directories.**

---

## 3. The plan

### The sequencing insight that drives everything

**Chain-breaking changes are cheap right now and become permanent the day the validator set
opens.** Three founder nodes, no outside stake, faucet coins only. A reset costs a stop/wipe/
start and ~9 h of dark faucet. After strangers hold real balances, the same change costs a
governance fight or a fork.

So: **everything chain-breaking lands before the set opens.** Set-opening is the point of no
return, and it is the last gate in the plan, not the first.

### Phase 0 — Stop making false claims *(days; mostly founder-only edits)*

Nothing else can be designed against a spec that misstates the present. 34 DOC-ERRORs, the
big ones being: no-premine (three places), "✅ full anti-scale protections enforced from block
one" (zero anti-scale exists on any economic path), "✅ demand-weighted emission" (zero
callers), "✅ five proof channels verified" (self-challenged, economically inert), "block
reward is split among all active validators" (producer takes 100%), "blocks every 2 seconds"
(measured **2.884 s** — every calendar and earnings figure in §7 is ~44% optimistic), "§3 Three
rules" followed by four (and the *published* site has only three, missing the dynamic
reserve), and the era table's Years column (an era is ~5.77 years, not 4).

Also in Phase 0, three cheap code truths — **in this order**, because publishing an unwired
endpoint just makes the lie public:

1. Wire `network_stats()` into `/compliance` and `/anti-scale`, or delete the endpoints. Today
   they serve `ComplianceDashboard::default()` — permanent zeros, no writer exists repo-wide.
2. *Then* move both from the admin tier to the public tier (verified: `rpc.rs:2111-2112` sit
   inside the key-gated `admin` router, while §10 promises them with no login). One-line move.
3. Delete or wire the hardcoded `80` (verified: `src/website/index.html:91`, a static value
   presented alongside genuinely live tiles).

**Do not fix the numbers by lowering the promises.** Fix the *false statements of present
fact*. Aspirations stay, correctly marked.

**Process note:** `WHITEPAPER.md` and `src/website/*` are founder-only surfaces — agents may
not draft them even in a lane. So Phase 0's paper and site edits are founder work by
construction; agents can prepare the *evidence* (the exact line, the true number, the
`file:line` proving it) but not the diff. The three code truths above are Tier 2 lane work.

### Phase 1 — Make the chain sound *(before any stranger joins)*

QC-001 + QC-009 **together** (the ledger's verdict: fixing either alone makes attack cheaper);
QC-003/004/005; the `peer_rtts` fix (the 30 s ping publishes onto the consensus topic, whose
handler strictly deserializes `ConsensusMessage`, so **every ping is dropped and every latency
number the node reports is a structural zero**); gossipsub peer scoring (libp2p's primary
eclipse/DoS defense, left at defaults); the peer-exchange port bug (advertises hardcoded 30303
while the default is 9000, so every gossiped third-party address is undialable); NAT traversal
(relay/DCUtR registered but no reservation is ever made, so hole-punching **can never fire** —
this silently excludes the restrictive-NAT residential users the paper is written for);
`MINIMUM_PEERS = 1`. Then deterministic network simulation (**turmoil**) as the regression net.

*Decision D12 gates the ceiling here: keep bespoke Snowball, or adopt Malachite.*

### Phase 2 — Make contribution measurable *(now on the critical path — see D1)*

Paying by contribution requires contribution to be *trustworthy*. Today it is self-asserted.
With D1 ruled score-proportional, **this phase is the gate on the entire reset**: the payout
rule cannot ship on scores the earner writes themselves. The research's #1 recommendation, and
all of it is BUILD-NOW:

- **Cross-node challenges with timing profiles** for all five channels — stop self-challenging,
  stop injecting hardcoded 100s. Real hardware has a measurable performance envelope; spoofed
  hardware cannot fake the timing, and the envelope doubles as a device fingerprint.
- **Real channel proofs.** Today: CPU is iterative SHA-256 (Bitcoin's own workload — the most
  ASIC-optimized function in existence, directly undermining §6's no-ASIC claim); GPU is a
  64×64 matmul **on the CPU** with a self-reported "used GPU" flag, so a GPU-less machine
  scores full marks; storage hashes a synthetic blob derived from the node's own address, and
  the live verifier accepts *any non-empty result*; bandwidth generates data locally and
  measures no network traffic. Borrow **RandomX** (CPU, and it doubles as the timing
  substrate), **fil-proofs or Chia** (storage), **Livepeer**'s probabilistic spot-checks (GPU).
- **Vivaldi network coordinates** — ~500 lines, production-proven in HashiCorp Serf. Embed
  *now*: every epoch without it is passively-collected training data lost, and faking distance
  is self-penalizing (added latency costs bandwidth-channel rewards).
- **ASN classification** as a cheap first-pass filter, weighted **low** — residential proxies
  are a commodity industry, so IP is a filter, never a proof. *(The existing BGP-grounded CIDR
  tables are genuinely production-grade and one wiring decision from mattering.)*
- **Opt-in TPM 2.0 / Keylime** with a reward or stake-discount carrot, never a hard gate.
- Also: proof `compute_time_ms` is **filled in by the prover's own clock** and the only check
  is `!= 0`. Every timing-based detection promise currently trusts the adversary's stopwatch.

### Phase 3 — THE RESET: the fairness bundle *(one shot, chain-breaking)*

Everything that requires fresh genesis, landed together:

- **Curve D** — flat + taper + tail, as a **pinned `const [u64; 260]` integer table** (a float
  in `block_reward` is a consensus-fork risk; the taper's last step lands on an integer
  boundary and every float spelling yields *r−1*). Horizon per **D2**.
- **Delete the premine** (D4).
- **The payout rule** (**D1** — the central decision). Whatever is chosen, this is where
  "one machine, capped" stops being a principle and becomes arithmetic.
- **Channel allocation** — pick *one* canonical floor table. Four mutually inconsistent sets
  exist today: the paper and `proof.rs` say 10/10/10/5/5; `genesis.json` says 0.20×5 (summing
  to 100%, leaving **zero** float for the "self-balancing" the paper advertises);
  `ChannelWeights` defaults differ again; and `main.rs`'s genesis generator emits a fourth.
  Implement the range caps (35%/25%) that exist nowhere, including in the dead code.
- **Fee policy** (**D3**) — tail vs EIP-1559 base-burn/tips.
- Fix `revert_block` never decrementing `total_emitted` (QC-020a) before reorg is ever wired.

### Phase 4 — Open the validator set *(the point of no return)*

The biggest fairness lever in the project — roughly a 4× effect against the curve's ~1.8×.
Three pinned addresses capture **100%** of emission today, so while the pin holds, §11's "zero
advantage" is inverted: only founder nodes can earn at all, and the operator docs point
strangers at a `/rewards` endpoint promising income they cannot earn.

The open regime is already written (`consensus_set.rs`, `MIN_CONSENSUS_BOND = 1 COMME`, with
tests asserting the regimes are exclusive) — but its own comment says *"⚠ THIS IS A SPAM
FLOOR, NOT THE REAL GATE."* Per the reference library: a flat 1-COMME floor is worth 0.15 s of
block production; use time-to-earn-back denomination plus a cap, and remember **opening a set
is never a boolean flip**. Resolve **D5** first: a bonded-stake entry gate structurally
collides with §8's *"No coins needed. Ever."*

Publish the opening deadline somewhere visible — a flat curve removes the natural forcing
function to ever open.

### Phase 5 — Products (the 51%)

Storage allocations, the Humanities Archive (Arweave-style endowment economics is the named
model), the communication layer, tiers that actually unlock something. Note four parallel,
mutually inconsistent tier systems exist today and **none of them gates anything**; and the
`AccessPath` the founder chose to wire has a real bug to fix first — `PartialContributor`
*shadows* holding, so someone holding 33 COMME who also contributes 10% resolves to Base tier,
strictly worse than not contributing at all.

### R&D — LAST (14 items, explicitly deferred)

Proof-of-personhood; sustained-RAM-availability proofs; honest-bandwidth proofs from a home PC
(*"needs the most original work"* — Helium's PoC was gamed relentlessly); executor-blind
computation (§9's *"the validator cannot see the job contents"* is **architecturally
impossible** alongside commit-reveal re-execution — the paper promises two mutually exclusive
things, see D13); distributed AI training at datacenter-competitive scale; per-resource-channel
DAG consensus; the will function's outbound egress (a chain that emails people needs either a
trusted relay — a centralization point — or validator-executed egress aimed at user-supplied
addresses, an SSRF surface; neither is designed).

---

## 4. The Decision Queue

Per the "decide per mechanism" ruling, each of these needs its own founder call. Ordered by
blast radius. Each carries the paper's words, the code's reality, and the prior art.

### ✅ RULED by the founder, 2026-08-01

- **D1 — Payout rule: score-proportional, capped at the gold-standard reference machine.**
  Wire the project's own orphaned model (`AnchorSelector` selection logic + `cap_at_reference`
  + the `R^0.7` sublinearity already in `proof.rs:121-150`). This makes "one machine, capped"
  literal: a $50 k server earns what a good home desktop earns. **Hard dependency: Phase 2.**
  Paying on today's self-asserted all-100s scores would be strictly worse than stake-weighting,
  because the scores are attacker-controlled. Sequencing consequence: **Phase 2 is now on the
  critical path to the reset**, not a parallel nicety.
- **D3 — Security budget: EIP-1559 split.** Burn the base fee, pay priority tips to the
  producer. Costs nothing against the 2 B cap and creates recurring validator income
  immediately. **§7 Burn 3 must be rewritten** from "100% of transaction fees are burned" to
  "the base fee is burned; priority tips pay the producer." Note the interaction with D1: tips
  flow to the *producer*, so producer selection must already be contribution-based (D1) or the
  tip stream re-imports the wealth inversion through the back door.
- **D4 — Premine: delete it, disclose the history.** Mainnet genesis carries zero allocations;
  the alpha testnet's 100 k faucet allocation is stated plainly as testnet-only and
  non-carrying. "No premine" then verifies exactly as §11 claims. Per the tokenomics research,
  saying it loudly is itself a differentiator in 2026.

### Still open

| # | Decision | Paper says | Code does | Recommendation |
|---|---|---|---|---|
| **D2** | Curve D horizon | halvings, 4-yr | 5.77-yr eras | 40 yr (9.5% to years 1–4 vs 34.7% today) |
| **D5** | Set-opening gate | "No coins needed. Ever." (§8) | closed allowlist; designed gate needs a bond | Needs an explicit coinless entry path (earn-first probation, or a protocol-granted bond) — no code or paper section specifies one |
| **D6** | Nerf ratchet | "starts at 80%, can only increase… not governance, it is math" | frozen, unwired, and `ComplianceAppeal` is an unconditional free un-nerf | Ratchet the *ceiling* on schedule; make the *applied* penalty per-operator and evidence-weighted. Ethereum's difficulty bomb was the canonical "code not votes" ratchet and was delayed **six times** |
| **D7** | Gold recalibration | "founder publishes initially, holders vote later" | hardcoded const | Annual constant with published derivation. A live gold feed would be the worst design |
| **D8** | Governance contradiction | "code, not votes" (§6) *and* holders vote on AI/charity/gold "proportionally, like shareholders" (§5) — while §3 promises "no whale advantages" | `CharitableVote` applies as a **nonce bump that records nothing** | The paper wants wealth-weighted voting and no governance simultaneously. Resolve explicitly |
| **D9** | Burst-compute pricing unit | "tied to the gold standard" *and* "scales with demand" — two different prices | unpriced; submitter picks any burn amount | Price in internal resource units (Helium Data Credits pattern); COMME-denominated pricing breaks "never priced out" under appreciation |
| **D10** | Tier table | 5 tiers (1/5/10/20/33) | 4 tiers — no 5-COMME email tier | Pick one before either ships |
| **D11** | Sub-second latency (§9) | "targets sub-second" | 2.884 s measured; nothing targets it | Drop the claim, or accept it as a consensus replacement |
| **D12** | Consensus foundation | "custom consensus built around multi-dimensional PoW" | Snowball + stake schedule; PoW influences **zero** consensus decisions; 5 open defects | Fix-in-place vs adopt Malachite. Gates Phase 1's ceiling |
| **D13** | "The validator cannot see the job contents" | present tense, §9 | false by architecture — verifiers must re-execute | Retract or re-scope; it contradicts commit-reveal |
| **D14** | License | site + LICENSE say AGPL-3.0 | `Cargo.toml` says **MIT** | Resolve — it changes what we may vendor (an AGPL chain can use GPL code an MIT chain cannot) |
| **D15** | "Three rules… never change" | §3 says three, lists **four**; published site lists only three; homepage claims a third reserve number | formula matches the local paper | Publish principle 4 and fix the count |

---

## 5. What this means in one paragraph

The project is in better shape than a 34-error list suggests: the hardest engineering — a
working chain, a real PoUW settlement game, sandboxed execution, a fixed supply that actually
holds — is done and done well, and much of the rest is already written and merely unwired.
What is broken is the **economic spine**: the live chain pays by wealth while the paper
promises payment by contribution, the anti-scale enforcement that would make the difference
real is decorative, and the proofs that would feed it are self-attested. Those three failures
are one failure, and it has one fix, in one order: **tell the truth about today, make the chain
sound, make contribution measurable, then reset the economics — all before a single stranger
holds a coin.**
