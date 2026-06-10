# Compliance Exemption Blueprint — A6-founder-exemption

**Status:** Proposal for founder review. READ-ONLY agent deliverable.
**Touches PROTECTED files at wire-in time:** `genesis.json` (data), `src/node/src/event_loop.rs`, `src/node/src/config.rs`.
**Non-protected file changed:** `src/validator/src/compliance_check.rs` (full patch logic supplied in `src/staging/compliance_exemption.rs`).

---

## 1. The problem, verified against current code

`ComplianceChecker::check()` lives at `src/validator/src/compliance_check.rs:501`:

```rust
pub fn check(&self, addr: &Address) -> ComplianceStatus { ... }
```

Inside it, in order:

| line | rule | verdict |
|------|------|---------|
| 507-513 | duplicate hardware fingerprint | `NerfedAdversarial` |
| **516** | `is_datacenter_ip(ip)` | `NerfedIncidental` |
| 521-523 | `ip_validator_count(ip) > 3` (VPN/proxy) | `NerfedAdversarial` |
| **534** | exact same IP as another node | `NerfedIncidental` |
| **539** | same /24 | `NerfedIncidental` |
| **545** | same /16 | `NerfedIncidental` |
| 552-558 | same ASN | `NerfedIncidental` |

`is_trusted()` (the 720-clean-epoch whitelist) is defined at `compliance_check.rs:302` but **is never consulted by `check()`**. Two facts make this worse than the task summary implies — both verified by grep across `src/`:

1. **`check()` has no `current_epoch` parameter.** It physically cannot call `is_trusted(addr, current_epoch)` without a signature change.
2. **`mark_clean_epoch()` / `clear_clean_streak()` / `record_compliance_status()` are never called anywhere outside the module.** `first_clean_epoch` is therefore always empty in production, so `is_trusted()` **always returns `false`** today. The trust whitelist (ANTI_SCALE.md §12) is documented but **dead code**. Wiring the bookkeeping lives in the PROTECTED `event_loop.rs` epoch-transition path.

Consequence as stated in the task and in `docs/operator/multi_machine_bootstrap.md:23-26` and `docs/adrs/0003`: a founder seed or a legit node on a public datacenter IP is **permanently `NerfedIncidental`**. For seeds this is documented as fine (seeds are connectivity, not validators). But there is **no mechanism** to exempt a genuinely-trusted node or a genesis-declared founder/seed if one were ever needed.

### Where the nerf actually bites
`check()`'s only production caller is `event_loop.rs:437`, which formats the result into the RPC `/peers` display string. The *economic* consequence flows through `account.compliance` (the `ComplianceStatus` stored on the on-chain account, see `src/storage/src/account.rs:116` `is_eligible_for_rewards`, and `event_loop.rs:2531/2534` which count compliant-vs-nerfed for the block `EpochSummary`). So an exemption that changes `check()`'s return value will, once the founder also wires `account.compliance` to be sourced from `check()`, change real reward eligibility. **Note:** today nothing copies `check()` -> `account.compliance` automatically (only `state.rs:764` sets it to `Compliant` on a successful appeal). This is an important containment fact: the exemption is display-only until the founder wires the reward path, which is itself a protected-file decision.

---

## 2. Options

### (a) Wire the existing `is_trusted()` (720 clean epochs) into `check()`

Idea: a validator continuously clean for 720 epochs (30 days) sheds an *incidental* nerf.

**Why it does NOT solve the founder/seed problem and is the wrong primary mechanism:**

- **Chicken-and-egg.** `is_trusted()` requires 720 *clean* epochs. A datacenter-IP node is `NerfedIncidental` from epoch 0 and (if `clear_clean_streak` were wired correctly) would never accumulate a clean streak — it can never become trusted. So wiring `is_trusted()` into the datacenter branch would either (i) do nothing for cloud nodes, or (ii) require defining "datacenter nerf does not break the clean streak," which then lets a 30-day-old warehouse node bleach off the exemption.
- **It is currently dead.** `first_clean_epoch` is never populated. Wiring `is_trusted()` into `check()` is a guaranteed no-op until the founder *also* wires `mark_clean_epoch`/`clear_clean_streak` into the PROTECTED `event_loop.rs` epoch loop and gives `check()` an epoch argument. That is a large protected-file change for a mechanism that still doesn't address founder/seed exemption.
- **Anti-abuse: 30 days is cheap at warehouse scale.** A warehouse can run 1 honest-looking node clean for 30 days, earn "trusted," then... nothing useful, because trust is per-address and each warehouse box is a fresh address that must independently wait 30 days while nerfed. So it isn't a *scale* loophole — but it also isn't an exemption for the founder/seed case, which needs to work from epoch 0.

**Verdict on (a):** Keep `is_trusted()` for its documented purpose (reduced scrutiny / future appeal weighting), and there is one *narrow, safe* use we DO recommend (see §3): let `is_trusted()` shed the **incidental subnet/ASN** nerf (NOT the datacenter nerf, NOT the same-machine nerf). But (a) alone cannot be the founder/seed answer.

### (b) Genesis-declared allowlist of founder/seed addresses, exempt from the **datacenter nerf only**

Idea: `genesis.json` (PROTECTED) gains an optional `founder_seed_exemptions` array of hex addresses. These addresses are exempt from the **`is_datacenter_ip` branch only** — NOT from same-IP, same-/24, same-/16, same-ASN, duplicate-fingerprint, or VPN/proxy branches.

**Why this is the right shape for the founder/seed case:**

- It is **declared at genesis** — fixed at chain birth, in a PROTECTED file only the founder edits, committed to git, auditable by every operator who reads `genesis.json`. A warehouse cannot add itself: there is no runtime API, no transaction, no governance call that mutates the set. The only way in is a new genesis / hard fork the founder signs.
- It is **bounded and tiny** — founder seeds number in the single digits. The anti-abuse argument is "the founder is not going to whitelist a warehouse against their own protocol," which is the same trust root as the founder controlling genesis emission and chain_id. No *new* trust is introduced.
- It is **scoped to the datacenter nerf only.** Seeds legitimately must run on public/cloud IPs for reachability (this is exactly the documented seed reality). But a genesis-exempt address that *also* trips same-IP / same-machine / fingerprint / VPN-proxy is STILL nerfed — so the exemption cannot be used to stand up a fleet behind one box or one subnet. The exemption removes precisely one false-positive ("this legit node happens to be on a cloud range") and nothing else.

**Anti-abuse analysis (the warehouse test):** Suppose an adversary obtains the genesis exemption for address `F` (they cannot, but assume). They spin up 50 boxes in one AWS subnet, all reporting `F`'s address. Result:
- duplicate fingerprint across the boxes -> `NerfedAdversarial` (line 507), exemption does not touch this branch.
- 50 nodes behind overlapping IPs -> `ip_validator_count > 3` -> `NerfedAdversarial` (line 521), not touched.
- same /24 across the boxes -> `NerfedIncidental` (line 539), not touched.
So even a *stolen* genesis exemption buys the adversary nothing beyond a single clean node on a cloud IP — which is exactly one home-equivalent validator, the protected class. The exemption is **not** a scale loophole.

### (c) Status quo — "seeds are not validators," document only

Idea: change nothing in code; rely on the existing documentation (`multi_machine_bootstrap.md`, `runbook.md`, ADR-0003) that seeds are connectivity, not validators, and cloud `NerfedIncidental` is expected.

**Why it is necessary but not sufficient:** It is correct *today* — there is no founder validator that needs rewards from a cloud IP, and the seed bootstrap topology (`multi_machine_bootstrap.md`) explicitly tolerates `NerfedIncidental` on seeds. The risk it leaves open is the task's exact ask: *"if one were ever needed,"* there is no lever to pull, and adding one under launch pressure is how scale loopholes get rushed in. (c) should remain the **default operating posture**; (b) should exist as a **dormant, off-by-default capability** so the lever exists, audited, before it is ever needed.

---

## 3. Recommendation

**Adopt (b) as a dormant, genesis-gated capability, keep (c) as the operating default, and add ONE narrow slice of (a).** Concretely:

1. **(b) — Genesis allowlist, datacenter-nerf-only, OFF by default.**
   - Add an optional `Vec<Address>` exemption set to `ComplianceChecker` plus a setter `set_datacenter_exempt(addr)`.
   - In `check()`, the `is_datacenter_ip` branch (line 516) is skipped **iff** the address is in the exempt set. Every other branch is unchanged.
   - The set is empty unless the founder populates `genesis.json` with `founder_seed_exemptions` and wires it through at boot. With an empty set, behavior is byte-identical to today -> this is (c), the status quo, until the founder opts in.

2. **(a) — narrow slice: `is_trusted()` sheds the incidental SUBNET/ASN nerf only.**
   - A validator that is `is_trusted(addr, epoch)` (720 clean epochs) skips the same-/24, same-/16, and same-ASN branches (lines 539/545/552) — the "geographic proximity" false positives ANTI_SCALE.md §12 explicitly says trust should relax.
   - It does **NOT** skip: datacenter IP (516), exact same IP (534), duplicate fingerprint (507), VPN/proxy (521). Those are the branches that catch actual co-location / Sybil; trust must never relax them.
   - This requires `check()` to take `current_epoch`. To stay non-breaking, the staged module adds `check_at_epoch(addr, current_epoch)` and keeps `check(addr)` as a thin wrapper that passes a sentinel meaning "no epoch known -> trust never applies." Existing call sites (incl. the PROTECTED `event_loop.rs:437`) keep compiling unchanged. The founder upgrades them to `check_at_epoch` only when ready, and only after wiring `mark_clean_epoch`/`clear_clean_streak` (without which trust is permanently false and this slice is inert — i.e. safe by default).

**Why not (a) as the primary lever:** it can't help the founder/seed-on-cloud case (chicken-and-egg, §2a) and it's dead until protected wiring lands. **Why not (b) alone:** the subnet/ASN false-positive relief for genuinely-long-clean home validators is a real, separate good that (b) doesn't give. **Why keep (c):** with an empty exemption set and no epoch wiring, the recommended code IS (c) at runtime — we ship the lever, not the policy.

### Anti-abuse summary table

| attack | defended by | exemption helps attacker? |
|--------|-------------|---------------------------|
| warehouse adds itself to allowlist at runtime | no runtime mutation path; genesis-only, founder-signed | No |
| stolen genesis exemption -> fleet on one cloud subnet | duplicate-fingerprint, same-IP, same-/24, VPN/proxy branches NOT exempted | No — caps at 1 clean node |
| 30-day-clean warehouse box claims trust to dodge datacenter nerf | trust slice does NOT touch the datacenter branch or same-IP/fingerprint | No |
| trusted home node with a noisy neighbor on same /16 | trust slice sheds subnet/ASN only | Yes — intended false-positive relief |
| genesis exemption used to escape same-machine collision | same-IP branch (534) not exempted | No |

---

## 4. What touches PROTECTED files vs not

**Non-protected (full logic staged in `src/staging/compliance_exemption.rs`, founder merges into the real file):**
- `src/validator/src/compliance_check.rs` — add `datacenter_exempt: HashSet<Address>` field, `set_datacenter_exempt`/`clear_datacenter_exempt`/`is_datacenter_exempt`, `check_at_epoch`, and the two skip-guards in the branches. Plus the staged tests.

**PROTECTED (founder-only; blueprint, not edits):**
- `genesis.json` — add optional `"founder_seed_exemptions": ["<64-hex>", ...]` (default absent -> empty -> status quo). Adding the matching `#[serde(default)] pub founder_seed_exemptions: Vec<String>` to `GenesisConfig` in `src/core/src/genesis.rs` is **non-protected** (it's core Rust, not the data file or config.rs) and is included as a staged snippet below for convenience, but the *populated values* in `genesis.json` are the founder's call.
- `src/node/src/event_loop.rs` — at boot/`NodeState::new`, after constructing `compliance`, loop the parsed exemption addresses and call `compliance.set_datacenter_exempt(addr)`. To activate the trust slice later: call `mark_clean_epoch`/`clear_clean_streak` in the epoch transition (around line 2405) and switch the `check()` call at line 437 to `check_at_epoch(addr, self.state.current_epoch)`.
- `src/node/src/config.rs` — only if the founder prefers to surface the exemption list via the node TOML instead of (or in addition to) genesis. Not required for the genesis path.

---

## 5. Wire-in checklist for the founder (morning)

1. Copy `src/staging/compliance_exemption.rs` logic into `src/validator/src/compliance_check.rs` (add field, methods, `check_at_epoch`, two skip-guards). Keep `check()` as the back-compat wrapper.
2. Copy the staged tests into the `#[cfg(test)] mod tests` block (they use only `Address([n;32])` and existing helpers — no new deps).
3. `cargo test -p commputer-validator` — confirm all existing tests still pass and the new ones pass.
4. **(b) activation, optional, PROTECTED:** add `founder_seed_exemptions` to `genesis.json` + `GenesisConfig`; in `event_loop.rs` boot, call `set_datacenter_exempt` per address. Empty = no behavior change.
5. **(a) slice activation, optional, PROTECTED:** wire `mark_clean_epoch`/`clear_clean_streak` in the epoch loop and switch `event_loop.rs:437` to `check_at_epoch`. Until then the trust slice is inert (safe).
6. Decide whether `account.compliance` should be sourced from `check_at_epoch` (today it is set only on appeal at `state.rs:764`). That decision is the actual reward consequence and is a separate, deliberate protected-path change — do not bundle it with the exemption merge.
