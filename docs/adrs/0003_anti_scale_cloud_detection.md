# ADR-0003: Anti-Scale via Cloud-IP Detection

## Status

Accepted, with known limitations documented below. Active in `validator`
crate; the auto-flag path is the active anti-scale enforcement at the
network layer.

## Context

Whitepaper §6 ("Scale Hurts") commits the protocol to actively *punish*
scale: a regular person on one home desktop is the ideal validator;
warehouses, cloud farms, and ASIC-style deployments must be made
economically unprofitable. The whitepaper enumerates several mechanisms —
exponential per-operator decay, diversity bonus, hardware fingerprinting,
behavioral analysis, network-wide concentration limits, and the adaptive
nerf — and we implement most of them in `src/validator/src/compliance_check.rs`.

The specific question this ADR answers: how do we cheaply detect, *at
registration time*, the most common adversarial pattern — someone spinning
up validators on AWS / GCP / Azure / Hetzner / OVH / DigitalOcean? Stake
limits don't apply (this is PoW). VRF-based selection doesn't help
(rewards, not block production, are what we need to discourage). Hardware
fingerprinting is implemented but expensive to verify and adversarially
brittle.

## Decision

Maintain a hard-coded table of cloud-provider IPv4 prefixes and flag any
validator whose reported IP falls inside one as `NerfedIncidental`
immediately upon registration:

```rust
// src/validator/src/compliance_check.rs:292-352
pub fn is_datacenter_ip(ip: &str) -> bool {
    // AWS:           3, 13, 18, 34, 35, 52, 54
    // GCP:           34, 35 (overlap), 104.196, 104.199
    // Azure:         20, 40
    // Hetzner:       88.198, 78.46, 148.251, 176.9, 46.4, 5.9
    // OVH:           51, 54.36, 87.98, 91.121, 149.202
    // DigitalOcean:  64.225, 104.131, 128.199, 167.71, 167.172
}
```

Used at `compliance_check.rs:462-463`: any cloud-IP validator skips the
clean-status check entirely and is permanently nerfed until they move off
the cloud range.

## Consequences

### Positive

- O(1) check, no external dependencies, no oracle, no on-chain state.
- Catches the lazy-adversary case: someone who reads the docs and spins up
  a fleet of `t3.micro` instances gets nerfed before their first epoch
  pays out. The economics fail before they invest serious money.
- Aligns with the whitepaper principle that the home-desktop validator is
  the protected class. Cloud VPS deployment is *not* a legitimate path.

### Negative

- Cloud IP allocations change. Our table is a 2026 snapshot; AWS will
  buy new ranges, OVH will rotate prefixes, and our table will rot.
- We currently nerf legitimate users on residential ISPs that happen to
  share a /8 with a cloud provider. False positives are silent — the
  validator simply earns 80% less and may not notice.
- The check is unilateral. There is no on-chain governance for adding /
  removing prefixes; updating the table requires a binary release.

### Known Limitations

This is the most important section of this ADR. The cloud-IP check is
*easily circumvented*:

1. **Bare-metal cloud** (Hetzner dedicated, OVH SoYouStart, AWS
   Outposts) often allocates IPs outside the listed prefixes. A
   determined adversary rents bare-metal and is invisible to this check.
2. **NAT'd home connections** — most residential ISPs put many customers
   behind a single CGNAT IP. Multiple legitimate validators behind one
   ISP NAT IP look like one Sybil cluster (caught separately by
   `is_vpn_proxy` at `compliance_check.rs:360`).
3. **VPN exit on a residential ISP**: a cloud-hosted validator routing
   outbound traffic through a residential VPN sees a residential IP. The
   check passes.
4. **Cloud VPS deployment is silently bricked.** This is documented
   external knowledge: legitimate users who try to run a node on a $5/mo
   VPS get auto-nerfed and the protocol does not currently surface a
   helpful error explaining why. (See project memory:
   `project_cloud_ip_nerf.md`.)

The cloud-IP table is the cheapest, most legible *first line* of
anti-scale defense. The real defense is the combination with: hardware
fingerprinting (`compliance_check.rs:136`), behavioral analysis (uptime
ratio, resource variance — `BehaviorProfile` at line 27), per-IP and
per-subnet caps (`is_vpn_proxy`), and the exponential per-operator decay
documented in the whitepaper. No single check is load-bearing.

## Alternatives Considered

- **Stake limits.** Rejected: this is a PoW chain; no stake to limit.
- **VRF-based block-production lottery.** Rejected: VRFs limit block
  *production*, not *rewards*. The whitepaper's anti-scale property is
  about reward economics, not turn-taking.
- **Live cloud-IP feed via oracle.** Rejected for v1: introduces an
  external trust dependency and an attack vector. May revisit for v2.
- **Pure behavioral detection (no IP table).** Considered, kept as
  secondary signal. Rejected as primary because behavioral signals
  require weeks of history; the cloud-IP check works on epoch 1.

## References

- `src/validator/src/compliance_check.rs:291-352` (the IP table)
- `src/validator/src/compliance_check.rs:460-505` (use-site)
- Whitepaper §6 "Scale Hurts" / "Anti-Scale Mechanisms"
- Project memory: `project_cloud_ip_nerf.md`
- Related: ADR-0001 (multi-channel PoW makes single-axis cloud farming
  uneconomical even when the IP check is bypassed)
