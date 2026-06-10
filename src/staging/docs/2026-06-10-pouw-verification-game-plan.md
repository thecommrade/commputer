# PoUW Verification & Settlement Game — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a self-contained, runnable Rust crate that implements the deterministic verification-and-settlement game from the spec, plus a simulation harness that empirically proves cheating is EV-negative.

**Architecture:** A new workspace-member crate `commputer-pouw` at `src/staging/pouw/`. Pure library: data model → trait seams (execution / equivalence / chain) → committee selection → commit-reveal → verdict → settlement (6 branches, integer basis-point math) → escalation & trap → engine (orchestrates one job) → simulation (adversarial agents + Monte-Carlo tournament). No protected-file edits; the only non-`staging/` change is adding the crate to `src/Cargo.toml` members.

**Tech Stack:** Rust 2024, `sha2` (hashing/commitments/toy VM), `rand` (seeded sim RNG), `proptest` (dev — property/conservation tests). All money is `u64` raw units; all fractions are integer **basis points** (`bps`, /10_000) — no floats in the money path, so settlement is deterministic and conservation is exact.

**Spec:** `src/staging/docs/2026-06-10-pouw-verification-game-design.md` (read it first; this plan implements it verbatim).

**Working rules for the executor of this plan:**
- TDD strictly: write the failing test, watch it fail, write minimal code, watch it pass, commit.
- Run from `/home/operator/Coin/src` (the workspace root). Commit on the current agent branch. Use identity `The Commrade <commrade@commputer.xyz>`.
- After each task, `cargo test -p commputer-pouw` must be green before moving on.

---

## File Structure (decomposition)

```
src/staging/pouw/
  Cargo.toml                 # package commputer-pouw; deps sha2, rand; dev proptest
  src/
    lib.rs                   # module decls + crate docs + re-exports
    params.rs                # GameParams (all knobs, bps); Default + invariant check
    ids.rs                   # ParticipantId, JobId, helpers (hashing, JobId derivation)
    job.rs                   # JobSpec, Job, ExecutorClaim, Commitment, Reveal, Challenge, Verdict, SettlementOutcome
    oracle.rs                # ExecutionOracle, EquivalenceOracle, ChainHooks traits + deterministic impls (IteratedHashVm, ByteEq, Ledger)
    committee.rs             # deterministic stake-weighted committee selection from a seed
    commit_reveal.rs         # commitment construction + reveal verification
    verdict.rs               # quorum over reveals via EquivalenceOracle -> Verdict
    settlement.rs            # the 6 terminal settlement branches; integer bps; returns SettlementOutcome
    escalation.rs            # K_escalate panel resolution: challenge path + NoQuorum path
    trap.rs                  # synthetic trap round + trap settlement (jackpot from slashed bonds)
    engine.rs                # drive ONE job through the whole game
  tests/
    conservation.rs          # proptest: every settlement branch balances exactly
  sim/                       # (a [[bin]]) the proof harness
    main.rs                  # tournament entry: prints the EV-negative regime table
    agents.rs                # honest/lazy/cheating executor; honest/rubber-stamp/colluding verifier
    tournament.rs            # Monte-Carlo driver + metrics
  README.md                  # how to run the sim and read its output
```

Each module has one responsibility; `engine.rs` is the only place that knows the full sequence; `settlement.rs` is the only place money moves.

---

## Task 0: Crate skeleton + workspace wiring

**Files:**
- Create: `src/staging/pouw/Cargo.toml`
- Create: `src/staging/pouw/src/lib.rs`
- Modify: `src/Cargo.toml` (add `"staging/pouw"` to `members`)

- [ ] **Step 1: Create the crate manifest**

`src/staging/pouw/Cargo.toml`:
```toml
[package]
name = "commputer-pouw"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Proof-of-useful-work verification & settlement game (deterministic prototype + simulation)"

[dependencies]
sha2 = { workspace = true }

[dev-dependencies]
proptest = "1.4"
rand = { workspace = true }

[[bin]]
name = "pouw-sim"
path = "sim/main.rs"
```

- [ ] **Step 2: Create a minimal lib so it compiles**

`src/staging/pouw/src/lib.rs`:
```rust
//! Proof-of-useful-work verification & settlement game (deterministic prototype).
//! See src/staging/docs/2026-06-10-pouw-verification-game-design.md.
```

- [ ] **Step 3: Add the crate to the workspace members**

In `src/Cargo.toml`, add `"staging/pouw",` to the `members = [ ... ]` array (keep alphabetical-ish; placement does not matter functionally).

- [ ] **Step 4: Verify it builds**

Run: `cd /home/operator/Coin/src && cargo build -p commputer-pouw`
Expected: compiles clean (one crate, no warnings of note).

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/Cargo.toml src/staging/pouw/src/lib.rs src/Cargo.toml
git commit -m "feat(pouw): crate skeleton + workspace member"
```

---

## Task 1: GameParams (all knobs, bps) + invariant check

**Files:**
- Create: `src/staging/pouw/src/params.rs`
- Modify: `src/staging/pouw/src/lib.rs` (add `pub mod params;`)

- [ ] **Step 1: Write the failing test** (append to `params.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_satisfy_invariants() {
        let p = GameParams::default();
        // settlement split sums to 100%
        assert_eq!(p.worker_bps + p.verifier_bps + p.burn_bps, 10_000);
        // escalation reward shares cannot exceed the slashed bond
        assert!(p.challenger_reward_bps + p.escalation_reward_bps <= 10_000);
        // escalation panel is larger than the committee
        assert!(p.k_escalate > p.k);
        // quorum is a real super-majority
        assert!(p.quorum_num * 2 >= p.quorum_den && p.quorum_num <= p.quorum_den);
        // a bond at least covers the value at risk
        assert!(p.executor_bond >= 1);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_rejects_bad_split() {
        let mut p = GameParams::default();
        p.burn_bps += 1; // now sums to 10_001
        assert!(p.validate().is_err());
    }
}
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p commputer-pouw params -- --nocapture`
Expected: FAIL (`GameParams` not found).

- [ ] **Step 3: Implement `GameParams`** (top of `params.rs`)

```rust
/// All tunable knobs of the verification game. Fractions are basis points (/10_000).
#[derive(Clone, Debug)]
pub struct GameParams {
    pub k: usize,                  // proactive committee size
    pub k_escalate: usize,         // escalation panel size (> k)
    pub sample_rate_bps: u32,      // P(a job is proactively verified)
    pub p_trap_bps: u32,           // P(a verification round is a trap)
    pub quorum_num: usize,         // quorum = ceil(quorum_num/quorum_den * committee)
    pub quorum_den: usize,
    pub worker_bps: u32,           // settlement split of a Confirmed budget
    pub verifier_bps: u32,
    pub burn_bps: u32,
    pub executor_bond: u64,        // posted by the executor (>= budget in practice)
    pub verifier_bond: u64,        // posted by each committee/panel verifier
    pub challenger_bond: u64,      // posted by a challenger of an unsampled result
    pub dispute_bounty_bps: u32,   // committee-Disputed: share of slashed Be to honest verifiers
    pub challenger_reward_bps: u32,// challenge-Disputed: share of slashed Be to the challenger
    pub escalation_reward_bps: u32,// escalation: share of slashed bond to the panel
    pub trap_jackpot_bps: u32,     // trap: share of slashed rubber-stamper bonds to honest verifiers
}

impl Default for GameParams {
    fn default() -> Self {
        Self {
            k: 3, k_escalate: 7,
            sample_rate_bps: 10_000,     // start verifying every job; the sim sweeps this down
            p_trap_bps: 1_000,           // 10%
            quorum_num: 2, quorum_den: 3,
            worker_bps: 8_500, verifier_bps: 1_000, burn_bps: 500,
            executor_bond: 100, verifier_bond: 20, challenger_bond: 50,
            dispute_bounty_bps: 2_000,
            challenger_reward_bps: 1_000,
            escalation_reward_bps: 1_000,
            trap_jackpot_bps: 5_000,
        }
    }
}

impl GameParams {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.worker_bps + self.verifier_bps + self.burn_bps != 10_000 {
            return Err("settlement split must sum to 10_000 bps");
        }
        if self.challenger_reward_bps + self.escalation_reward_bps > 10_000 {
            return Err("escalation reward shares exceed the slashed bond");
        }
        if self.k_escalate <= self.k { return Err("k_escalate must exceed k"); }
        if self.quorum_den == 0 || self.quorum_num > self.quorum_den {
            return Err("bad quorum fraction");
        }
        Ok(())
    }

    /// Minimum agreeing votes for a quorum over `committee_size` participants.
    pub fn quorum(&self, committee_size: usize) -> usize {
        // ceil(quorum_num/quorum_den * committee_size)
        (self.quorum_num * committee_size + self.quorum_den - 1) / self.quorum_den
    }
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p commputer-pouw params`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/params.rs src/staging/pouw/src/lib.rs
git commit -m "feat(pouw): GameParams with bps knobs + invariant validation"
```

---

## Task 2: IDs + hashing helpers

**Files:**
- Create: `src/staging/pouw/src/ids.rs`
- Modify: `lib.rs` (`pub mod ids;`)

- [ ] **Step 1: Failing test** (in `ids.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn job_id_is_deterministic_and_input_sensitive() {
        let a = JobId::derive(&[1; 32], &[2; 32], &ParticipantId([3; 32]), 0);
        let b = JobId::derive(&[1; 32], &[2; 32], &ParticipantId([3; 32]), 0);
        let c = JobId::derive(&[1; 32], &[2; 32], &ParticipantId([3; 32]), 1);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
    #[test]
    fn hash_helper_is_stable() {
        assert_eq!(hash2(b"x", b"y"), hash2(b"x", b"y"));
        assert_ne!(hash2(b"x", b"y"), hash2(b"y", b"x"));
    }
}
```

- [ ] **Step 2: Run, verify fail** — `cargo test -p commputer-pouw ids` → FAIL (not found).

- [ ] **Step 3: Implement** (top of `ids.rs`)

```rust
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ParticipantId(pub [u8; 32]);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct JobId(pub [u8; 32]);

/// SHA-256 over a sequence of byte slices.
pub fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts { h.update(p); }
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}
pub fn hash2(a: &[u8], b: &[u8]) -> [u8; 32] { hash_parts(&[a, b]) }

impl JobId {
    pub fn derive(spec_hash: &[u8; 32], input_hash: &[u8; 32], submitter: &ParticipantId, nonce: u64) -> JobId {
        JobId(hash_parts(&[spec_hash, input_hash, &submitter.0, &nonce.to_le_bytes()]))
    }
}
```

- [ ] **Step 4: Run, verify pass** — `cargo test -p commputer-pouw ids` → PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): participant/job ids + hashing helpers"`

---

## Task 3: Core data model

**Files:** Create `src/staging/pouw/src/job.rs`; modify `lib.rs` (`pub mod job;`).

- [ ] **Step 1: Failing test** (in `job.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    #[test]
    fn settlement_outcome_starts_empty() {
        let s = SettlementOutcome::default();
        assert_eq!(s.worker_paid, 0);
        assert_eq!(s.burned, 0);
        assert!(s.slashed.is_empty());
    }
    #[test]
    fn verdict_equality() {
        assert_eq!(Verdict::Confirmed { result_hash: [1; 32] }, Verdict::Confirmed { result_hash: [1; 32] });
        assert_ne!(Verdict::Confirmed { result_hash: [1; 32] }, Verdict::NoQuorum);
    }
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement** — port the §5 data model verbatim (this is the spec's `Data model` section). Include: `JobSpec`, `Job`, `ExecutorClaim`, `Commitment`, `Reveal`, `Challenge`, `Verdict` (derive `PartialEq, Eq, Clone, Debug`), and `SettlementOutcome` (derive `Default, Clone, Debug, PartialEq`). Use `crate::ids::{ParticipantId, JobId}`. Field-for-field match the spec §5 (including `Commitment.bond`, the `challenger_paid`/`panel_paid`/`bonds_returned` fields, and `slashed: Vec<(ParticipantId, u64)>`).

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): core data model (job, claim, commit/reveal, verdict, outcome)"`

---

## Task 4: Oracle seams + deterministic impls (execution, equivalence, ledger)

**Files:** Create `src/staging/pouw/src/oracle.rs`; modify `lib.rs`.

This task delivers the three trait seams **and** the deterministic implementations the prototype runs on. The `Ledger` is the conservation backbone — get it right.

- [ ] **Step 1: Failing tests** (in `oracle.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;

    #[test]
    fn toy_vm_is_deterministic() {
        let vm = IteratedHashVm { rounds: 1000 };
        let spec = crate::job::JobSpec { program_hash: [7; 32], input_hash: [9; 32] };
        assert_eq!(vm.run(&spec, b"in"), vm.run(&spec, b"in"));
        assert_ne!(vm.run(&spec, b"in"), vm.run(&spec, b"other"));
    }

    #[test]
    fn byte_eq_oracle() {
        let eq = ByteEq;
        assert!(eq.equiv(&[1; 32], &[1; 32]));
        assert!(!eq.equiv(&[1; 32], &[2; 32]));
    }

    #[test]
    fn ledger_conserves_total_supply() {
        let a = ParticipantId([1; 32]);
        let b = ParticipantId([2; 32]);
        let mut l = Ledger::new();
        l.credit(a, 100);
        l.credit(b, 50);
        let total0 = l.total_supply();
        // escrow, pay, burn, slash must never change total_supply (no mint)
        l.escrow(a, 40);                 assert_eq!(l.total_supply(), total0);
        l.pay(b, 25);                    assert_eq!(l.total_supply(), total0);
        l.burn(10);                      assert_eq!(l.total_supply(), total0);
        l.slash(b, 5);                   assert_eq!(l.total_supply(), total0);
    }
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement.**

```rust
use crate::ids::ParticipantId;
use crate::job::JobSpec;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Deterministic execution. Prototype: a toy VM. Later: real WASM. The game never looks inside.
pub trait ExecutionOracle { fn run(&self, spec: &JobSpec, input: &[u8]) -> Vec<u8>; }

/// "Are two results equivalent?" Prototype: byte/hash equality. Cycle B/C swap this; the game is unchanged.
pub trait EquivalenceOracle { fn equiv(&self, a: &[u8; 32], b: &[u8; 32]) -> bool; }

/// Abstract stake/value ops. Prototype: in-memory ledger. Later: adapter onto real ChainState.
pub trait ChainHooks {
    fn escrow(&mut self, who: ParticipantId, amount: u64);
    fn pay(&mut self, to: ParticipantId, amount: u64);
    fn burn(&mut self, amount: u64);
    fn slash(&mut self, who: ParticipantId, amount: u64);
    fn stake_of(&self, who: &ParticipantId) -> u64;
}

/// Toy deterministic VM: iterated SHA-256 over (program_hash ‖ input), `rounds` times.
pub struct IteratedHashVm { pub rounds: u32 }
impl ExecutionOracle for IteratedHashVm {
    fn run(&self, spec: &JobSpec, input: &[u8]) -> Vec<u8> {
        let mut cur = Sha256::digest([&spec.program_hash[..], input].concat());
        for _ in 1..self.rounds.max(1) { cur = Sha256::digest(cur); }
        cur.to_vec()
    }
}

pub struct ByteEq;
impl EquivalenceOracle for ByteEq { fn equiv(&self, a: &[u8; 32], b: &[u8; 32]) -> bool { a == b } }

/// In-memory value ledger. `escrow` is held value (still in supply); `burned` leaves supply.
/// total_supply = Σ balances + Σ escrow + burned is INVARIANT across all ops (never minted).
pub struct Ledger {
    balances: HashMap<ParticipantId, u64>,
    escrowed: u64,
    pub burned: u64,
}
impl Ledger {
    pub fn new() -> Self { Self { balances: HashMap::new(), escrowed: 0, burned: 0 } }
    pub fn credit(&mut self, who: ParticipantId, amount: u64) { *self.balances.entry(who).or_insert(0) += amount; }
    pub fn balance_of(&self, who: &ParticipantId) -> u64 { *self.balances.get(who).unwrap_or(&0) }
    pub fn total_supply(&self) -> u64 {
        self.balances.values().sum::<u64>() + self.escrowed + self.burned
    }
}
impl ChainHooks for Ledger {
    fn escrow(&mut self, who: ParticipantId, amount: u64) {
        let b = self.balances.entry(who).or_insert(0);
        *b = b.checked_sub(amount).expect("escrow exceeds balance");
        self.escrowed += amount;                       // moved from balance to escrow; supply unchanged
    }
    fn pay(&mut self, to: ParticipantId, amount: u64) {
        self.escrowed = self.escrowed.checked_sub(amount).expect("pay exceeds escrow");
        *self.balances.entry(to).or_insert(0) += amount;
    }
    fn burn(&mut self, amount: u64) {
        self.escrowed = self.escrowed.checked_sub(amount).expect("burn exceeds escrow");
        self.burned += amount;                          // moved escrow -> burned; supply unchanged
    }
    fn slash(&mut self, who: ParticipantId, amount: u64) {
        let b = self.balances.entry(who).or_insert(0);
        *b = b.checked_sub(amount).expect("slash exceeds balance");
        self.burned += amount;                          // slashed stake is burned
    }
    fn stake_of(&self, who: &ParticipantId) -> u64 { self.balance_of(who) }
}
```

> Note for the implementer: model **bonds** as escrow too — when a participant posts a bond, `escrow` it; on return, `pay` it back to them; on slash, `burn` it. This keeps every bond inside `total_supply` until it is explicitly burned, which is what makes the conservation test (Task 11) exact.

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): execution/equivalence/chain oracle seams + deterministic impls"`

---

## Task 5: Committee selection (deterministic, stake-weighted, post-commit-unpredictable)

**Files:** Create `src/staging/pouw/src/committee.rs`; modify `lib.rs`.

Selection method (integer-only, deterministic): for each candidate `id` with stake `s ≥ 1`, compute `ticket = u128::from_be_bytes(hash(seed ‖ id)[..16])`, then `key = ticket / s`. Sort ascending by `key`; take the first `count` candidates that are **not** the executor. Higher stake ⇒ smaller key ⇒ more likely selected; unknowable before `seed` is revealed. (Monotone-in-stake approximation of VRF sortition; exact proportional sortition is a follow-up.)

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    fn pid(n: u8) -> ParticipantId { ParticipantId([n; 32]) }

    #[test]
    fn selection_is_deterministic_and_excludes_executor() {
        let cands = vec![pid(1), pid(2), pid(3), pid(4), pid(5)];
        let stake = |_: &ParticipantId| 1u64;
        let a = select_committee(&[42; 32], &cands, &pid(3), 3, &stake);
        let b = select_committee(&[42; 32], &cands, &pid(3), 3, &stake);
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
        assert!(!a.contains(&pid(3)));
    }

    #[test]
    fn higher_stake_selected_more_often() {
        // One whale (stake 1000) vs many minnows (stake 1); over many seeds the whale should
        // be selected far more than its 1/N share.
        let mut cands = vec![pid(99)]; // whale
        for n in 1..20u8 { cands.push(pid(n)); }
        let stake = |p: &ParticipantId| if *p == pid(99) { 1000 } else { 1 };
        let mut whale_hits = 0;
        for seed in 0u8..100 {
            let c = select_committee(&[seed; 32], &cands, &pid(200), 3, &stake);
            if c.contains(&pid(99)) { whale_hits += 1; }
        }
        assert!(whale_hits > 60, "whale selected {whale_hits}/100, expected heavy bias");
    }
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**

```rust
use crate::ids::{ParticipantId, hash_parts};

/// Deterministic stake-weighted selection of `count` verifiers from `candidates`,
/// excluding `executor`. `stake_of` returns each candidate's stake (>= 1 assumed; 0 -> treated as 1).
pub fn select_committee(
    seed: &[u8; 32],
    candidates: &[ParticipantId],
    executor: &ParticipantId,
    count: usize,
    stake_of: &dyn Fn(&ParticipantId) -> u64,
) -> Vec<ParticipantId> {
    let mut scored: Vec<(u128, ParticipantId)> = candidates
        .iter()
        .filter(|c| *c != executor)
        .map(|c| {
            let h = hash_parts(&[seed, &c.0]);
            let ticket = u128::from_be_bytes(h[..16].try_into().unwrap());
            let s = stake_of(c).max(1) as u128;
            (ticket / s, *c)
        })
        .collect();
    scored.sort_by_key(|(k, id)| (*k, id.0)); // tie-break on id for determinism
    scored.into_iter().take(count).map(|(_, id)| id).collect()
}
```

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): deterministic stake-weighted committee selection"`

---

## Task 6: Commit-reveal

**Files:** Create `src/staging/pouw/src/commit_reveal.rs`; modify `lib.rs`.

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    use crate::job::{Commitment, Reveal};
    fn pid(n: u8) -> ParticipantId { ParticipantId([n; 32]) }

    #[test]
    fn valid_reveal_matches_its_commitment() {
        let c = make_commitment(&pid(1), &[7; 32], &[3; 32], 20);
        let r = Reveal { verifier: pid(1), result_hash: [7; 32], salt: [3; 32] };
        assert!(reveal_matches(&c, &r));
    }
    #[test]
    fn tampered_reveal_is_rejected() {
        let c = make_commitment(&pid(1), &[7; 32], &[3; 32], 20);
        let bad = Reveal { verifier: pid(1), result_hash: [8; 32], salt: [3; 32] };
        assert!(!reveal_matches(&c, &bad));
    }
    #[test]
    fn commitment_hides_the_result() {
        // Two different results under the same salt/verifier produce different commitments,
        // and the commitment reveals nothing about the result without the salt.
        let a = make_commitment(&pid(1), &[7; 32], &[3; 32], 20);
        let b = make_commitment(&pid(1), &[8; 32], &[3; 32], 20);
        assert_ne!(a.commit, b.commit);
    }
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**

```rust
use crate::ids::{ParticipantId, hash_parts};
use crate::job::{Commitment, Reveal};

/// commit = H(result_hash ‖ salt ‖ verifier). Binding + hiding (salt is secret until reveal).
pub fn make_commitment(verifier: &ParticipantId, result_hash: &[u8; 32], salt: &[u8; 32], bond: u64) -> Commitment {
    Commitment { verifier: *verifier, commit: hash_parts(&[result_hash, salt, &verifier.0]), bond }
}

pub fn reveal_matches(c: &Commitment, r: &Reveal) -> bool {
    r.verifier == c.verifier
        && hash_parts(&[&r.result_hash, &r.salt, &r.verifier.0]) == c.commit
}
```

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): commit-reveal (binding + hiding)"`

---

## Task 7: Verdict (quorum over reveals)

**Files:** Create `src/staging/pouw/src/verdict.rs`; modify `lib.rs`.

Logic: group revealed `result_hash`es by `EquivalenceOracle`; find the largest group; if its size `>= quorum` it is the **committee value**; compare to the executor's claimed hash → `Confirmed`/`Disputed`; else `NoQuorum`.

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    use crate::job::{Reveal, Verdict};
    use crate::oracle::ByteEq;
    use crate::params::GameParams;
    fn rv(n: u8, h: u8) -> Reveal { Reveal { verifier: ParticipantId([n; 32]), result_hash: [h; 32], salt: [0; 32] } }

    #[test]
    fn unanimous_agreeing_with_executor_is_confirmed() {
        let p = GameParams::default();
        let reveals = vec![rv(1, 9), rv(2, 9), rv(3, 9)];
        let v = compute_verdict(&reveals, &[9; 32], p.quorum(3), &ByteEq);
        assert_eq!(v, Verdict::Confirmed { result_hash: [9; 32] });
    }
    #[test]
    fn majority_against_executor_is_disputed() {
        let p = GameParams::default();
        let reveals = vec![rv(1, 5), rv(2, 5), rv(3, 9)]; // committee says 5, executor said 9
        let v = compute_verdict(&reveals, &[9; 32], p.quorum(3), &ByteEq);
        assert_eq!(v, Verdict::Disputed { correct_hash: [5; 32] });
    }
    #[test]
    fn three_way_split_is_no_quorum() {
        let p = GameParams::default();
        let reveals = vec![rv(1, 1), rv(2, 2), rv(3, 3)];
        let v = compute_verdict(&reveals, &[9; 32], p.quorum(3), &ByteEq);
        assert_eq!(v, Verdict::NoQuorum);
    }
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**

```rust
use crate::job::{Reveal, Verdict};
use crate::oracle::EquivalenceOracle;

/// `quorum` = minimum agreeing votes required (from GameParams::quorum(committee_size)).
pub fn compute_verdict(
    reveals: &[Reveal],
    executor_hash: &[u8; 32],
    quorum: usize,
    eq: &dyn EquivalenceOracle,
) -> Verdict {
    // Group reveals into equivalence classes; track each class's representative hash + count.
    let mut classes: Vec<([u8; 32], usize)> = Vec::new();
    for r in reveals {
        match classes.iter_mut().find(|(rep, _)| eq.equiv(rep, &r.result_hash)) {
            Some((_, n)) => *n += 1,
            None => classes.push((r.result_hash, 1)),
        }
    }
    match classes.into_iter().max_by_key(|(_, n)| *n) {
        Some((value, votes)) if votes >= quorum => {
            if eq.equiv(&value, executor_hash) {
                Verdict::Confirmed { result_hash: value }
            } else {
                Verdict::Disputed { correct_hash: value }
            }
        }
        _ => Verdict::NoQuorum,
    }
}
```

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): verdict quorum over reveals"`

---

## Task 8: Settlement — the happy branches (Confirmed sampled / unsampled, committee Disputed)

**Files:** Create `src/staging/pouw/src/settlement.rs`; modify `lib.rs`.

Implement the spec §6.9 splits with integer bps. This task covers the three non-escalation branches; Task 9 adds escalation; Task 10 adds traps. Every function takes a `&mut dyn ChainHooks` and a `&GameParams`, moves value, and returns a `SettlementOutcome` for assertions.

Define a helper `fn bps(amount: u64, bps: u32) -> u64 { (amount as u128 * bps as u128 / 10_000) as u64 }` and always send the **remainder** somewhere explicit (no rounding leak).

- [ ] **Step 1: Failing tests** (assert exact amounts AND ledger conservation)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    use crate::oracle::Ledger;
    use crate::params::GameParams;
    fn pid(n: u8) -> ParticipantId { ParticipantId([n; 32]) }

    #[test]
    fn confirmed_sampled_split_85_10_5() {
        let p = GameParams::default();
        let (worker, v1, v2) = (pid(1), pid(2), pid(3));
        let mut l = Ledger::new();
        // submitter funded the escrow; here we just escrow 100 directly for the test.
        l.credit(pid(0), 100); l.escrow(pid(0), 100);
        let total0 = l.total_supply();
        let out = settle_confirmed_sampled(&mut l, &p, 100, worker, &[v1, v2]);
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.verifiers_paid, 10);
        assert_eq!(out.burned, 5);
        assert_eq!(l.balance_of(&worker), 85);
        assert_eq!(l.total_supply(), total0); // no mint
    }

    #[test]
    fn confirmed_unsampled_burns_the_verifier_slice() {
        let p = GameParams::default();
        let mut l = Ledger::new();
        l.credit(pid(0), 100); l.escrow(pid(0), 100);
        let out = settle_confirmed_unsampled(&mut l, &p, 100, pid(1));
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.burned, 15); // 5% protocol + 10% unclaimed verifier slice
        assert_eq!(out.verifiers_paid, 0);
    }

    #[test]
    fn committee_disputed_refunds_submitter_and_bounties_from_bond() {
        let p = GameParams::default(); // executor_bond 100, dispute_bounty 20%
        let mut l = Ledger::new();
        let (submitter, exec, v1) = (pid(0), pid(9), pid(1));
        l.credit(submitter, 100); l.escrow(submitter, 100);  // budget escrowed
        l.credit(exec, 100); l.escrow(exec, 100);            // executor bond escrowed
        let total0 = l.total_supply();
        let out = settle_committee_disputed(&mut l, &p, 100, submitter, exec, 100, &[v1]);
        assert_eq!(out.submitter_refunded, 100);
        assert_eq!(out.verifiers_paid, 20);   // 20% of the 100 bond
        assert_eq!(out.burned, 80);           // remaining bond burned
        assert_eq!(l.balance_of(&submitter), 100);
        assert_eq!(l.total_supply(), total0);
    }
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement** the three functions. Each: pull the named amounts out of escrow via `ChainHooks`, split with `bps`, route the remainder to `burn`, and record into a `SettlementOutcome`. Signatures:

```rust
pub fn bps(amount: u64, bps: u32) -> u64 { (amount as u128 * bps as u128 / 10_000) as u64 }

pub fn settle_confirmed_sampled(l: &mut dyn ChainHooks, p: &GameParams, budget: u64,
    worker: ParticipantId, verifiers: &[ParticipantId]) -> SettlementOutcome { /* 85/10/5; split the 10% evenly, remainder of the even split burned */ }

pub fn settle_confirmed_unsampled(l: &mut dyn ChainHooks, p: &GameParams, budget: u64,
    worker: ParticipantId) -> SettlementOutcome { /* worker 85%, burn the rest (15%) */ }

pub fn settle_committee_disputed(l: &mut dyn ChainHooks, p: &GameParams, budget: u64,
    submitter: ParticipantId, executor: ParticipantId, executor_bond: u64,
    honest_verifiers: &[ParticipantId]) -> SettlementOutcome {
    /* refund submitter `budget` from escrow; bounty = bps(executor_bond, dispute_bounty_bps) to honest
       verifiers; burn the rest of executor_bond; record slashed(executor, executor_bond). */
}
```

> Rounding rule (apply everywhere): compute each named share with `bps`, pay it, and **burn whatever is left in escrow for that pool** so no unit is ever minted or stranded. The conservation test in Task 11 enforces this.

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): settlement — confirmed (sampled/unsampled) + committee disputed"`

---

## Task 9: Settlement — escalation branches (challenge path + NoQuorum path)

**Files:** Modify `src/staging/pouw/src/settlement.rs`; create `src/staging/pouw/src/escalation.rs`; modify `lib.rs`.

Implement the four escalation outcomes from spec §6.9: `Disputed`-via-challenge, false-challenge (`Confirmed`-via-challenge), `NoQuorum`→`Confirmed`, `NoQuorum`→`Disputed`. `escalation.rs` runs the panel (reuse `compute_verdict` with `k_escalate`) and calls the matching settlement function.

- [ ] **Step 1: Failing tests** — one per branch, asserting exact splits + `total_supply` invariance. Mirror Task 8's style. Key assertions:
  - `Disputed`-via-challenge (`Be`=100, challenger_reward 10%, escalation_reward 10%): submitter refunded 100; challenger gets bond back + 10; panel gets 10; burn 80.
  - false-challenge (`Bc`=50): challenger loses 50; panel gets `bps(50, escalation_reward_bps)=5`; burn 45; worker still 85/burn-the-10/5.
  - `NoQuorum`→`Confirmed`: worker 85/10/5; wrong-side committee bonds slashed → panel reward + burn; no `Bc`.
  - `NoQuorum`→`Disputed`: submitter refunded; `Be` split `(challenger_reward+escalation_reward)` to honest verifiers+panel, rest burned.

- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** the four settlement fns + `escalation::resolve(...)` which: selects the panel (`select_committee` with `k_escalate`), runs `compute_verdict`, then dispatches to the correct settlement fn. Keep all arithmetic in `bps`; route remainders to burn.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): settlement + escalation (challenge & NoQuorum paths)"`

---

## Task 10: Trap rounds

**Files:** Create `src/staging/pouw/src/trap.rs`; modify `lib.rs`.

A trap is synthetic (no budget): the protocol presents a known-wrong claim. Rubber-stampers (revealed the planted wrong answer) are slashed; honest verifiers split `trap_jackpot_bps` of the slashed bonds; remainder burned.

- [ ] **Step 1: Failing tests**
  - Rubber-stamper slashed, honest verifier gets jackpot, conservation holds.
  - No rubber-stampers ⇒ no slash, no jackpot, no mint.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** `trap::settle_trap(l, p, planted_wrong: [u8;32], true_answer: [u8;32], reveals: &[Reveal], bonds: &dyn Fn(&ParticipantId)->u64) -> SettlementOutcome`. Classify each reveal: matches `planted_wrong` ⇒ rubber-stamper (slash bond); matches `true_answer` ⇒ honest (jackpot share). Jackpot = `bps(total_slashed, trap_jackpot_bps)`; remainder burned.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): trap rounds (jackpot funded only from slashed bonds)"`

---

## Task 11: Conservation property test (proptest, all branches)

**Files:** Create `src/staging/pouw/tests/conservation.rs`.

The single most important test: for randomized budgets/bonds/participant sets, drive **each** settlement branch and assert the §9 identity holds via `Ledger::total_supply()` being invariant from before-escrow to after-settle, and that `SettlementOutcome` fields sum to the inflows.

- [ ] **Step 1: Write the proptest** covering all 6 branches (a strategy that picks a branch + random amounts within valid ranges), each asserting `total_supply` unchanged and outflow-fields == inflows.
- [ ] **Step 2: Run, verify it fails** (until any off-by-one in Task 8-10 is fixed) — `cargo test -p commputer-pouw --test conservation`.
- [ ] **Step 3: Fix any settlement arithmetic the property surfaces** (this is why the rounding rule routes remainders to burn).
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "test(pouw): conservation property across all settlement branches"`

---

## Task 12: Engine — drive one job end-to-end

**Files:** Create `src/staging/pouw/src/engine.rs`; modify `lib.rs`.

`engine::run_job(...)` wires the phases: escrow + bond → execute (oracle) → sample? (seed + sample_rate) → commit-reveal (committee) → verdict → escalate on NoQuorum / challenge → settle. Returns `(Verdict, SettlementOutcome)`. For the prototype, randomness (sample? trap? salts) comes from a seeded RNG passed in, so runs are reproducible.

- [ ] **Step 1: Failing test** — an all-honest job (honest executor + honest committee, sampled) returns `Confirmed` and an 85/10/5 settlement, `total_supply` invariant.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** `run_job`, composing the prior modules. Keep it a thin orchestrator — no money logic here (that all lives in `settlement.rs`).
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): engine orchestrates one job through the full game"`

---

## Task 13: Simulation — adversarial agents

**Files:** Create `src/staging/pouw/sim/agents.rs`.

Agent strategies as enums/closures: `Executor::{Honest, Lazy(returns garbage), Cheat(plausible-wrong)}`; `Verifier::{Honest, RubberStamp(echo executor), Collude(with a named executor)}`. Each exposes "what result_hash do I reveal for this job?".

- [ ] **Step 1: Failing tests** — each strategy reveals what the spec says (honest = true hash; rubber-stamp = executor's claimed hash; lazy executor = a wrong hash).
- [ ] **Step 2-4:** Implement + verify.
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): simulation agent strategies"`

---

## Task 14: Simulation — Monte-Carlo tournament + the proof

**Files:** Create `src/staging/pouw/sim/tournament.rs` and `src/staging/pouw/sim/main.rs`.

Run many jobs with a configured mix of agent strategies and a seeded RNG; accumulate per-strategy net profit (rewards − bonds lost − compute cost) and global metrics (% cheats caught). The deliverable claim: **honest executor profit > 0 ≥ cheating executor profit**, and **rubber-stamp verifier profit < honest verifier profit**, within the default params.

- [ ] **Step 1: Failing test** (`tournament.rs` unit test) — over N=5000 seeded jobs with default params, assert `cheat_executor_ev <= 0 < honest_executor_ev` and `rubber_stamp_ev < honest_verifier_ev`.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** the tournament loop + metrics; `main.rs` runs a default tournament and prints a small table (strategy, jobs, caught %, net EV). If the assertion can't be met at default params, **tune `p_trap_bps`/`executor_bond`/`sample_rate_bps`** until it holds, and record the regime in the README — that regime is the deliverable.
- [ ] **Step 4: Run, verify pass** — `cargo test -p commputer-pouw` (all) green; `cargo run -p commputer-pouw --bin pouw-sim` prints the table.
- [ ] **Step 5: Commit** — `git commit -am "feat(pouw): Monte-Carlo tournament proving cheating is EV-negative"`

---

## Task 15: README + final verification + founder-review note

**Files:** Create `src/staging/pouw/README.md`; append to the session review doc.

- [ ] **Step 1:** Write `README.md` — what the crate is, how to run the sim (`cargo run -p commputer-pouw --bin pouw-sim`), how to read the table, the identified safe-parameter regime, and the seams (where a real WASM runtime / approximate equivalence / chain adapter plug in). Reference the spec.
- [ ] **Step 2:** Run the full workspace check: `cargo build -p commputer-pouw && cargo test -p commputer-pouw` — green; then `cargo build --workspace` — confirm adding the crate didn't break the workspace.
- [ ] **Step 3:** Append a short entry to `src/staging/WIRE_SESSION_REVIEW_2026-06-10.md` (non-protected) flagging the prototype for founder review: what it proves, what's still mocked (execution oracle, modeled VRF, in-memory ledger), and the on-chain wiring that remains founder-only.
- [ ] **Step 4: Commit** — `git commit -am "docs(pouw): README + founder-review note; final verification"`

---

## Done criteria (matches spec §12)
1. `cargo test -p commputer-pouw` green (unit + conservation property).
2. `cargo run -p commputer-pouw --bin pouw-sim` prints a regime where every modeled cheating strategy is EV-negative.
3. The three trait seams exist; a stub "approximate" `EquivalenceOracle` compiles against the same engine (prove the seam — add as a tiny test).
4. Zero protected-file edits; only `src/Cargo.toml` gained the member.
5. README explains how to run + read the sim.
