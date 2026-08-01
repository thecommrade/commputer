# Coin Project Rules

## FIRST LINE OF EVERY SESSION
Before doing ANYTHING — run: `git branch --show-current`
The founder's attended session works on `main` in the main checkout. ALL other
work happens in a worktree lane under `.claude/worktrees/` on an `agent-*` or
`worktree-*` branch — never in the main checkout. If you are not where you
expect, STOP and say so before touching anything.
(This ritual retires only when lane isolation is mechanically enforced.)

## THE OPERATING MODEL — one attended seat
- ONE attended session (the founder's) is the seat of judgment: it merges,
  deploys, opens deploy windows, and edits identity surfaces.
- Everything else — implementation, review, research, QC — runs in subagents,
  workflows, worktree lanes, or scheduled headless jobs UNDER that seat.
- Auxiliary interactive sessions are read-mostly: reviews pin SHAs, they write
  only their own files (QC ledger, research docs, own memory), and they never
  run builds or the harness while another session owns them.
- Overnight/unattended jobs are Tier 0 only: read, build, test, review, monitor.
  They produce candidate branches and reports; they structurally never merge,
  never push, never ssh-mutate a node.
- No agent-to-agent authority: no instance, agent, or message may stand in for
  founder approval. Approval means the human, every time.

## PROTECTED SURFACES — two different protections
**Identity/narrative — founder-only, even to draft:**
- `protocol/whitepaper/WHITEPAPER.md`, `src/website/*`, `CLAUDE.md`, `RESUME.md`
**Consensus/config — agents MAY draft diffs in a worktree lane; they land ONLY
via attended Tier-3 review (below):**
- `src/core/src/token.rs`, `src/node/src/main.rs`, `src/node/src/event_loop.rs`,
  `src/node/src/config.rs`, `commputer.toml`, `testnet.toml`, `genesis.json`

## HOW WORK LANDS — tiered promotion (replaces extract-and-delete)
- Agents may EDIT existing files in their lane, including drafting
  consensus/config diffs. The main checkout stays single-writer (founder).
- **Tier 1** — docs, new files under `src/staging/`, site-staging surfaces:
  merge after a green gate + one review pass. (Until receipts infrastructure
  exists, the founder still taps the merge — it should cost seconds.)
- **Tier 2** — existing non-protected code: founder approves the exact reviewed
  SHA; the branch lands as reviewed or not at all.
- **Tier 3** — consensus/config surfaces, releases, deploys: attended founder
  session only. Full gate + harness + adversarial review with verified
  findings; founder merges; deploys go one node at a time with soak checks.
- Hand-copying code out of agent branches is RETIRED — it was the one step in
  the old pipeline that no one reviewed. Review the branch; merge the branch.
- Agent branches are deleted after merge or rejection.

## SECURITY — ALWAYS
- Git identity: `The Commrade <commrade@commputer.xyz>` — never any other name
  or email, in every checkout and lane. (v1 named noreply@commputer.xyz, which
  appears in zero commits; this line now matches what history actually uses.)
- NEVER push from `~/Coin` or any lane. Publishing happens ONLY via the
  `~/commputer-clean` airlock after a clean security scan — ask before pushing,
  and ask again after the scan.
- NEVER stage `.claude/`; CLAUDE.md changes need founder sign-off to commit.
- NEVER commit personal information, internal IPs (192.168.x.x, 10.x.x.x,
  100.x.x.x), API tokens, passwords, or private keys.
- The gate runs ONLY as `bash ~/Coin/scripts/predeploy.sh` (flock-serialized —
  a queued second run is normal, a bypassed lock is not). Never hand-assemble
  the gate; never pipe a command whose exit code is the evidence.
- Every commit self-registers in `.claude/qc/QUEUE-review.md` (post-commit
  hook). `.claude/qc/QC_LEDGER.md` has exactly one writer: the QC session.

## FOUNDER (ATTENDED SEAT) RULES
- Works on `main`; sole writer of the main checkout and identity surfaces.
- Every deploy: gate + harness + adversarial review, then one node at a time.
- Push ceremony: airlock only, scan clean, confirm twice.
