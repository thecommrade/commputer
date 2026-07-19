# Alpha payload merge plan — `agent-testnet-20260707` → `main`

Status: DRAFT FOR FOUNDER CONFIRMATION (per CLAUDE.md, agent branches merge only on founder
request + explicit confirmation at execution time; this document is the plan half). Prepared
2026-07-19 at branch HEAD `022f7ba`+ (go-live batch Tasks A–F in flight; the ACTUAL merge
candidate is the batch-gated HEAD — re-verify every item below at that exact commit).

## 1. Shape and scope

- **Mechanics: fast-forward only.** `git merge-base main agent-testnet-20260707` == `main`
  (`1251133`, the PoUW flip) — verified. Command: `git checkout main && git merge --ff-only
  agent-testnet-20260707`. No merge commit, no conflict surface, identical trees. Matches the
  flip-merge precedent.
- **Size at draft time:** 40 commits, 68 files, +20,627/−447 (will grow with batch Tasks B–F and
  the approved chain-id hunks; re-run `git rev-list --count main..HEAD` at execution).
- **NO push anywhere.** Local merge only; publication happens via the scrub/airlock, never here.

## 2. What the payload delivers (by theme)

1. **Testnet hardening + enforcement (daca2fd batch era):** protected enforcement/security batch,
   chain-id `commputer-testnet-2`, ENFORCE flip, faucet dispenser substrate, aggregator/decay.
2. **Track-2 PoUW actors:** executor/verifier/DA-attestation loops + protected DA activation —
   payout REACHABLE (`386bdcd`, `bd6053e`), e2e proof (`b4e890b`, `6b7c39e`), smoke harness +
   `commputer bond` (`f4e8357`), §332 wrong-side forfeiture (`2b957a0`).
3. **Live-payout fixes (2026-07-18):** DA discovery + publisher self-fetch (protected, approved)
   + claim-race nonce no-op (`4d4e17a`); sync-watchdog test re-pin (`9f5d76a`).
4. **EscalationRound on-chain (2026-07-19):** the full 2nd-panel feature, 13 commits
   (`c867082..4a73daa` + protected `1e71920`), F2 viability gate, golden-oracle + B10 + e2e
   proofs; economics acceptance doc (`e9c86cb`); retransmission no-op wedge fix (`1dcc028`).
5. **Go-live batch (in flight):** seed DNS dialing, DA ceiling+pacing, Windows fix, release
   packaging (macOS/Windows), chain-id `-3` prep, ops-docs truth-pass.

## 3. Protected-file audit trail (4 files change in the range — each founder-approved)

| File | Changed by | Approval record |
|---|---|---|
| `src/node/src/event_loop.rs` | daca2fd batch; 2026-07-18 DA fixes (`4d4e17a`); 2026-07-19 escalation hunks (`1e71920`) | §2 protected-batch artifact; session approvals 07-18 (FindProviders/FetchChunk) and 07-19 (C7 + snapshot) |
| `src/node/src/main.rs` | daca2fd batch; Track-2 Phase B (`bd6053e`) | protected-batch + Phase B approvals |
| `src/node/src/config.rs` | daca2fd batch (chain-id `-2` etc.); PENDING `-3` hunk | batch approval; `-3` hunk awaits approval (Task E) — merge AFTER it lands or without it (founder choice at execution) |
| `genesis.json` | daca2fd batch; PENDING `-3` line | same as above |

`CLAUDE.md` working-tree WIP is UNCOMMITTED and is not part of any commit — it does not merge.
`.claude/` is untracked. Frozen `src/staging/pouw/`: byte-identical across the entire range
(re-verify: `git diff main..HEAD --stat -- src/staging/pouw/` must be empty).

## 4. Pre-merge checklist (ALL at the exact merge-candidate commit)

1. Go-live batch Tasks A–F complete, each task-reviewed.
2. Protected chain-id `-3` hunks approved + applied (or founder explicitly defers `-3`).
3. Faucet address pasted + rebuilt IF the founder wants the faucet in the merge payload
   (recommended — it gates the ship binary anyway; can also land as a follow-up commit on main).
4. `cargo test --workspace` exit 0 at the candidate; frozen-crate diff empty; zero new warnings
   from payload crates.
5. Multi-node payout smoke PASS at the candidate binary.
6. `git status` clean except `CLAUDE.md` WIP + `.claude/`.
7. SDD/progress ledger + memory updated to name the candidate SHA.

## 5. Execution (after founder confirmation at run time)

```
git branch main-pre-alpha-backup main          # rollback anchor (delete after acceptance)
git checkout main
git merge --ff-only agent-testnet-20260707
git log --oneline -3                            # confirm HEAD == candidate SHA
cargo test --workspace                          # belt-and-braces on main
```

Rollback if anything is wrong post-merge: `git checkout main && git reset --hard
main-pre-alpha-backup` (safe: nothing pushed).

## 6. Post-merge

- `main` becomes the scrub/publish payload (checklist Step 0 satisfied).
- Branch `agent-testnet-20260707` is RETAINED until launch acceptance passes, then deleted per
  house rules (`main-pre-alpha-backup` deleted at the same time).
- Founder continues on `main` for the reset ceremony; any new agent work starts a fresh
  `agent-*` branch off the new `main`.
