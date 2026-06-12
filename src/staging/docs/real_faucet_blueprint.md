# Real Faucet Blueprint (A3-real-faucet)

Status: STILL RELEVANT. The `/faucet` handler at `src/node/src/rpc.rs:721` returns an
honest 503 ("faucet not provisioned"). To actually dispense, the node needs a funded,
key-bearing faucet wallet on `RpcState` and a way to pre-fund its address. This document
is the founder-only wire-in plan plus the staged drop-in handler.

All file:line claims below were verified against the working tree on branch
`agent-overnight-20260610` (read-only).

---

## 0. TL;DR recommendation

**A working faucet is NOT required for a 3-operator bootstrap. Ship the honest 503; do
the faucet later.** Validators self-register and earn COMME purely by producing blocks
(`credit_block_reward`, `src/storage/src/state.rs:373`), and registration is
stake-exempt for the first `BOOTSTRAP_REGISTRATION_BLOCKS = 1000` blocks
(`src/core/src/transaction.rs:162`). A 3-operator bring-up therefore needs zero faucet:
each operator funds itself by mining. The faucet only matters once non-validating
end-users (wallet testers, dApp devs) need a few COMME to pay the `MINIMUM_FEE` on a
Transfer. See section 7 for the full argument and the staged-but-dormant landing path.

---

## 1. What the current code actually does (verified)

- `RpcState` (`src/node/src/rpc.rs:60`) has `tx_sender: mpsc::Sender<Transaction>`
  (`:62`), `status` (`:64`), `balances: Mutex<HashMap<String, BalanceInfo>>` (`:68`),
  `is_testnet: bool` (`:90`), `faucet_claims: Mutex<HashMap<String, u64>>` (`:94`).
  There is **no** faucet wallet field.
- `faucet()` (`src/node/src/rpc.rs:721`): testnet gate -> address-format check (64 hex)
  -> per-epoch rate-limit read of `faucet_claims` -> returns `503` with
  `{"error":"faucet not provisioned", ...}` WITHOUT consuming the claim slot. This is
  the W5.7 F-6 honesty fix; the older version lied with `{success:true}`.
- The route is wired at `build_router` (`src/node/src/rpc.rs:1197`):
  `.route("/faucet", post(faucet))`.
- The honesty test `faucet_does_not_lie_when_unprovisioned` is at
  `src/node/src/rpc.rs:1631`; the test-only constructor `make_rpc_state()` is at
  `src/node/src/rpc.rs:1276` and builds `is_testnet: true` with empty `faucet_claims`.
- `RpcState` is constructed for real at `src/node/src/main.rs:1107` (PROTECTED file).
- The validator wallet is loaded at `src/node/src/main.rs:973-981` via
  `Keystore::load(...)` (or `Wallet::generate()` on first run, `:990`) and passed to
  `EventLoop::new(state, wallet, ...)` at `:1146`.

### Transaction admission rules the faucet tx MUST satisfy

A tx queued via `tx_sender` is drained by the event loop and validated by
`validate_tx_for_mempool` (`src/node/src/event_loop.rs:2109`). The faucet tx must pass
ALL of these or it is silently dropped (logged `warn!`, no client feedback):

1. `tx.from != [0u8;32]` (`:2110`). Faucet `from` is the faucet address — fine.
2. `tx.verify()` (`:2113`). `verify()` (`src/core/src/transaction.rs:213`) signs over
   `(from || nonce || kind || fee)` with **no chain_id**. `sign_transaction`
   (`src/core/src/signing.rs:30`) signs exactly those bytes via `tx_signable_bytes`
   (`:7`). So a tx signed with `sign_transaction(&mut tx, &faucet_wallet)` passes
   `verify()`. (Do NOT use `tx_signable_bytes_with_chain_id`; the mempool path does
   not use chain_id.)
3. Not a duplicate hash (`:2118`).
4. `tx.fee >= MINIMUM_FEE` (`= 100_000`, `src/core/src/transaction.rs:148`) because a
   `Transfer` is **NOT** fee-exempt (`:2131-2136`; only `ValidatorRegister` is exempt).
   **The faucet wallet therefore spends `MINIMUM_FEE` per dispense on top of the
   1 COMME sent.** Budget accordingly when pre-funding.
5. **Strict nonce** (`:2137-2149`): `tx.nonce == on_chain_nonce + pending_from_sender`,
   where `on_chain_nonce` is the faucet account's `nonce` in chain state and
   `pending_from_sender` is the count of the faucet's txs already in the mempool. This
   is the hard constraint and drives the design in section 3.

---

## 2. Non-protected change: add the faucet wallet + nonce counter to RpcState

`src/node/src/rpc.rs` is NOT protected, so the founder edits it directly. Add two
fields to `struct RpcState` (after `faucet_claims`, `:94`):

```rust
    /// A3: Optional faucet signing wallet. `None` on mainnet / when no faucet
    /// seed is provisioned -> the handler keeps the honest 503. `Some` only when
    /// a funded faucet account exists in genesis and a seed phrase was supplied.
    pub faucet_wallet: Option<commputer_core::wallet::Wallet>,
    /// A3: Monotonic next-nonce for the faucet account. Seeded from the faucet
    /// account's on-chain nonce at construction; incremented on every successful
    /// dispense so back-to-back claims in one block don't collide on nonce.
    pub faucet_next_nonce: tokio::sync::Mutex<u64>,
```

Notes:
- `Wallet` is NOT `Clone`/`Debug` and has a `Drop` that zeroizes the key
  (`src/core/src/wallet.rs:77`). `RpcState` derives nothing, so adding a non-Clone
  field is fine. `RpcState` is only ever held behind `Arc`, never cloned.
- Keep `faucet_wallet` as the LAST-ish field or place both together; update every
  `RpcState { .. }` literal (there are exactly two: `main.rs:1107` and the test
  `make_rpc_state` at `rpc.rs:1276`).

Then replace the body of `faucet()` (`rpc.rs:721-767`) with the staged handler in
`src/staging/rpc_faucet_dispense.rs` (see section 5 for the exact drop-in).

---

## 3. Nonce-race design (why a counter, not a balances lookup)

The naive approach -- read the faucet's nonce from `state.balances` -- is wrong. The
`balances` map (`rpc.rs:68`) is refreshed by the event loop from chain state
(`event_loop.rs:484-497`) and only advances the faucet nonce AFTER a faucet tx is
*finalized in a block* (~2s block time + finalization). Two faucet requests arriving in
the same block window would both read the same on-chain nonce and emit two txs with the
SAME nonce; the second fails rule #5 and is dropped with no client signal.

Fix: `RpcState.faucet_next_nonce` is the source of truth at runtime.
- Seed it once, at construction, from the faucet account's on-chain nonce (0 for a
  brand-new genesis-funded account that has never sent a tx).
- On each dispense: lock it, use the current value as `tx.nonce`, queue the tx, and only
  if `try_send` succeeds, increment it. On a full queue (503) do NOT increment.
- This makes the faucet correct under burst load within a block and across blocks. The
  per-epoch `faucet_claims` rate-limit (1 claim/address/epoch) caps absolute volume so
  the counter can't run unboundedly ahead of chain state.

Edge case: if the faucet wallet ever sends txs by some other path, the counter could
desync. The faucet wallet is dedicated and only this handler signs for it, so that does
not happen. (Documented as an invariant.)

---

## 4. PROTECTED wire-in at main.rs:1107 (founder-only)

In `run_node` (`src/node/src/main.rs:847`), before constructing `RpcState`, derive the
faucet wallet from a seed and read the faucet account's on-chain nonce:

```rust
    // A3: Optional faucet wallet. Source the seed phrase from env (preferred for a
    // secret) or config; absent -> faucet stays at the honest 503.
    let faucet_wallet: Option<commputer_core::wallet::Wallet> = if testnet {
        match std::env::var("COMMPUTER_FAUCET_SEED") {
            Ok(phrase) if !phrase.trim().is_empty() => {
                match commputer_core::wallet::Wallet::from_seed_phrase(phrase.trim()) {
                    Ok(w) => {
                        info!("Faucet wallet provisioned: {}", hex::encode(w.address().0));
                        Some(w)
                    }
                    Err(e) => { warn!("Invalid COMMPUTER_FAUCET_SEED, faucet disabled: {}", e); None }
                }
            }
            _ => None,
        }
    } else { None };

    // Seed the runtime nonce counter from chain state (0 if the account never sent a tx).
    let faucet_next_nonce = faucet_wallet.as_ref()
        .and_then(|w| state.accounts.get(w.address()).map(|a| a.nonce))
        .unwrap_or(0);
```

Why a **seed phrase**, not raw 32 bytes: `Wallet`'s only public raw constructors are
`Wallet::generate()` and `Wallet::from_seed_phrase(&str)` (`src/core/src/wallet.rs:19,47`).
`from_secret_bytes` is private. A 24-word BIP39 phrase is the supported way to load a
deterministic key, and it round-trips (`wallet.rs:120` test). Generate the faucet phrase
once with `commputer wallet` tooling, fund its address in genesis (section 6), and pass
the phrase via `COMMPUTER_FAUCET_SEED` (mirrors the existing
`COMMPUTER_WALLET_PASSWORD` env pattern at `main.rs:337,1031`). Do NOT commit the phrase.

Then add the two fields to the `RpcState { .. }` literal at `main.rs:1107`:

```rust
        faucet_wallet,
        faucet_next_nonce: tokio::sync::Mutex::new(faucet_next_nonce),
```

### Optional config plumbing (config.rs is PROTECTED)

If you'd rather not use an env var, add a field to `NodeConfig`
(`src/node/src/config.rs:17`), e.g. `pub faucet_seed: Option<String>`, and read it in
`run_node`. Env var is simpler and keeps the secret out of `~/.commputer/config.toml`;
recommended. Either way this is a protected-file edit only the founder makes.

---

## 5. The genesis pre-fund gap (IMPORTANT — bigger than the prompt implies)

The prompt says "pre-fund its address in genesis (testnet_genesis.rs/genesis.json)".
Verified reality: **there is currently NO mechanism that funds ANY account at genesis.**

- `genesis.json` is a bare `GenesisConfig` (`chain_id`, supply, emission, floors,
  timestamps). It has **no `accounts` array** (read the file: keys end at
  `channel_floors`). `GenesisConfig` (`src/core/src/genesis.rs:11-37`) has no accounts
  field either.
- `create_genesis_for_dir` (`src/node/src/main.rs:363`) builds a genesis `Block` with
  `transactions: vec![]` and producer `Address([0u8;32])`. Empty.
- `apply_block` at height 0 credits no reward (`credit_block_reward` early-returns for
  height 0 / zero producer, `src/storage/src/state.rs:374-377`) and seeds no balances.
- `testnet_genesis.rs::generate_testnet_genesis` builds a `TestnetGenesis { accounts }`
  with `Wallet::generate()` accounts -- but (a) it **discards the secret keys** (only
  `hex::encode(wallet.address().0)` is kept, `:54-58`), and (b) **nothing calls it**:
  `rg generate_testnet_genesis` finds only the `mod` declaration
  (`src/node/src/lib.rs:3`, `main.rs:22`) and its own tests. It is dead code. Its
  `accounts` are never applied to `ChainState`.

So "pre-fund in genesis" is not a one-line genesis.json edit; it requires building the
genesis-funding path that doesn't exist yet. Two viable approaches:

### Approach A (recommended): genesis funding transactions
Add an `accounts: Vec<(String /*addr hex*/, u64 /*raw units*/)>` (serde-default empty)
to `GenesisConfig` (`genesis.rs`, NOT protected) and to `genesis.json` (PROTECTED). In
`create_genesis_for_dir` (`main.rs`, PROTECTED), for each entry synthesize a
protocol-issued credit. The cleanest is to special-case genesis funding inside
`apply_block`/`apply_transaction` for height 0 (storage `state.rs`, NOT protected): for
a genesis credit, insert/increase the account balance directly and bump
`total_emitted` by the funded amount (so supply accounting stays correct). Because the
genesis block is deterministic and every node builds it identically, all nodes converge
on the same funded faucet balance.

Fund the faucet generously for testnet, e.g. `100_000 * UNITS_PER_COMME` (100k COMME).
Each dispense costs `1 COMME + MINIMUM_FEE (0.001 COMME)`, so 100k COMME supports ~100k
claims. (`UNITS_PER_COMME = 100_000_000`, `src/core/src/token.rs:8`, PROTECTED constant
-- only referenced, not changed.)

### Approach B (no protocol change): mine into the faucet, then it's just a hot wallet
Make the faucet wallet a validator (it mines block rewards like any operator). No
genesis change at all. Downside: the faucet has no balance until it has produced blocks,
and it competes for rewards. Fine for a quick demo, ugly for a real faucet.

Approach A is the right long-term path; Approach B is the zero-protected-change fallback.

---

## 6. Staged handler — see src/staging/rpc_faucet_dispense.rs

That file contains the full replacement `faucet()` body plus a private
`build_faucet_transfer` helper, written against the post-edit `RpcState` (with
`faucet_wallet` + `faucet_next_nonce`). Behavior:
- testnet gate, address-format check, per-epoch rate-limit read — unchanged from current.
- if `state.faucet_wallet.is_none()` -> the EXACT existing honest 503
  (`{"error":"faucet not provisioned", ...}`), claim slot untouched. Honesty test
  (`rpc.rs:1631`) keeps passing.
- if `Some(wallet)`: lock `faucet_next_nonce`, build a `TxKind::Transfer { to,
  amount = Amount::from_raw(UNITS_PER_COMME) }` (1 COMME) with `fee = MINIMUM_FEE`,
  sign with `sign_transaction`, `tx_sender.try_send`:
  - `Ok` -> increment nonce, record `faucet_claims[address] = current_epoch`, return 200
    with `{success:true, amount, tx_hash}`.
  - `Full` -> 503 "transaction queue full", do NOT consume claim or nonce.
  - `Closed` -> 500 "node shutting down".

---

## 7. Is a working faucet needed for a 3-operator bootstrap? (recommendation)

**No.** Evidence:
- Validators earn COMME by producing blocks: `credit_block_reward`
  (`src/storage/src/state.rs:373`) pays the producer the halving-schedule reward each
  block. A 3-operator set mints and accrues balance with zero faucet involvement.
- Validator registration is stake-exempt for the first
  `BOOTSTRAP_REGISTRATION_BLOCKS = 1000` blocks
  (`src/core/src/transaction.rs:160-162`), and `ValidatorRegister` is fee-exempt in the
  mempool (`event_loop.rs:2131-2134`). So an operator can self-register and start
  earning with a zero balance — no seed COMME required.
- The faucet's only purpose is funding *non-mining* accounts (wallet/dApp testers) so
  they can pay `MINIMUM_FEE` on a `Transfer`. That audience does not exist at 3-operator
  bring-up.

Recommendation: **keep the honest 503 for launch.** Land the faucet as REVIEW-ONLY
staging now (this deliverable), and wire it in only when you onboard non-validator
users. When you do wire it: prefer genesis Approach A, fund 100k COMME, supply the seed
via `COMMPUTER_FAUCET_SEED`, and keep `faucet_wallet = None` on mainnet so production
never ships a hot dispensing key by accident. Priority: LOW (no launch blocker;
user-experience nicety).

---

## 8. Exact edit checklist for the founder

1. [non-protected] `src/node/src/rpc.rs`: add `faucet_wallet` + `faucet_next_nonce`
   fields to `RpcState` (after `:94`); replace `faucet()` body (`:721-767`) with the
   staged handler + `build_faucet_transfer` helper.
2. [non-protected] `src/node/src/rpc.rs` tests: update `make_rpc_state` (`:1276`) to set
   `faucet_wallet: None, faucet_next_nonce: Mutex::new(0)`; add the staged
   `faucet_dispenses_when_provisioned` test (section 6 / staged file).
3. [PROTECTED] `src/node/src/main.rs`: add the faucet-wallet derivation block before
   `:1107` and the two field initializers in the `RpcState { .. }` literal.
4. [PROTECTED, optional] `src/node/src/config.rs`: add `faucet_seed` to `NodeConfig` if
   not using the env var.
5. [non-protected] `src/core/src/genesis.rs`: add `#[serde(default)] pub accounts:
   Vec<(String,u64)>` to `GenesisConfig` (Approach A).
6. [non-protected] `src/storage/src/state.rs`: apply genesis `accounts` as height-0
   credits in `apply_block`, bumping `total_emitted`.
7. [PROTECTED] `genesis.json`: add `"accounts": [["<faucet_addr_hex>", 10000000000000]]`
   (100k COMME = 100000 * 100_000_000).
8. Generate the faucet seed phrase out-of-band, fund its address in step 7, export
   `COMMPUTER_FAUCET_SEED` on the node. NEVER commit the phrase.
