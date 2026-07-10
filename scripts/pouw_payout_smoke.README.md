# pouw_payout_smoke.sh — operator runbook

The **live PoUW pay-out smoke harness**. It is the one acceptance proof that
cannot run inside `cargo test`: a real multi-node loopback network where a
submitted compute job flows through the **live executor + verifier actor loops**
and the **real libp2p DA transport**, settles on-chain, and **actually pays** the
executor (85%) and the committee verifiers (10%/k), burning 5% and consuming the
submitter's budget.

The in-process equivalent (no network, no libp2p) is
`src/node/tests/pouw_payout_e2e.rs`. This harness proves the same money-path
end-to-end over the network, the way an operator will actually run it.

> **This is operator-run.** It builds and runs `N+1` real `commputer` processes,
> mines them up, bonds them, submits a job, waits for settlement (tens of seconds
> to a few minutes), and asserts the pay-out. It **cannot** be run to completion
> in a CI sandbox or the dev container — it needs minutes of wall-clock, the live
> actor loops, and a working libp2p swarm. The script itself has been
> syntax-checked (`bash -n`) and dry-run-validated (`DRY_RUN=1`); the full run is
> yours to execute on real hardware.

---

## 1. Prerequisites

- Linux/macOS host with the Rust toolchain (`cargo`) — the harness builds
  `target/debug/commputer` if it is missing.
- `curl` and `jq` on `PATH` (the harness parses RPC JSON with `jq`).
- Enough free RAM/CPU to run `SMOKE_NODES + 1` debug-build nodes (default **5**
  processes) plus their RocksDB stores. A few GB is plenty for a smoke run.
- Free localhost TCP ports: P2P `19001…` and RPC `19945…` (override with
  `SMOKE_BASE_P2P` / `SMOKE_BASE_RPC`).
- No network egress needed at run time (all nodes are on `127.0.0.1`). A build
  may need crates.io the first time.

There are **no manual steps inside the run** — wallet creation, funding, bonding,
submission, and settlement polling are all automated. The only decisions are the
env knobs in §4.

---

## 2. How to run

```bash
# From anywhere in the repo:
bash scripts/pouw_payout_smoke.sh

# First validate plumbing without starting any node (fast, safe anywhere):
DRY_RUN=1 bash scripts/pouw_payout_smoke.sh

# A larger, higher-signal run:
SMOKE_NODES=5 SMOKE_BUDGET_COMME=50 bash scripts/pouw_payout_smoke.sh
```

Exit codes: **0** = PASS · **1** = FAIL (an assertion did not hold) ·
**2** = build/node/setup failure · **3** = settle timeout.

Logs land in `scripts/pouw_payout_smoke_logs/`:
`node1.log … nodeN.log`, `submitter.log`, `submit.log`, `bond.log`.

---

## 3. What it does (flow)

1. **Build** `commputer` (skipped if the debug binary already exists).
2. **Pre-create wallets** — one keystore per node with `commputer wallet create`
   (this prints the address and, for the submitter, the 24-word seed, and does
   **not** touch any chain DB). The node loads this exact wallet on boot.
3. **Start `N` validator nodes + 1 submitter node** on loopback, wiped homes,
   bootstrap-leader topology (node1 has no `--seeds`; everyone else seeds node1).
   Every node starts with `--rpc-key` so the admin `/submit_job` tier is gated.
4. **Wait for consensus** — poll every node's `/status` until they reach
   `WARMUP_MIN_HEIGHT` and agree (a mirror of `multinode_assert.sh`, via RPC).
5. **Wait for funds** — nodes earn block rewards (~15.855 COMME/block, credited
   to the block producer). Wait until every validator can afford `bond + budget`
   and the submitter can afford `budget + fee`. The **submitter self-funds by
   mining** — see §6 for why it is a node and not a `commputer send` target.
6. **Bond every validator** with `commputer bond <amt>` (signs with the node's
   local wallet, reads the nonce over RPC, broadcasts to `/tx`). Bonding makes a
   node PoUW-**eligible** (`is_validator && Compliant && bonded ≥ min_bond`), which
   is what auto-enables its executor/verifier loops. The submitter is left
   **unbonded**, so it can never be drawn as executor or committee.
7. **Snapshot BEFORE** — every account's `(balance, total_mined)` and the chain
   `burned`, all read from node1's consistent view.
8. **Submit the job** — `POST /submit_job` to node1 with `X-API-Key`, carrying
   `program_hex + input_hex + budget + submitter_seed`. node1 publishes the DA
   blob and advertises its chunks; the executor/committee fetch it over libp2p.
9. **Wait for settlement** — poll until `burned` jumps (the Confirmed 5% burn) or
   the job's deadline height passes (`submit_height + MAX_SETTLE_BLOCKS`).
10. **Snapshot AFTER** and **assert** (§5).

---

## 4. Knobs (env vars, with defaults)

| Var | Default | Meaning |
|---|---|---|
| `SMOKE_NODES` | `4` | Bonded validator nodes (executor + committee candidates). **4+ recommended** — see §7. |
| `SMOKE_BUDGET_COMME` | `10` | Job budget in whole COMME. Must be `≥ 1` (the `/submit_job` floor is 1 COMME). |
| `SMOKE_BOND_COMME` | `2` | Per-validator bond. `min_bond` is only `0.00001` COMME, so this is generous while leaving budget headroom for the executor's claim bond. |
| `SMOKE_RPC_KEY` | `smoke-admin-key` | Admin key; required by `/submit_job`. Public read endpoints stay open. |
| `SMOKE_PASSWORD` | `smoke` | Wallet password (also passed as `COMMPUTER_WALLET_PASSWORD`). |
| `SMOKE_NODES`,`SMOKE_BASE_P2P`,`SMOKE_BASE_RPC` | `19000/19944` | Port bases; node `i` uses `base+i`. |
| `SMOKE_TMPROOT` | `/tmp/pouw-payout-smoke` | Per-node HOME root (wiped each run). |
| `SMOKE_LOG_DIR` | `scripts/pouw_payout_smoke_logs` | Log output. |
| `WARMUP_MIN_HEIGHT` | `3` | Chain must reach this before funding proceeds. |
| `FUND_TIMEOUT` | `420` | Seconds to wait for nodes to mine enough. |
| `BOND_SETTLE_BLOCKS` | `6` | Blocks to wait after bonding so Bond txs apply. |
| `MAX_SETTLE_BLOCKS` | `50` | `submit_height + this` ⇒ the lifecycle must have settled. |
| `SETTLE_TIMEOUT` | `900` | Hard wall-clock cap on the settle wait. |
| `SMOKE_PROGRAM_HEX` / `SMOKE_INPUT_HEX` | embedded DOUBLER | Override the guest program / input (hex). |
| `DRY_RUN` | `0` | `1` ⇒ validate config + embedded program + jq/curl plumbing, start no nodes. |
| `FORCE_BUILD` | unset | Force `cargo build` even if the binary exists. |

---

## 5. What a PASS looks like

The harness prints a **PAY-OUT LEDGER**, then four checks. The core idea: every
node's balance is churning from block-reward mining, so raw balance deltas are
useless. Instead it computes, per account,

```
payout_delta = (balance_after - balance_before) - (total_mined_after - total_mined_before)
```

`total_mined` is credited in lockstep with `balance` for every mined block, so
this subtraction removes **all** mining income exactly, leaving only the PoUW
pay-out (± negligible per-tx fee noise). The four assertions (all must hold):

```
[1/4] executor : exactly ONE validator's payout_delta ≈ 0.85 * budget   (worker share)
[2/4] committee: ≥2 OTHER validators paid, summing ≈ 0.10 * budget        (verifier pool)
[3/4] submitter: payout_delta ≈ -(budget + fee) — budget CONSUMED, not refunded
[4/4] burn     : chain `burned` rose ≈ 0.05 * budget                       (Confirmed signature)
```

Example PASS tail (budget = 10 COMME, N = 4; node2 happened to claim):

```
[payout]   node1 5f3a..  payout_delta=0.33333333 COMME  verifier
[payout]   node2 91c0..  payout_delta=8.50000000 COMME  EXECUTOR (85%)
[payout]   node3 a771..  payout_delta=0.33333333 COMME  verifier
[payout]   node4 0b8e..  payout_delta=0.33333333 COMME  verifier
[payout]   submitter e2d1..  payout_delta=-10.00100000 COMME (expect ~ -10.0)
[payout]   chain burned Δ = 0.50000001 COMME (expect ~ 0.5)
[payout] PASS  [1/4] executor …
[payout] PASS  [2/4] committee …
[payout] PASS  [3/4] submitter …
[payout] PASS  [4/4] burn …
[payout] ======== PASS — the job PAID the executor + committee on a live network ========
```

**Which node claims is non-deterministic** — the harness asserts the *pattern*
(one 85% winner, ≥2 verifiers summing to the pool), never a specific node.

A refund terminal (NoQuorum / Timeout / Disputed / claim-expiry) leaves the
submitter made-whole and no 5% burn, so checks **[3]** and **[4]** fail loudly —
the harness will not mistake a refund for a pay-out.

---

## 6. Auth + a note on how the submitter is funded (a real constraint)

- **`/submit_job` auth**: it lives in the RPC **admin tier**, gated by
  `auth_middleware`, which checks the **`X-API-Key`** request header against the
  node's `--rpc-key` (constant-time compare; a missing/incorrect key ⇒ `401`).
  The harness starts every node with `--rpc-key "$SMOKE_RPC_KEY"` and sends that
  header on the submit. Everything else it polls (`/status`, `/account`,
  `/nonce`, `/tx`) is on the **public** tier and needs no key.

- **Why the submitter is a dedicated unbonded node, not a `commputer send`
  target**: `commputer send` verifies the sender's balance by opening the chain
  RocksDB (`open_chain_state`), which takes an **exclusive process lock**. While a
  node is running it holds that lock, so `send` from a live node's HOME fails.
  `commputer bond`, by contrast, reads its nonce over **RPC** and never opens the
  DB, so bonding a running node works. The harness therefore funds the submitter
  the only lock-free way available: it runs the submitter as its own **unbonded**
  node that mines its own balance. Unbonded ⇒ never eligible ⇒ never drawn as
  executor or committee, so it stays cleanly outside the game while its
  `payout_delta` still cleanly shows the `-budget` consumption.

---

## 7. Why the default is 4 nodes (committee math)

The executor is **excluded from its own committee** (`candidates` filter:
`a.address != executor`). The committee target is `k = 3` and the verdict quorum
is `2`. So:

- **3 nodes** → 1 executor + only **2** committee candidates (undersized). It can
  still Confirm (quorum 2), but a **single** DA hiccup or straggler on either
  verifier ⇒ NoQuorum ⇒ refund ⇒ **FAIL**. The harness warns and runs anyway.
- **4 nodes** → 1 executor + **3** committee. Quorum 2 of 3 tolerates one
  straggler. This is the robust default.
- **5+** → same, with spare candidates.

`>= 2` verifiers paid is asserted regardless, so a full `k = 3` isn't required to
pass; 4 nodes just makes reaching quorum reliable.

---

## 8. Expected timings (debug build, ~5–8 s/block)

| Phase | Rough time |
|---|---|
| Build (first run only) | a few minutes |
| Boot + reach consensus | ~15–40 s |
| Mine funds + bond + apply | ~1–4 min (dominated by block-reward emission; larger budgets ⇒ longer) |
| Submit → settle | ~claim + 30 phase-blocks + margin ≈ **3–6 min** (windows are 10 blocks each: claim/result/commit/reveal) |
| **Total** | typically **~6–12 min** after the build |

Settlement is anchored at the executor's claim height, not the submit height, so
the wait scales with the phase windows (genesis default `10/10/10/10`), not with
how fast the actors act.

---

## 9. Troubleshooting

- **FAIL [3] submitter refunded + FAIL [4] no burn** → the job did **not**
  Confirm. Almost always one of:
  - **DA fetch failed across libp2p** — the executor/committee could not retrieve
    the blob node1 published. Grep the node logs for `da`, `fetch`, `provider`,
    `Advertise`. Ensure all nodes are peered (`/peers`), and that DA is enabled
    (it is by default whenever a node auto-runs the loops).
  - **Committee straggler / NoQuorum** — raise `SMOKE_NODES` to 4+ (see §7).
- **`nodes never mined enough to bond/submit`** → raise `FUND_TIMEOUT`, or lower
  `SMOKE_BUDGET_COMME` / `SMOKE_BOND_COMME` (less to accumulate), or reduce
  `SMOKE_NODES`. Check that blocks are actually advancing in `node1.log`.
- **`bond rejected`** → see `bond.log`. Usually insufficient balance (funding
  gate raced) or the RPC not reachable; the fund gate should prevent the former.
- **`submit_job not accepted`** → see `submit.log`. `401` ⇒ key mismatch
  (`SMOKE_RPC_KEY`); `503 DA backend not enabled` ⇒ the target node didn't open
  its DA store (should not happen with defaults); `budget too low` ⇒ raise
  `SMOKE_BUDGET_COMME` (floor is 1 COMME).
- **`settle wait timed out`** (exit 3) → blocks stalled or windows are longer
  than expected; inspect `node1.log` for production, raise `SETTLE_TIMEOUT` /
  `MAX_SETTLE_BLOCKS`.
- **Port already in use** → change `SMOKE_BASE_P2P` / `SMOKE_BASE_RPC`.
- **Stuck processes after a crash** → the EXIT trap kills children, but if the
  script was `kill -9`'d, `pkill -f 'target/debug/commputer run'` and remove
  `/tmp/pouw-payout-smoke-*`.

---

## 10. How `program_hex` was produced

The default `program_hex` is the **DOUBLER** WASM guest (doubles each input byte
mod 256) — byte-for-byte the guest used by `src/node/tests/pouw_payout_e2e.rs`.
It was compiled from its WAT with the workspace's pinned `wat = "=1.251.0"` crate
and hex-encoded:

```rust
// throwaway: hex::encode(wat::parse_str(DOUBLER))
```

- `program_hex` = 229 bytes, `sha256 = 570471e71188e17bffaf66d6abbf85e9d73cca26d91df1a3e41dbe9a71a0d7c5`
  (this is exactly the on-chain `program_hash` the submitted `SubmitJobV2`
  carries — the linchpin the executor/verifiers re-bind on fetch + re-execute).
- `input_hex` = `0102032807` (the bytes `[1,2,3,40,7]`).

To use a different guest, set `SMOKE_PROGRAM_HEX` / `SMOKE_INPUT_HEX`. Any
deterministic WASM whose `run(ptr,len)->i64` re-executes identically on every
node will do; a non-deterministic guest would make the committee disagree and
the job would settle Disputed/NoQuorum instead of Confirmed.
```

Since no `wat2wasm`/`wasm-tools` binary is assumed on operator boxes, the hex is
**embedded** rather than compiled at run time — the harness needs no WASM
toolchain, only `cargo` (for the node), `curl`, and `jq`.
