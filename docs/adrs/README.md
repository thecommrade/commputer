# Architecture Decision Records (ADRs)

This directory holds ADRs — short, dated records of the load-bearing
decisions that shaped Commputer. Each one captures *why* a choice was made,
not just *what* the code does. The code already tells you what.

## Where this should live

These files are currently staged at `src/staging/docs/adrs/` per the agent
sandboxing rules. The intended final home (after founder review) is
`docs/adrs/` at the repo root, alongside `ARCHITECTURE.md`.

## Index

| #    | Title                                                          | Status   |
|------|----------------------------------------------------------------|----------|
| 0000 | [ADR template](0000_template.md)                               | —        |
| 0001 | [Multi-Channel Proof of Work](0001_multi_channel_pow.md)       | Accepted |
| 0002 | [Snowball Consensus](0002_snowball_consensus.md)               | Accepted |
| 0003 | [Anti-Scale via Cloud-IP Detection](0003_anti_scale_cloud_detection.md) | Accepted |
| 0004 | [Bootstrap-Leader Topology](0004_bootstrap_leader_topology.md) | Accepted |
| 0005 | [`spawn_blocking` + mpsc for CPU-bound `select!` arms](0005_spawn_blocking_select_pattern.md) | Accepted |

## When to write a new ADR

Write one when a decision has all three properties:

1. **Load-bearing** — reversing it requires a non-trivial refactor or
   breaks user-facing properties.
2. **Non-obvious** — a future contributor reading the code alone would
   wonder "why is it like this?"
3. **Has alternatives** — there was a real choice, not a forced move.

Examples that *deserve* an ADR:
- Choosing a consensus family
- Choosing an anti-Sybil mechanism
- A non-obvious concurrency or runtime pattern
- A protocol-level constant whose value matters (block time, halving
  schedule, channel emission floors)
- Anything that contradicts a default a reasonable engineer would expect

Examples that *do not* deserve an ADR:
- Picking `serde_json` over `simd-json` (swap is local, not load-bearing)
- Renaming a struct
- Adding a feature that follows an existing pattern
- Routine performance fixes (those go in commit messages)

If you're not sure: write the ADR. They are cheap to write and free to
read.

## How to write one

1. Copy `0000_template.md` to `NNNN_short_title.md`, where `NNNN` is the
   next free number, zero-padded.
2. Fill in every section. Target ~300-500 words. Cite specific commits
   (`<short-sha>`) and `path/to/file.rs:LINE` in the References section.
3. Be honest about tradeoffs. The "Negative" and "Known Limitations"
   sections are the most valuable parts of an ADR — without them it is
   marketing copy.
4. Open a PR. Do not modify or delete previous ADRs; supersede them
   instead. (Set the old ADR's status to `Superseded by ADR-NNNN`. Old
   decisions stay in the record so future contributors can see the
   evolution.)

## Why this directory exists

Codebases lose context fast. The person who chose Snowball over
Tendermint, or who picked the cloud-IP table over a stake limit, will not
be available to answer the question in 2031. ADRs are how that
institutional memory survives staff turnover, team handoffs, and the
inevitable "wait, why are we doing it like this?" moment six months from
now.

The five ADRs in this directory are *retroactive* — they document
decisions that were already made and embedded in the code. Future ADRs
should be written *before* or *during* implementation, not after.
