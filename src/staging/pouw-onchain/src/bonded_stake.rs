//! Module 10 — the standing bonded/slashable stake source (blueprint G4, the last open design gate).
//!
//! The committee-selection weight (`stake_of`) + a slash surface, backed by LOCKED tokens that cannot
//! be spent while bonded and cannot be pulled out to dodge a slash (PoS-style: bond → cooldown unbond
//! → withdraw; slashable throughout the cooldown). Distinct from the per-job posted bonds (those stay
//! escrowed per job in `EscrowLedger`, P1). Does NOT impl `ChainHooks` (whose slash/stake_of are
//! spendable-balance semantics); the game consumes it via a closure `|p| stake.stake_of(p)` into the
//! frozen `committee::select_committee`.
//!
//! Conservation: `total_supply = Σ balances + Σ bonded + Σ unbonding + burned`, INVARIANT across every
//! op (`credit` is the sole mint). WIRE-IN (founder): unify with `EscrowLedger` into one account in
//! `storage/state.rs`; add `StakeParams` to the genesis `ConsensusParams`; filter the committee pool
//! by `is_eligible` then weight by `stake_of` in `event_loop.rs` (P2 patch-spec committee-draw step).

use commputer_pouw::ids::ParticipantId;
use std::collections::HashMap;

/// Genesis-anchored staking params (join the P3 ConsensusParams bundle at wire-in).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StakeParams {
    /// Cooldown length (blocks) before unbonded stake is withdrawable.
    pub unbonding_blocks: u64,
    /// Minimum ACTIVE bond to be eligible for committee selection.
    pub min_bond: u64,
}

impl Default for StakeParams {
    fn default() -> Self {
        // placeholders — the founder sets the real genesis values.
        Self { unbonding_blocks: 100, min_bond: 1_000 }
    }
}

/// One unbonding request in its cooldown window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UnbondingChunk {
    amount: u64,
    matures_at: u64, // block height at/after which this chunk is withdrawable
}

/// Why a stake op was rejected (the source bucket was short — no state changed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StakeError {
    InsufficientBalance { who: ParticipantId, have: u64, want: u64 },
    InsufficientBonded { who: ParticipantId, have: u64, want: u64 },
}

/// Standing bonded-stake ledger. `total_supply = Σ balances + Σ bonded + Σ unbonding + burned`,
/// INVARIANT across every op (`credit` is the sole mint).
pub struct BondedStake {
    balances: HashMap<ParticipantId, u64>,                  // spendable
    bonded: HashMap<ParticipantId, u64>,                    // active: selectable + slashable
    unbonding: HashMap<ParticipantId, Vec<UnbondingChunk>>, // cooldown: slashable, NOT selectable
    burned: u64,
    params: StakeParams,
}

impl BondedStake {
    pub fn new(params: StakeParams) -> Self {
        Self {
            balances: HashMap::new(),
            bonded: HashMap::new(),
            unbonding: HashMap::new(),
            burned: 0,
            params,
        }
    }

    /// Mint into spendable balance — the SOLE mint (funding only).
    pub fn credit(&mut self, who: ParticipantId, amount: u64) {
        *self.balances.entry(who).or_insert(0) += amount;
    }

    pub fn balance_of(&self, who: &ParticipantId) -> u64 {
        *self.balances.get(who).unwrap_or(&0)
    }

    pub fn bonded_of(&self, who: &ParticipantId) -> u64 {
        *self.bonded.get(who).unwrap_or(&0)
    }

    pub fn unbonding_of(&self, who: &ParticipantId) -> u64 {
        self.unbonding.get(who).map(|v| v.iter().map(|c| c.amount).sum()).unwrap_or(0)
    }

    pub fn total_supply(&self) -> u64 {
        self.balances.values().sum::<u64>()
            + self.bonded.values().sum::<u64>()
            + self.unbonding.values().flatten().map(|c| c.amount).sum::<u64>()
            + self.burned
    }

    /// balance → bonded. Err(InsufficientBalance) if balance < amount (no state change).
    pub fn bond(&mut self, who: ParticipantId, amount: u64) -> Result<(), StakeError> {
        let have = self.balance_of(&who);
        if have < amount {
            return Err(StakeError::InsufficientBalance { who, have, want: amount });
        }
        *self.balances.entry(who).or_insert(0) -= amount;
        *self.bonded.entry(who).or_insert(0) += amount;
        Ok(())
    }

    /// bonded → a cooldown chunk maturing at `now + unbonding_blocks`. The amount immediately stops
    /// counting toward stake_of/selection but stays slashable. Err(InsufficientBonded) if short.
    pub fn request_unbond(&mut self, who: ParticipantId, amount: u64, now: u64) -> Result<(), StakeError> {
        if amount == 0 {
            return Ok(()); // no-op: a zero-amount unbond would only bloat state with empty chunks
        }
        let have = self.bonded_of(&who);
        if have < amount {
            return Err(StakeError::InsufficientBonded { who, have, want: amount });
        }
        *self.bonded.entry(who).or_insert(0) -= amount;
        self.unbonding.entry(who).or_default().push(UnbondingChunk {
            amount,
            matures_at: now.saturating_add(self.params.unbonding_blocks),
        });
        Ok(())
    }

    /// Move all matured cooldown chunks (`matures_at <= now`) back to spendable balance; returns the
    /// total withdrawn (0 if none matured). Saturating; never errors.
    pub fn withdraw(&mut self, who: ParticipantId, now: u64) -> u64 {
        let chunks = match self.unbonding.get_mut(&who) {
            Some(c) => c,
            None => return 0,
        };
        let mut withdrawn = 0u64;
        chunks.retain(|c| {
            if c.matures_at <= now {
                withdrawn += c.amount;
                false
            } else {
                true
            }
        });
        if chunks.is_empty() {
            self.unbonding.remove(&who);
        }
        if withdrawn > 0 {
            *self.balances.entry(who).or_insert(0) += withdrawn;
        }
        withdrawn
    }

    /// Slash up to `amount` of `who`'s AT-RISK stake — bonded FIRST, then cooldown chunks in order —
    /// burning it. Anti-dodge: cooldown stake is reachable, so unbonding before a slash does not
    /// escape it. Returns the amount actually slashed (capped at total at-risk = bonded + Σ unbonding).
    pub fn slash(&mut self, who: ParticipantId, amount: u64) -> u64 {
        let mut remaining = amount;
        let mut slashed = 0u64;
        // bonded first (get_mut, not entry, so slashing a never-bonded account creates no 0 entry)
        if let Some(b) = self.bonded.get_mut(&who) {
            let take = remaining.min(*b);
            *b -= take;
            slashed += take;
            remaining -= take;
        }
        // then cooldown chunks in stored order
        if remaining > 0
            && let Some(chunks) = self.unbonding.get_mut(&who)
        {
            for c in chunks.iter_mut() {
                if remaining == 0 {
                    break;
                }
                let take = remaining.min(c.amount);
                c.amount -= take;
                slashed += take;
                remaining -= take;
            }
            chunks.retain(|c| c.amount > 0);
            if chunks.is_empty() {
                self.unbonding.remove(&who);
            }
        }
        self.burned += slashed;
        slashed
    }

    /// Committee-selection weight = ACTIVE bonded only (cooldown excluded — it is leaving).
    pub fn stake_of(&self, who: &ParticipantId) -> u64 {
        self.bonded_of(who)
    }

    /// Eligible for selection iff active bonded >= min_bond (the candidate-pool filter applied BEFORE
    /// `select_committee`, which weights by `stake_of`).
    pub fn is_eligible(&self, who: &ParticipantId) -> bool {
        self.bonded_of(who) >= self.params.min_bond
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    #[test]
    fn credit_mints_into_balance_and_conserves() {
        let mut s = BondedStake::new(StakeParams::default());
        s.credit(pid(1), 5_000);
        s.credit(pid(2), 2_000);
        assert_eq!(s.balance_of(&pid(1)), 5_000);
        assert_eq!(s.bonded_of(&pid(1)), 0);
        assert_eq!(s.unbonding_of(&pid(1)), 0);
        assert_eq!(s.total_supply(), 7_000); // sum of credits = total supply (the conserved quantity)
    }

    #[test]
    fn bond_moves_balance_to_bonded() {
        let mut s = BondedStake::new(StakeParams::default());
        s.credit(pid(1), 5_000);
        let total0 = s.total_supply();
        assert_eq!(s.bond(pid(1), 3_000), Ok(()));
        assert_eq!(s.balance_of(&pid(1)), 2_000);
        assert_eq!(s.bonded_of(&pid(1)), 3_000);
        assert_eq!(s.total_supply(), total0); // conserved
        // over-balance bond rejected, no state change
        assert_eq!(
            s.bond(pid(1), 9_999),
            Err(StakeError::InsufficientBalance { who: pid(1), have: 2_000, want: 9_999 })
        );
        assert_eq!(s.balance_of(&pid(1)), 2_000);
        assert_eq!(s.bonded_of(&pid(1)), 3_000);
    }

    #[test]
    fn request_unbond_moves_bonded_to_cooldown() {
        let mut s = BondedStake::new(StakeParams { unbonding_blocks: 100, min_bond: 1_000 });
        s.credit(pid(1), 5_000);
        s.bond(pid(1), 5_000).unwrap();
        let total0 = s.total_supply();
        assert_eq!(s.request_unbond(pid(1), 2_000, 50), Ok(()));
        assert_eq!(s.bonded_of(&pid(1)), 3_000); // dropped
        assert_eq!(s.unbonding_of(&pid(1)), 2_000); // in cooldown (matures at 50+100=150)
        assert_eq!(s.total_supply(), total0);
        // over-bonded unbond rejected
        assert_eq!(
            s.request_unbond(pid(1), 9_999, 50),
            Err(StakeError::InsufficientBonded { who: pid(1), have: 3_000, want: 9_999 })
        );
        // a zero-amount unbond is a no-op (no empty cooldown chunk created)
        let unbonding_before = s.unbonding_of(&pid(1));
        assert_eq!(s.request_unbond(pid(1), 0, 50), Ok(()));
        assert_eq!(s.unbonding_of(&pid(1)), unbonding_before, "zero unbond pushed no chunk");
    }

    #[test]
    fn withdraw_respects_maturity() {
        let mut s = BondedStake::new(StakeParams { unbonding_blocks: 100, min_bond: 1_000 });
        s.credit(pid(1), 5_000);
        s.bond(pid(1), 5_000).unwrap();
        s.request_unbond(pid(1), 1_000, 10).unwrap(); // matures at 110
        s.request_unbond(pid(1), 2_000, 50).unwrap(); // matures at 150
        let total0 = s.total_supply();
        // before any maturity
        assert_eq!(s.withdraw(pid(1), 109), 0);
        assert_eq!(s.unbonding_of(&pid(1)), 3_000);
        // first chunk matured, second not
        assert_eq!(s.withdraw(pid(1), 110), 1_000);
        assert_eq!(s.balance_of(&pid(1)), 1_000); // 5000 bonded - 3000 unbonded + 1000 back
        assert_eq!(s.unbonding_of(&pid(1)), 2_000);
        // second chunk matured
        assert_eq!(s.withdraw(pid(1), 200), 2_000);
        assert_eq!(s.unbonding_of(&pid(1)), 0);
        assert_eq!(s.balance_of(&pid(1)), 3_000);
        assert_eq!(s.total_supply(), total0); // conserved throughout
    }

    #[test]
    fn slash_anti_dodge_reaches_cooldown_stake() {
        let mut s = BondedStake::new(StakeParams { unbonding_blocks: 100, min_bond: 1_000 });
        s.credit(pid(1), 5_000);
        s.bond(pid(1), 5_000).unwrap();
        s.request_unbond(pid(1), 5_000, 10).unwrap(); // ALL stake now in cooldown (matures 110)
        let total0 = s.total_supply();
        assert_eq!(s.bonded_of(&pid(1)), 0);
        assert_eq!(s.unbonding_of(&pid(1)), 5_000);
        // slash still reaches the cooldown stake (anti-dodge)
        assert_eq!(s.slash(pid(1), 4_000), 4_000);
        assert_eq!(s.unbonding_of(&pid(1)), 1_000);
        assert_eq!(s.total_supply(), total0); // slashed value moved to burned, conserved
        // a later withdraw only returns what survived the slash
        assert_eq!(s.withdraw(pid(1), 110), 1_000);
        assert_eq!(s.total_supply(), total0);
    }

    #[test]
    fn slash_bonded_first_then_caps() {
        let mut s = BondedStake::new(StakeParams { unbonding_blocks: 100, min_bond: 1_000 });
        s.credit(pid(1), 5_000);
        s.bond(pid(1), 5_000).unwrap();
        s.request_unbond(pid(1), 2_000, 10).unwrap(); // 3_000 bonded, 2_000 cooldown
        let total0 = s.total_supply();
        // partial slash hits bonded first
        assert_eq!(s.slash(pid(1), 1_000), 1_000);
        assert_eq!(s.bonded_of(&pid(1)), 2_000);
        assert_eq!(s.unbonding_of(&pid(1)), 2_000);
        // slash beyond total at-risk (now 4_000) burns everything and returns the cap
        assert_eq!(s.slash(pid(1), 10_000), 4_000);
        assert_eq!(s.bonded_of(&pid(1)), 0);
        assert_eq!(s.unbonding_of(&pid(1)), 0);
        assert_eq!(s.total_supply(), total0);
        // slashing an actor with nothing is a no-op
        assert_eq!(s.slash(pid(9), 100), 0);
    }

    #[test]
    fn stake_of_excludes_unbonding_and_eligibility_floors_at_min_bond() {
        let mut s = BondedStake::new(StakeParams { unbonding_blocks: 100, min_bond: 1_000 });
        s.credit(pid(1), 5_000);
        s.bond(pid(1), 5_000).unwrap();
        assert_eq!(s.stake_of(&pid(1)), 5_000);
        assert!(s.is_eligible(&pid(1)));
        s.request_unbond(pid(1), 4_500, 10).unwrap(); // active bonded now 500
        assert_eq!(s.stake_of(&pid(1)), 500); // cooldown excluded from selection weight
        assert_eq!(s.unbonding_of(&pid(1)), 4_500); // but still at-risk
        assert!(!s.is_eligible(&pid(1))); // 500 < min_bond 1_000
        // eligibility boundary
        let mut s2 = BondedStake::new(StakeParams { unbonding_blocks: 100, min_bond: 1_000 });
        s2.credit(pid(2), 1_000);
        s2.bond(pid(2), 1_000).unwrap();
        assert!(s2.is_eligible(&pid(2))); // == min_bond
    }

    #[test]
    fn every_op_preserves_total_supply() {
        let mut s = BondedStake::new(StakeParams { unbonding_blocks: 50, min_bond: 1_000 });
        s.credit(pid(1), 10_000);
        let total0 = s.total_supply();
        assert_eq!(total0, 10_000); // credit IS the mint; record after funding
        s.bond(pid(1), 8_000).unwrap();
        assert_eq!(s.total_supply(), total0, "bond: balance → bonded");
        s.request_unbond(pid(1), 3_000, 5).unwrap();
        assert_eq!(s.total_supply(), total0, "request_unbond: bonded → unbonding");
        s.slash(pid(1), 2_000);
        assert_eq!(s.total_supply(), total0, "slash: bonded → burned");
        s.withdraw(pid(1), 1_000);
        assert_eq!(s.total_supply(), total0, "withdraw: matured unbonding → balance");
    }

    #[test]
    fn stake_of_drives_select_committee_weighting() {
        use commputer_pouw::committee::select_committee;

        let mut s = BondedStake::new(StakeParams { unbonding_blocks: 100, min_bond: 1_000 });
        // a whale (large bond) + many min-bonded minnows + one ineligible (below min_bond).
        let whale = pid(99);
        s.credit(whale, 1_000_000);
        s.bond(whale, 1_000_000).unwrap();
        let minnows: Vec<ParticipantId> = (1u8..20).map(pid).collect();
        for m in &minnows {
            s.credit(*m, 1_000);
            s.bond(*m, 1_000).unwrap();
        }
        let ineligible = pid(50);
        s.credit(ineligible, 500);
        s.bond(ineligible, 500).unwrap(); // 500 < min_bond → filtered out

        // candidate pool = eligible only (the founder-side filter), then weight by stake_of.
        let mut pool: Vec<ParticipantId> = std::iter::once(whale).chain(minnows.iter().copied()).collect();
        pool.push(ineligible);
        let eligible: Vec<ParticipantId> = pool.iter().copied().filter(|p| s.is_eligible(p)).collect();
        assert!(!eligible.contains(&ineligible), "sub-min-bond candidate filtered out");

        let stake_of = |p: &ParticipantId| s.stake_of(p);
        let executor = pid(200); // not in the pool
        let mut whale_hits = 0;
        for seed in 0u8..100 {
            let c = select_committee(&[seed; 32], &eligible, &executor, 3, &stake_of);
            if c.contains(&whale) {
                whale_hits += 1;
            }
        }
        assert!(whale_hits > 60, "the heavily-bonded whale is selected far more often ({whale_hits}/100)");
    }
}
