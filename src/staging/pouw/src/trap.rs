//! Trap rounds (spec §7, §6.9 trap settlement) — the verifier's-dilemma killer.
//!
//! A trap is a *synthetic* verification challenge with **no real budget `B`**: the
//! protocol presents a known-wrong claim to a committee. Each verifier has already
//! `escrow`'d a `verifier_bond` (the caller does this, exactly like the bond escrows in
//! Tasks 8-9). On reveal:
//!   - a verifier who revealed the **planted wrong** answer rubber-stamped (or computed
//!     and lied) — their bond is slashed;
//!   - a verifier who revealed the **true** answer is honest — they split the jackpot.
//!
//! Settlement discipline (same as `settlement.rs`): bonds are already in escrow, so we
//! move value ONLY with `pay`/`burn`; we never call `ChainHooks::slash` on an escrowed
//! bond. The jackpot is funded **only** from slashed rubber-stamper bonds — never minted —
//! so a trap strictly shrinks supply. Slashed amounts are recorded in
//! `SettlementOutcome.slashed` as a log; every rounding remainder is routed to `burn` so
//! no unit is minted or stranded and `Ledger::total_supply` stays invariant.
//!
//! If no one rubber-stamps, there is no slash, no jackpot, and nothing is burned —
//! honest verifiers are paid on *real* jobs via the 10% slice; traps exist only to
//! punish cheats.
//!
//! WIRE-IN: `engine.rs` (Task 12) decides whether a verification round is a trap
//! (probability `p_trap_bps`) and, when it is, calls `settle_trap` with the committee's
//! reveals instead of running the normal verdict/settlement path. Honest verifiers' bonds
//! are returned by the caller (they are not touched here); only rubber-stamper bonds move.

use crate::ids::ParticipantId;
use crate::job::{Reveal, SettlementOutcome};
use crate::oracle::ChainHooks;
use crate::params::GameParams;
use crate::settlement::bps;

/// Settle a synthetic trap round.
///
/// `planted_wrong` is the deliberately-wrong result hash the protocol presented as the
/// executor's claim; `true_answer` is the hash the protocol actually knows to be correct.
/// `reveals` are the committee's openings. `bonds` returns each verifier's escrowed bond
/// amount (so the slash burns exactly what was escrowed).
///
/// Classification (exact byte match — a trap uses planted hashes the protocol controls,
/// so no `EquivalenceOracle` fuzz is needed):
///   - reveal == `planted_wrong`  ⇒ **rubber-stamper**: bond slashed (burned), recorded in `slashed`.
///   - reveal == `true_answer`    ⇒ **honest**: shares the jackpot.
///   - anything else              ⇒ ignored (neither slashed nor rewarded; its bond is the
///     caller's to return, like any other untouched bond).
///
/// `jackpot = bps(total_slashed, trap_jackpot_bps)`, split evenly across honest verifiers;
/// the remainder of the slashed pool (non-jackpot share + the indivisible jackpot remainder)
/// is burned.
pub fn settle_trap(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    planted_wrong: [u8; 32],
    true_answer: [u8; 32],
    reveals: &[Reveal],
    bonds: &dyn Fn(&ParticipantId) -> u64,
) -> SettlementOutcome {
    // Partition the committee by what they revealed.
    let mut honest: Vec<ParticipantId> = Vec::new();
    let mut slashed: Vec<(ParticipantId, u64)> = Vec::new();
    let mut total_slashed: u64 = 0;
    for r in reveals {
        if r.result_hash == planted_wrong {
            let bond = bonds(&r.verifier);
            total_slashed += bond;
            slashed.push((r.verifier, bond));
        } else if r.result_hash == true_answer {
            honest.push(r.verifier);
        }
    }

    // Jackpot funded ONLY from the slashed rubber-stamper bonds (which are already in
    // escrow); split it evenly across honest verifiers and burn the rest. With no
    // rubber-stampers `total_slashed == 0`, so nothing is paid and nothing is burned.
    let jackpot_pool = bps(total_slashed, p.trap_jackpot_bps);
    let verifiers_paid = pay_even(l, jackpot_pool, &honest);
    // Burn whatever is left of the slashed-bond escrow: the non-jackpot share plus any
    // indivisible jackpot remainder (global rounding rule keeps supply exact).
    let burned = total_slashed - verifiers_paid;
    l.burn(burned);

    SettlementOutcome {
        verifiers_paid,
        burned,
        slashed,
        ..Default::default()
    }
}

/// Pay `pool` evenly across `recipients` from escrow, returning the total actually paid.
/// Any indivisible remainder is left in escrow for the caller to burn. With an empty
/// recipient list (no honest verifiers), nothing is paid. (Mirrors `settlement::pay_even`,
/// which is module-private there.)
fn pay_even(l: &mut dyn ChainHooks, pool: u64, recipients: &[ParticipantId]) -> u64 {
    let n = recipients.len() as u64;
    if n == 0 {
        return 0;
    }
    let each = pool / n;
    if each == 0 {
        return 0;
    }
    for r in recipients {
        l.pay(*r, each);
    }
    each * n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    use crate::job::Reveal;
    use crate::oracle::Ledger;
    use crate::params::GameParams;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }
    fn rv(n: u8, h: u8) -> Reveal {
        Reveal { verifier: ParticipantId([n; 32]), result_hash: [h; 32], salt: [0; 32] }
    }

    /// A rubber-stamper (revealed the planted-wrong hash) is slashed; honest verifiers
    /// (revealed the true hash) split `trap_jackpot_bps` of the slashed bonds; the
    /// remainder is burned. Supply is invariant and no escrow is stranded.
    #[test]
    fn rubber_stamper_slashed_honest_gets_jackpot() {
        let p = GameParams::default(); // verifier_bond 20, trap_jackpot 50%
        let mut l = Ledger::new();
        // planted-wrong = hash 8 (the deliberately-wrong claim); true answer = hash 5.
        let (good1, good2, bad) = (pid(1), pid(2), pid(3));
        // Each verifier escrowed their bond up front (the caller does this).
        for v in [good1, good2, bad] {
            l.credit(v, 20);
            l.escrow(v, 20);
        }
        let total0 = l.total_supply();
        let burned0 = l.burned;
        let reveals = vec![rv(1, 5), rv(2, 5), rv(3, 8)]; // good1,good2 honest; bad rubber-stamped
        let bonds = |_: &ParticipantId| 20u64;
        let out = settle_trap(&mut l, &p, [8; 32], [5; 32], &reveals, &bonds);
        // 1 rubber-stamper slashed (20); jackpot = 50% of 20 = 10, split across 2 honest = 5 each.
        assert_eq!(out.verifiers_paid, 10);
        // burn = slashed pool (20) - jackpot paid (10) = 10.
        assert_eq!(out.burned, 10);
        assert_eq!(out.slashed, vec![(bad, 20)]);
        assert_eq!(l.balance_of(&good1), 5); // jackpot share (bond returned by caller, not here)
        assert_eq!(l.balance_of(&good2), 5);
        assert_eq!(l.balance_of(&bad), 0); // slashed
        assert_eq!(l.total_supply(), total0); // no mint
        assert_eq!(l.burned - burned0, 10); // 10 units truly left circulation
        // The slashed pot drains fully (bad's 20 = 10 to honest + 10 burned). The two
        // honest verifiers' bonds (40) are NOT touched here — the caller returns them —
        // so 40 legitimately remains in escrow. The slashed pot strands nothing.
        assert_eq!(l.escrowed(), 40);
    }

    /// No rubber-stampers ⇒ no slash, no jackpot, no burn, no mint. (Honest verifiers are
    /// paid on real jobs via the 10% slice; a clean trap moves nothing.)
    #[test]
    fn no_rubber_stampers_no_slash_no_jackpot() {
        let p = GameParams::default();
        let mut l = Ledger::new();
        let (good1, good2) = (pid(1), pid(2));
        // Honest bonds stay escrowed; the caller returns them. settle_trap touches nothing.
        for v in [good1, good2] {
            l.credit(v, 20);
            l.escrow(v, 20);
        }
        let total0 = l.total_supply();
        let escrowed0 = l.escrowed();
        let burned0 = l.burned;
        let reveals = vec![rv(1, 5), rv(2, 5)]; // both honest
        let bonds = |_: &ParticipantId| 20u64;
        let out = settle_trap(&mut l, &p, [8; 32], [5; 32], &reveals, &bonds);
        assert_eq!(out.verifiers_paid, 0);
        assert_eq!(out.burned, 0);
        assert!(out.slashed.is_empty());
        assert_eq!(l.total_supply(), total0); // no mint
        assert_eq!(l.burned, burned0); // nothing burned
        assert_eq!(l.escrowed(), escrowed0); // honest bonds untouched (still escrowed)
        assert_eq!(l.balance_of(&good1), 0); // got nothing (no jackpot on a clean trap)
    }
}
