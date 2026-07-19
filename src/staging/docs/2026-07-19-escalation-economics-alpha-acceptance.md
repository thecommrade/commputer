# Escalation economics — ACCEPTED-FOR-ALPHA properties + pre-mainnet rebalance items

Status: founder decision 2026-07-19 — **accept and document for alpha**; rebalance is a
PRE-MAINNET work item. Source: final 3-lens adversarial review of the EscalationRound feature
(`wf_36d143b3-7af`, branch `agent-testnet-20260707` @ `4a73daa`). These are properties of the
FROZEN game math (`src/staging/pouw/src/settlement.rs` `settle_noquorum_confirmed/disputed`),
implemented faithfully on-chain (pinned by `golden_full_panel_matches_frozen_escalation_resolve`)
— NOT implementation defects. The whitepaper note, if any, is founder-only (protected file).

## Property 1 — Panel incentive inversion

A panel member's bond is returned on EVERY terminal (the frozen settle functions never partition
the PANEL's reveals — only the round-1 committee's), and the panel reward pool differs by ~4
orders of magnitude across terminals:

| Terminal | Panel pool source | Pool @ default params (budget=Be=1,000,000; Bv=20; 3-member round-1 committee) | Per-panelist (6-seat panel) |
|---|---|---|---|
| Confirmed | 10% of SLASHED round-1 committee bonds | bps(60, 1000) = **6** | **+1** |
| Bounded NoQuorum | 10% of the SLASHED executor bond | bps(1,000,000, 1000) = **100,000** | **+16,666** |

Consequence: a rational panelist — and any panel minority large enough to deny quorum (3 of 7, or
1 of a gate-minimum 5) — profits ~16,000× by abstaining/splitting to force the punitive terminal,
at zero personal cost (its bond returns either way). Honest confirmation is strictly dominated
once real value rides escalations. The punitive terminal itself is the founder-approved
anti-griefing deterrent (F5/F6); the inversion is an emergent side-effect of funding the panel
premium from slashed bonds.

**Alpha acceptance rationale:** stakes are testnet-only; escalations require a round-1 NoQuorum
(rare among honest actors); the validator set at alpha is small/known.

**Pre-mainnet rebalance options (pick at the frozen-crate revision):**
- R1: partition PANEL reveals like committee reveals — wrong-side/abstaining panelists forfeit
  their bond (removes the zero-cost defection; symmetric with §332 rubber-stamp forfeiture).
- R2: fund the Confirmed panel premium from the job budget (or a protocol pot) instead of slashed
  committee bonds, sizing it ≥ the NoQuorum premium so honest confirmation dominates.
- R3: cap the NoQuorum panel pool (e.g. min(10% Be, k·Bv·multiple)) so denial never out-pays work.

## Property 2 — DA unavailability funnels a viable panel into the punitive terminal

Panel members who cannot fetch the blob silently abstain (`verifier_loop.rs` fetch-fail →
continue; indistinguishable from voluntary abstention). A viable-at-open panel with sub-quorum
reveals settles `Verdict::NoQuorum` → bounded terminal: the HONEST executor's bond is burned
(900,004 of it at defaults — 10% of Be goes to any panel revealers; with zero revealers the full
Be burns) and the whole round-1 committee is slashed. Pre-feature, the identical outage cost
nothing (zero-comp refund). The frozen staging game HAS an `Unavailable` 100%-refund arm
(`Trigger`-level, per the 2026-06-13 settlement cycle) that is NOT wired on-chain — on-chain DA
health is unobservable to consensus, so there is currently no deterministic signal to route it.

**Alpha acceptance rationale:** the F2 gate already refunds structurally-short panels; the DA
layer at alpha is small and operator-observed; the known ~8-peer discovery ceiling (see
[[session_20260718_payout_live]] follow-ups) must be fixed before the validator set grows
regardless, and escalation (up to 7 extra fetchers per job) raises its priority.

**Pre-mainnet options:**
- U1: fix the DA discovery ceiling first (raise `max_attempts_per_chunk`, HasChunk pre-filter,
  fetcher re-store+advertise) — shrinks the outage window that triggers this.
- U2: an on-chain availability signal (e.g. panel members can post a bonded "Unavailable" vote;
  quorum of Unavailable → refund terminal instead of punitive) — consensus design work.
- U3: soften the zero-reveal case only: if NO panelist commits at all, treat as structural
  (fallback refund) rather than behavioral — a one-line deterministic distinction at settle
  (commitments.is_empty()), worth considering even for alpha if outages prove common.

## Related accepted-for-alpha notes (same review, no action now)
- Claim-time candidate snapshots can seat since-unbonded validators on panels (round-1 parity;
  stale seats count toward the F2 gate).
- `DaStore::gc` retention doc must mention escalation rounds before gc is ever wired (a
  doc-contract fix owed whenever gc wiring is scoped).
- C7 ingress admits doomed ClaimJob/CompleteJob txs naming escalation-only jobs (mempool noise,
  dropped at selection).
