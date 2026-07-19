# Go-Live Code Batch — Implementation Plan (founder decisions 2026-07-19)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Detailed
> specs live in the readiness audit (`wf_7574b793-967`; extract at scratchpad `golive-sweep.md`,
> re-derivable from the repo). Tasks are file-disjoint; execute sequentially, review each.

**Goal:** close every agent-executable gap between `1dcc028` and a launchable alpha: seed dialing,
DA ceiling + fetch pacing, Windows runtime fix + best-effort target, macOS release enablement,
chain-id `-3` prep, ops-docs truth-pass.

**Founder decisions in force:** Windows = fix + ship best-effort (untested, continue-on-error);
macOS release = enable; chain-id = bump to `commputer-testnet-3`; seed gap = implement DNS-default
dialing. Protected files (`config.rs`, `commputer.toml`, `genesis.json` line-edits) are PREPARED
here and presented for founder approval at the end — never applied unilaterally.

## Global constraints
Branch `agent-testnet-20260707`; commits local, never push; never stage `CLAUDE.md`/`.claude/`;
frozen `src/staging/pouw/` byte-identical; `src/staging/da/` no-touch by convention; commit
subjects end " (local)" + the standing Co-Authored-By/Claude-Session trailers; full
`cargo test --workspace` gates the batch end.

### Task A — Seed DNS-default dialing
`src/network/src/transport.rs` (non-protected): the empty `SEED_NODES` (:365-369 `TODO(seed)`)
mechanism gains DNS defaults — in testnet mode, convert compiled default seeds (`host:port`,
source of truth `commputer::config DEFAULT_TESTNET_SEEDS = ["seed.commputer.xyz:9000"]` — hoist or
mirror the literal into the network crate to avoid a dependency cycle; note which) into
`/dns4/host/tcp/port` (+ `/dns4/host/udp/port/quic-v1` if the QUIC dial path supports it — check
`.with_dns()` + the existing dial code) and dial them by default; CLI `--seeds`/`--dns-seeds`
still add/override; a non-resolving name must retry-harmlessly, never panic or block startup
(verify the existing dial error path). Unit-test the conversion + a non-resolving-name tolerance
test. If threading testnet-mode INTO transport requires `main.rs` — STOP, report; find a
non-protected seam (the transport already knows dial lists from its callers — inspect).

### Task B — DA ceiling + fetch pacing
1. `src/node/src/executor_loop.rs:54`: `DEFAULT_DA_MAX_ATTEMPTS_PER_CHUNK` 8 → 64 + comment
   citing the ceiling math + const-sanity test.
2. Client-side pacing so serial chunk fetches stay under the server's 10/s/peer GetChunk limit
   (`sync_rate_limiter` tag-2): add a minimum inter-`FetchChunk` interval to `BridgeTransport`
   (`src/staging/pouw-onchain/src/da_transport.rs`, non-frozen) — e.g. `with_min_fetch_interval`
   builder defaulting ON at 150ms for the production constructor path used by
   `BridgeBlobFetcher`/`DaBackedAttestationSource` (`src/node/src/da_attestation.rs`) WITHOUT any
   `main.rs` change (default inside the bridge/fetcher, not the spawn). Std-only (monotonic
   Instant + sleep — it lives on dedicated OS threads, never the event loop). Tests: pacing
   observed between calls; zero-interval config for existing tests so suites stay fast.

### Task C — Windows salt-store runtime fix
`src/node/src/salt_store.rs:144-147`: the UNGATED directory-fsync (`File::open(parent)` +
`sync_all`) fails on Windows → every `persist()` errors → verifier bricked. Gate it `#[cfg(unix)]`
with a `#[cfg(not(unix))]` no-op + comment (durability caveat: dir-entry fsync is a Unix
guarantee; Windows relies on file `sync_all` only). Audit the file for any other ungated unix-ism.
Tests still pass on Linux (behavior unchanged there).

### Task D — Release packaging (macOS + Windows + script hygiene)
`src/staging/ops/release.yml`:
1. Enable macOS: uncomment `build-macos`, assets named `commputer-macos-arm64` → **rename to
   `commputer-macos-aarch64`** (+ `commputer-macos-x86_64`) to match PROTECTED
   `src/website/install.sh` expectations; `continue-on-error: true` stays.
2. Add `build-windows`: `runs-on: windows-latest`, target `x86_64-pc-windows-msvc`, asset
   `commputer-windows-x86_64.exe`, `continue-on-error: true` (best-effort, untested platform).
3. Fix the three stub gaps: release job `needs: [build, build-macos, build-windows]`; extend the
   combined-manifest loop (:190) and `ASSETS` loop (:213) with the new names; refresh the
   release-notes Assets section AND the stale "PoUW production-inert" notes text (now: live payout
   + escalation, alpha, consensus-reset chain-id-3).
`scripts/install.sh`: handle Darwin (`uname -s`) + `arm64→aarch64`; keep Linux behavior.
`scripts/build-release.sh:10`: `VERSION="0.1.0"` → read from `src/Cargo.toml` (grep
workspace.package version) or hardcode `0.1.0-alpha.1` with a sync comment.
`scripts/cross-build.sh`: header comment noting it's a dev helper whose asset names do NOT match
release.yml (or align them — implementer's choice, note it).

### Task E — Chain-id `commputer-testnet-3` prep
Non-protected now: `src/core/src/genesis.rs:6` `TESTNET_CHAIN_ID` → `"commputer-testnet-3"`;
`:268` `genesis_timestamp` → the reset day (use a placeholder date constant + comment "founder
sets at reset"; keep deterministic — pick 1784505600 = 2026-07-20 00:00 UTC); doctor crate 3 sites
(`doctor/src/main.rs:113,:267`, `doctor/src/checks/genesis.rs:358`); `scripts/seed.toml:5`; NEW
cross-crate equality test (non-protected test file, e.g. in node tests): asserts
`commputer::config DEFAULT_TESTNET_CHAIN_ID == commputer_core::genesis::TESTNET_CHAIN_ID` (the
missing §1.12 test). Run workspace tests — expect chain-id-sensitive tests to follow the const;
fix any that hardcode `-2` (report each).
PREPARE-ONLY (present to founder, do NOT apply): `src/node/src/config.rs:12` one-liner;
`commputer.toml:8` (`chain_id`, also stale `-1`); root `genesis.json:2` — exact diffs written to
`src/staging/docs/2026-07-19-protected-chainid-hunks.md`.

### Task F — Ops docs truth-pass + gc retention doc
All in `src/staging/ops/` + `src/node/src/da_store.rs` docs (non-protected):
1. Refresh the 5 ops docs: PoUW is LIVE (payout proven 2026-07-18, escalation 2026-07-19) — remove
   "production-inert / pots refund" framing; operator guide §6 `/submit_job` claim (it IS wired,
   keyed tier); chain-id `-3`; new release assets (macOS/Windows best-effort); seed dialing now
   default-DNS (Task A); add the worker-vs-Caddy `/faucet`/`/tx`/`/ws` conflict as an explicit
   FOUNDER DECISION box in the publish checklist (grey-cloud-Caddy-only vs extend worker.js);
   scrub-runbook note: identity correction still unapplied in `scrub_and_mirror.sh` + ground truth
   must be re-derived (public repo moved 2026-07-03; `492c5b1`/`b8ee843`).
2. `da_store.rs:20-26,:165-167` doc: gc live-set = `pending_jobs ∪ job_lifecycles ∪
   escalation_rounds` (+ attestation keys); note gc is UNWIRED and the 4 GiB cap is a slow-burn
   publisher-liveness item for the seed (founder batch, protected event_loop).

### Batch gate
Full `cargo test --workspace` green; frozen check; smoke NOT required (no consensus-path change
except chain-id, which multinode smoke covers implicitly at next run); then present Task E's
protected hunks + the go-live founder checklist.
