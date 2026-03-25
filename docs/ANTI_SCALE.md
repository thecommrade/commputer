# Commputer Anti-Scale Mechanisms

Commputer is designed so that a single honest desktop earns more than any number of datacenter nodes. This document details every anti-scale mechanism, its rationale, and how it is verified.

## 1. Sub-Linear Resource Scoring (R^0.7)

**What**: Each proof channel score is raised to the power 0.7 before summing into the Composite Resource Score.

**Why**: Doubling hardware gives only ~1.62x the score, not 2x. This makes scaling hardware increasingly unprofitable.

**Verification**: `EpochProofSummary::composite_score()` in `core/proof.rs` applies the formula. Tests in `core/proof.rs` verify the math.

**Example**: Score 100 on one channel = 100^0.7 = 25.1. Score 200 = 200^0.7 = 38.1 (not 50.2).

## 2. Multi-Node Exponential Decay

**What**: Rewards per node drop exponentially when an operator runs multiple nodes.

| Node | Multiplier |
|---|---|
| 1 | 100% |
| 2 | 25% |
| 3 | 6.25% |
| 4 | 1.5625% |
| 5+ | 0% |

**Why**: Makes multi-node operations unprofitable. Total reward for all nodes converges to ~132.8% of a single node -- but with 80%+ nerf applied, total drops below a single honest node.

**Verification**: `compliance::multi_node_multiplier()` in `core/compliance.rs`. Test `feature_199_anti_scale_100_validators_same_subnet` proves 100 nerfed nodes earn less than 1 honest.

## 3. Adaptive Nerf Rate (80-100%)

**What**: Non-compliant validators have rewards reduced by 80% or more. The nerf percentage auto-scales upward.

**Formula**: `8000 + (nerfed_ratio * 2000)` basis points, where nerfed_ratio = nerfed_count / total_validators.

**Why**: As more cheaters are detected, the penalty increases. The rate can only go up, never down.

**Verification**: `NerfRate::compute_adaptive()` in `core/compliance.rs`. The `increase_to()` method enforces monotonic increase.

## 4. IP-Based Colocation Detection

**What**: Validators sharing network proximity are flagged.

**Checks**:
- Same exact IP -> NerfedIncidental
- Same /24 subnet -> NerfedIncidental
- Same /16 subnet -> NerfedIncidental
- Same ASN -> NerfedIncidental
- >3 validators behind same IP -> NerfedAdversarial (VPN/proxy)

**Why**: Residential users have unique IPs. Datacenter/VPN operators share subnets.

**Verification**: `ComplianceChecker::check()` in `validator/compliance_check.rs`. Tests cover all detection paths.

## 5. Hardware Fingerprint Deduplication

**What**: Each validator reports a hardware fingerprint (CPU model, cores, RAM, GPU, storage, OS). Duplicate fingerprints across nodes trigger NerfedAdversarial.

**Why**: Identical hardware profiles across different "nodes" indicate cloning/VMs.

**Verification**: `ComplianceChecker::register_fingerprint()` in `validator/compliance_check.rs`.

## 6. Datacenter IP Detection

**What**: Known datacenter IP ranges (AWS, GCP, Azure, Hetzner, OVH, DigitalOcean) are detected and flagged as NerfedIncidental.

**Ranges checked**: First octet matching AWS (3, 13, 18, 34, 35, 52, 54), GCP (104.196, 104.199), Azure (20, 40), Hetzner (88.198, 78.46, 148.251, 176.9, 46.4, 5.9), OVH (51, 54.36, 87.98, 91.121, 149.202), DigitalOcean (64.225, 104.131, 128.199, 167.71, 167.172).

**Why**: Commputer is for desktops, not datacenters.

**Verification**: `ComplianceChecker::is_datacenter_ip()` with tests for known ranges.

## 7. Behavioral Analysis

**What**: Validator behavior profiles are analyzed for datacenter patterns.

**Flags**:
- Uptime > 99.5% -> datacenter pattern (desktops get rebooted, lose power, etc.)
- Resource variance < 0.01 -> flat resource curve (desktops have natural variance)

**Why**: Real desktops show human-scale variability. Datacenters are unnaturally stable.

**Verification**: `BehaviorProfile::is_datacenter_pattern()` and `is_flat_resource()` in `validator/compliance_check.rs`.

## 8. Resource Spike Detection

**What**: If RAM or CPU reported capacity jumps by >100% between reports, the validator enters a 3-epoch cooldown (zero rewards).

**Why**: Legitimate hardware changes are gradual. Sudden spikes suggest hot-swapping or VM scaling.

**Verification**: `ComplianceChecker::report_resources()` tracks previous values and triggers cooldown via `cooldown_until`.

## 9. Gold Standard Hardware Ceiling

**What**: The reference node hardware ceiling is pegged to what ~10 grams of gold (0.3225 troy ounces) buys at the median global currency value.

**Why**: Prevents spending your way into an advantage. The ceiling evolves with technology over time, but is always anchored to a physical commodity.

**Verification**: `EmissionSchedule::per_validator_daily_rate()` returns 0.09 COMME/day for the reference node. Test `feature_217_gold_standard_reference_node` verifies ~33 COMME/year.

## 10. Diversity Bonus

**What**: Validators contributing across all 5 proof channels receive up to 25% bonus to their CRS.

**Why**: Rewards well-rounded home machines (which naturally have CPU, RAM, storage, bandwidth) over specialized GPU farms or storage silos.

**Verification**: `EpochProofSummary::composite_score()` applies `diversity_bonus / 200.0` as a multiplier.

## 11. Sybil Suspicion Scoring

**What**: Each validator gets a suspicion score (0-100) based on multiple signals:
- +25 for same /24 subnet as another node
- +25 for duplicate hardware fingerprint
- +25 for datacenter behavioral pattern
- +25 for geographic proximity (same /16 or same ASN or datacenter IP)

**Why**: Composite scoring catches sophisticated attackers who evade any single check.

**Verification**: `ComplianceChecker::suspicion_score()` in `validator/compliance_check.rs`.

## 12. Compliance History and Trust Whitelist

**What**: Compliance status changes are recorded per-validator. After 720 consecutive clean epochs (30 days), a validator becomes "trusted" and gets reduced scrutiny.

**Why**: Long-term honest participants should not be harassed by false positives.

**Verification**: `ComplianceChecker::is_trusted()` checks `first_clean_epoch` against a 720-epoch threshold.

## 13. GPU Fallback Scoring Cap

**What**: GPU proof responses include a flag byte indicating whether a real GPU was used. CPU fallback responses are capped at score 50.

**Why**: Prevents CPU-only farms from claiming full GPU channel rewards.

**Verification**: `GpuProver::used_cpu_fallback()` checks the flag byte. Score capping in `ProofManager::finalize_epoch_with_difficulty()`.

## 14. Proof Timing Enforcement

**What**: Suspiciously fast proof responses are flagged as `ProofVerdict::Suspicious` (score capped at 50). Zero-time responses for large challenges are rejected outright.

**Why**: Prevents pre-computation or lookup table attacks.

**Verification**: `ProofVerifier::is_timing_suspicious()` and RAM/bandwidth verifier timing checks.

## Anti-Scale Dashboard

The `/anti-scale` RPC endpoint exposes:
- Total warehouse detections
- Total nerfed rewards (raw units)
- Nerf percentage history (epoch, bps)
- Largest detected clusters (size, IP)

Monitor these metrics to observe anti-scale enforcement in action.
