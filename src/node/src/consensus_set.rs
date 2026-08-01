//! Who is allowed to participate in CONSENSUS — one derivation, used everywhere.
//!
//! WHAT: the set of addresses eligible to produce blocks and have their block
//! candidates admitted to a round.
//! WHERE IT IS WIRED: `event_loop.rs` derives the leader schedule from this on
//! BOTH the validation side (`validate_block_from_peer`) and the production
//! side. Both MUST derive the same set or they disagree about who the valid
//! leader is.
//! WHY IT LIVES HERE: `event_loop.rs` is protected and needs a live EventLoop to
//! test. This rule is a pure function of committed chain state, so it is
//! testable in isolation — and it is the rule that decides whether opening
//! consensus to strangers is safe.
//!
//! THE SYBIL PROBLEM IT SOLVES. Vote selection is deterministic
//! lowest-hash-among-candidates, and producer keypairs are free to mint. While
//! the validator set is PINNED to the founder nodes that is harmless. The
//! moment outsiders can join, an attacker can spin up unlimited identities,
//! flood candidates, and grind headers until one of theirs wins every round —
//! capturing block production for free. Registration alone cannot gate this:
//! it is free and automatic.
//!
//! The gate is therefore BONDED STAKE, which cannot be minted: an identity
//! counts for consensus only if it has `min_consensus_bond` genuinely bonded.
//! Sybil identities then cost real balance per identity instead of nothing,
//! and the honest-majority assumption rests on stake rather than on key count.


/// Is `addr` eligible to take part in consensus?
///
/// The gate depends on WHICH REGIME is in force, and the two must not be
/// ANDed — that would get it backwards in both directions:
///
///  * **Allowlist active** (`pin_active`, i.e. the compiled list is non-empty):
///    the founder allowlist IS the alpha trust anchor. Membership alone
///    decides; no stake floor. The listed nodes are trusted by construction,
///    and imposing a stake floor on them would mean a slash or an unbond could
///    silently eject a founder node and stall the live chain.
///
///  * **Allowlist retired** (empty): anyone may join, so the Sybil gate must be
///    live — `bonded >= min_consensus_bond`. This is the regime that makes
///    opening the set safe.
///
/// Note `is_pinned_validator` answers TRUE FOR EVERYONE when the list is empty
/// (it means "not restricted"), so `is_validator && pinned && staked` would
/// drop the stake requirement precisely when the set opens — exactly when it
/// is needed. Hence `pin_active` is a separate input and the regimes are
/// exclusive.
pub fn is_consensus_eligible(
    is_validator: bool,
    bonded: u64,
    min_consensus_bond: u64,
    pin_active: bool,
    pinned: bool,
) -> bool {
    if !is_validator {
        return false;
    }
    if pin_active {
        // Alpha: the allowlist is the trust anchor.
        pinned
    } else {
        // Open: unmintable stake is the trust anchor.
        bonded >= min_consensus_bond
    }
}

/// Minimum bonded stake to take part in consensus.
///
/// ⚠ THIS IS A SPAM FLOOR, **NOT** THE SYBIL GATE. Do not treat it as one.
///
/// Research (2026-07-30, see the reference library) demolished the idea that a
/// flat floor can provide Sybil resistance at our emission rate:
///   * `INITIAL_BLOCK_REWARD` ≈ 15.855 COMME, so 1 COMME is **6.3% of a single
///     block reward** — an identity pays for itself in ~0.15 s of production.
///   * At ~2.4 s blocks and n=3, a validator earns ~190,000 COMME/day. Even a
///     10,000-COMME floor is ~1.3 hours of one validator's revenue.
///   * NO surveyed chain (Cosmos, Ethereum, Polkadot, Avalanche, Solana, NEAR,
///     Hyperliquid) relies on a flat minimum alone.
///
/// **RULE LEARNED: denominate any floor in TIME-TO-EARN-BACK, never in tokens.**
///
/// The real Sybil gate is two things this constant cannot substitute for:
///   1. **Stake-weighted selection** — if proposal share is linear in bonded
///      stake, splitting 10 COMME across 10 identities yields exactly the same
///      share as one 10-COMME identity, so minting keypairs gains *nothing*.
///      Our `leader_for_height` is currently count-based (`height % n`), which
///      with a flat uncapped floor is the one configuration nobody ships.
///   2. **A cap (top-N by stake)**, which turns the floor into a
///      market-clearing seat price instead of a constant someone guessed.
/// Both are the remaining step-3 work; until they land, the ALLOWLIST is what
/// is actually holding the line, not this number.
///
/// Consensus-visible: every node must use the same value or they derive
/// different validator sets and disagree on the leader. Changing it is a
/// coordinated upgrade, not a config knob.
#[cfg(not(feature = "formation-test"))]
pub const MIN_CONSENSUS_BOND: u64 = commputer_core::token::UNITS_PER_COMME;

/// Never let the consensus set fall below this. Below it, the derivation
/// returns a FALLBACK set rather than a short one.
///
/// An empty eligible set is an unrecoverable halt: `leader_for_height` returns
/// `None`, no address is a legal producer, and the chain cannot make the block
/// that would fix the situation. Cosmos states it plainly — "a chain cannot
/// produce a block without a validator set" — and Polkadot encodes the same
/// idea as `MinimumValidatorCount` triggering emergency conditions.
///
/// Reachable without any attacker: a mass unbond, a slash cascade, or a
/// bond-accounting bug. The derivation must therefore be TOTAL.
pub const MIN_CONSENSUS_SET: usize = 1;

/// Formation-harness builds run with the allowlist EMPTIED, which puts them in
/// the open regime where the stake gate applies. The harness nodes mine but
/// never bond, so a real floor would empty the consensus set, silently switch
/// the leader check off, and gut exactly what those scenarios exercise
/// (rotation, view change, runaway detection).
///
/// The floor is therefore 0 here: eligibility reduces to `is_validator`, which
/// is precisely the pre-change harness semantics, so the scenarios keep testing
/// consensus mechanics rather than stake economics. The REAL floor still ships
/// in the production binary. Exercising the stake gate itself needs a scenario
/// whose nodes actually bond — that belongs with opening the set (step 4), and
/// is filed as such.
#[cfg(feature = "formation-test")]
pub const MIN_CONSENSUS_BOND: u64 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed floor for the LOGIC tests, deliberately independent of the
    /// shipped constant (which is 0 under `formation-test`, where `MIN - 1`
    /// would underflow and the "under the floor" cases would be vacuous).
    /// The rule is a pure function of its inputs; the shipped value is
    /// asserted separately by `the_floor_costs_real_stake`.
    const MIN: u64 = 100_000_000;

    /// THE regime that matters for opening the set: with the allowlist retired,
    /// registration is free so it must not be enough — unmintable stake is the
    /// only thing standing between us and unlimited Sybil identities.
    #[test]
    fn open_regime_requires_real_stake() {
        // pin_active = false (allowlist retired), pinned is then meaningless.
        assert!(
            !is_consensus_eligible(true, 0, MIN, false, true),
            "a registered but unbonded identity must not participate once the set is open — \
             otherwise Sybil identities cost nothing"
        );
        assert!(!is_consensus_eligible(true, MIN - 1, MIN, false, true), "under the floor is out");
        assert!(is_consensus_eligible(true, MIN, MIN, false, true), "exactly the floor is in");
        assert!(!is_consensus_eligible(false, MIN, MIN, false, true), "must be a registered validator");
    }

    /// While the allowlist is in force it alone decides, and NO stake floor is
    /// imposed: the listed founder nodes are the trust anchor, and a floor
    /// could eject one (via a slash or unbond) and stall the live chain.
    #[test]
    fn alpha_regime_uses_the_allowlist_and_imposes_no_stake_floor() {
        assert!(
            is_consensus_eligible(true, 0, MIN, true, true),
            "a pinned validator participates regardless of bonded stake"
        );
        assert!(
            !is_consensus_eligible(true, u64::MAX, MIN, true, false),
            "an unpinned identity does not, however much it bonds"
        );
    }

    /// The regimes must be EXCLUSIVE, not ANDed. `is_pinned_validator` returns
    /// true for everyone when the list is empty, so `pinned && staked` would
    /// drop the stake requirement exactly when the set opens — the inversion
    /// this signature exists to prevent.
    #[test]
    fn the_open_regime_is_not_disabled_by_the_empty_allowlist() {
        // Empty allowlist => pin_active false AND pinned true for everyone.
        assert!(
            !is_consensus_eligible(true, 0, MIN, false, true),
            "an empty allowlist must ENABLE the stake gate, never bypass it"
        );
    }

    /// The SHIPPED floor must be meaningfully expensive, not the PoUW dust
    /// minimum — otherwise identities are cheap enough to mint in bulk.
    /// (Formation-harness builds deliberately use 0; see the constant.)
    #[cfg(not(feature = "formation-test"))]
    #[test]
    fn the_floor_costs_real_stake() {
        assert_eq!(MIN_CONSENSUS_BOND, commputer_core::token::UNITS_PER_COMME, "1 COMME");
        assert!(
            MIN_CONSENSUS_BOND >= 1_000 * 1_000,
            "must dwarf StakeParams::min_bond (1_000 raw), which gates PoUW committees, not blocks"
        );
    }
}
