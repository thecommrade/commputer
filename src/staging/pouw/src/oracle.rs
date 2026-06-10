//! The three trait seams the game runs on — execution, equivalence, and chain
//! ops — plus the deterministic prototype implementations (`IteratedHashVm`,
//! `ByteEq`, `Ledger`). The `Ledger` is the conservation backbone (spec §9):
//! `total_supply` is invariant across every op (no mint).

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
    /// Value currently held in escrow (still in supply, not yet paid out or burned).
    /// After a complete settlement this must be 0 — any non-zero remainder is value
    /// stranded in escrow, the exact failure mode burning the rounding remainder prevents.
    pub fn escrowed(&self) -> u64 { self.escrowed }
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
    /// Debits **un-escrowed balance** and burns it. NOTE: this is NOT used in the
    /// settlement money path — bonds there are already in escrow, so settlement
    /// moves them with `pay`/`burn`. This exists only to match the spec's chain-ops
    /// surface and applies solely to un-escrowed stake. Do not wire it into settlement.
    fn slash(&mut self, who: ParticipantId, amount: u64) {
        let b = self.balances.entry(who).or_insert(0);
        *b = b.checked_sub(amount).expect("slash exceeds balance");
        self.burned += amount;                          // slashed stake is burned
    }
    fn stake_of(&self, who: &ParticipantId) -> u64 { self.balance_of(who) }
}

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
