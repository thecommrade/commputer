//! Stake-weighted proposer schedule — a PURE function, no carried state.
//!
//! WHAT: given a validator set and their bonded stakes, produce the repeating
//! sequence of proposers, so a validator's share of blocks is proportional to
//! its stake.
//! WHERE IT WILL BE WIRED: `leader.rs` (`leader_for_height` / `fallback_leader`)
//! once the epoch snapshot lands. NOT WIRED YET — this module is inert.
//! WHY IT LIVES HERE: it is a pure function of `(height, set, stakes)`, so it
//! is fully testable without a node, and its properties can be proven rather
//! than hoped for.
//!
//! ## Why stake-weighting IS the Sybil defence
//!
//! Our current schedule is `sorted[height % n]` — a count of ADDRESSES. Since
//! keypairs are free to mint, that means N identities buy N times the block
//! production, and no flat stake floor can fix it: research measured 1 COMME
//! at 6.3% of a single block reward, so an identity pays for itself in ~0.15s.
//!
//! When share is LINEAR IN STAKE, splitting stake gains exactly nothing:
//! 10 COMME as one identity and 10 COMME split across ten identities receive
//! the same total share. Minting keypairs stops being an attack, which is why
//! this — not a floor — is the real gate.
//!
//! ## Why it is a pure function (and CometBFT's is not)
//!
//! This is CometBFT's proposer-priority algorithm, which is in turn nginx's
//! smooth weighted round-robin: each step add every validator's weight to its
//! accumulator, pick the max, subtract the total. CometBFT CARRIES that
//! accumulator across heights and set changes, which forces three corrections
//! (re-centering, rescaling, and a -1.125*P penalty for rejoiners) and makes
//! the schedule path-dependent on the chain's whole history — a resyncing node
//! must replay every increment to learn who may propose.
//!
//! But from a zero start the walk is EXACTLY PERIODIC with period `W = Σ
//! weights`: over W steps each validator gains `W * w(i)` and is selected
//! exactly `w(i)` times, losing `w(i) * W`. Net zero, so the accumulator
//! returns to all-zeros and the sequence repeats.
//!
//! So we generate ONE cycle from zero and index it by `height % W`. Nothing is
//! carried, every node computes the same answer from the same inputs, and all
//! three CometBFT corrections become unnecessary — they exist only to repair a
//! carried accumulator. A rejoining validator cannot reset anything, because
//! there is nothing to reset.

use commputer_core::identity::Address;

/// Cap on the generated cycle length, so a pathological stake spread cannot
/// make us allocate an enormous schedule. Weights are scaled down to fit.
pub const MAX_CYCLE_LEN: u64 = 4096;

/// Scale raw bonded stakes to small positive weights whose sum is `<= MAX_CYCLE_LEN`.
///
/// Every eligible validator gets at least 1, so nobody is scaled out of the
/// schedule entirely (a validator that can never propose is worse than an
/// imprecise share, and at the limit it would be an unannounced ejection).
/// Proportionality is preserved to within integer rounding.
pub fn scale_weights(stakes: &[u64]) -> Vec<u64> {
    if stakes.is_empty() {
        return Vec::new();
    }
    let n = stakes.len() as u64;
    // Everyone gets a floor of 1, so the remaining budget is what can be
    // distributed proportionally.
    let budget = MAX_CYCLE_LEN.saturating_sub(n).max(0);
    let total: u128 = stakes.iter().map(|s| *s as u128).sum();
    if total == 0 || budget == 0 {
        // No stake information (or no room): equal weights, which makes the
        // schedule reduce exactly to round-robin.
        return vec![1; stakes.len()];
    }
    // FIRST try the exact ratio in smallest terms. Dividing every stake by
    // their GCD preserves proportions exactly, and for the ratios a real
    // validator set produces (3:1:1, or all-equal) it collapses enormous stake
    // numbers to single digits — a 3-slot cycle instead of a 4095-slot one for
    // the same schedule.
    //
    // Order matters: reducing the SCALED weights instead would not work,
    // because the `+1` floor applied below destroys the common factor. Live
    // output showed exactly that — 3:1:1 stakes became [2456, 819, 819], whose
    // GCD is 1, so nothing reduced.
    let exact = reduce_by_gcd(stakes.to_vec());
    let exact_total: u128 = exact.iter().map(|w| *w as u128).sum();
    if exact_total <= MAX_CYCLE_LEN as u128 && exact.iter().all(|w| *w >= 1) {
        return exact;
    }

    // The exact ratio is too large (a pathological spread, or coprime stakes
    // in raw units). Fall back to a proportional scaling with a floor of 1, so
    // the cycle stays bounded and nobody is scaled out entirely.
    let scaled: Vec<u64> = stakes
        .iter()
        .map(|s| 1 + ((*s as u128 * budget as u128) / total) as u64)
        .collect();
    reduce_by_gcd(scaled)
}

/// Divide weights by their greatest common divisor.
///
/// Purely an efficiency measure — dividing every weight by a common factor
/// cannot change any validator's PROPORTION, so the schedule is unchanged; it
/// is just expressed in the smallest equivalent terms.
///
/// It matters because the cycle length is the SUM of the weights, and we
/// rebuild that whole cycle each epoch. Observed live: three near-equal stakes
/// scaled to 1365 each, giving a 4095-entry cycle where 3 entries express
/// exactly the same schedule — a 1365x cost for no information. Equal stakes
/// are also the common case for a small validator set, so this is the case
/// worth optimising.
fn reduce_by_gcd(mut weights: Vec<u64>) -> Vec<u64> {
    let divisor = weights.iter().copied().fold(0u64, gcd);
    if divisor > 1 {
        for w in &mut weights {
            *w /= divisor;
        }
    }
    weights
}

fn gcd(a: u64, b: u64) -> u64 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// One full cycle of proposers: `Σ weights` entries, each validator appearing
/// exactly `weight(i)` times, spread smoothly rather than in a run.
///
/// `validators` MUST already be in the caller's canonical (sorted) order —
/// every node must build the identical cycle.
pub fn build_cycle(validators: &[Address], weights: &[u64]) -> Vec<Address> {
    if validators.is_empty() || validators.len() != weights.len() {
        return Vec::new();
    }
    let total: i128 = weights.iter().map(|w| *w as i128).sum();
    if total <= 0 {
        return Vec::new();
    }
    let mut acc: Vec<i128> = vec![0; validators.len()];
    let mut cycle = Vec::with_capacity(total as usize);
    for _ in 0..total {
        // Smooth weighted round-robin: add each weight, take the max, subtract
        // the total from the winner. Ties break on the LOWEST INDEX, which is
        // the caller's sorted order — deterministic across nodes, and never a
        // function of block content (that is what makes it un-grindable).
        for (i, w) in weights.iter().enumerate() {
            acc[i] += *w as i128;
        }
        let mut best = 0usize;
        for i in 1..acc.len() {
            if acc[i] > acc[best] {
                best = i;
            }
        }
        acc[best] -= total;
        cycle.push(validators[best]);
    }
    cycle
}

/// The proposer for `height` after `views` view-changes, skipping to a
/// genuinely DIFFERENT validator each time.
///
/// A plain `cycle[(slot + views) % W]` is wrong on a weighted cycle, and
/// dangerously so. Weighting places a heavy validator in adjacent slots —
/// `{5,1,1}` produces `[a,a,b,a,c,a,a]` — so advancing one slot from the first
/// `a` lands on `a` again. A view change exists precisely because the current
/// leader is not producing; handing the slot back to that same dead node makes
/// it ITS OWN FALLBACK and the outage cannot be routed around. The chain would
/// wait out repeated view timeouts on a node already known to be silent.
///
/// So each view advances to the next DISTINCT validator.
///
/// AFTER A FULL LAP IT RETURNS TO THE PRIMARY. The rotation is
/// `[primary, d0, d1, … d(k-1)]` repeating — so view `k+1` comes back around to
/// the primary rather than cycling among the fallbacks forever. Two reasons:
///
///   1. It is what the set-based `fallback_leader` this replaces already does
///      (`sorted[(h + views) % n]` wraps), so at equal stakes the two agree for
///      EVERY view count, not merely the first n-1. That exact agreement is the
///      safety argument for flipping this on a live chain — see the equivalence
///      tests in `leader.rs`. An earlier version excluded the primary
///      permanently, which diverged at `views == n` (~18 s of stall) and would
///      have made a rolling upgrade disagree about who may produce precisely
///      when the network was already struggling.
///   2. Permanently excluding the primary is not obviously safe: a leader that
///      was briefly slow can never be tried again for that height, even after
///      every other validator has also failed.
///
/// What it still prevents is the thing that matters — handing the round straight
/// back to the node whose silence caused the view change, which a naive
/// `cycle[(slot + views) % W]` does on a weighted cycle because a heavy
/// validator occupies adjacent slots.
///
/// With only one validator in the cycle it returns that one: nobody to hand to.
///
/// Bounded: at most one lap of the cycle, then modular arithmetic.
pub fn proposer_after_views(cycle: &[Address], height: u64, views: usize) -> Option<Address> {
    let n = cycle.len();
    if n == 0 {
        return None;
    }
    let start = (height % n as u64) as usize;
    let current = cycle[start];
    if views == 0 {
        return Some(current);
    }
    // Distinct validators following `start`, in cycle order.
    let mut distinct: Vec<Address> = Vec::new();
    for step in 1..=n {
        let cand = cycle[(start + step) % n];
        if cand != current && !distinct.contains(&cand) {
            distinct.push(cand);
        }
    }
    if distinct.is_empty() {
        // Only one validator appears in the cycle: nobody to fall back to.
        return Some(current);
    }
    // One full rotation is the primary plus every distinct fallback.
    let period = distinct.len() + 1;
    match views % period {
        0 => Some(current),
        r => Some(distinct[r - 1]),
    }
}

/// The proposer for `height`, stake-weighted. `None` if there is no set.
pub fn proposer_for_height(height: u64, validators: &[Address], stakes: &[u64]) -> Option<Address> {
    let weights = scale_weights(stakes);
    let cycle = build_cycle(validators, &weights);
    if cycle.is_empty() {
        return None;
    }
    Some(cycle[(height % cycle.len() as u64) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> Address {
        Address([n; 32])
    }

    /// LIVE-SAFETY: with equal stakes the weighted cycle must reduce EXACTLY to
    /// today's `sorted[height % n]`. Our three founder nodes hold equal bonds,
    /// so switching to this schedule must not change who proposes — otherwise
    /// it is not a safe swap.
    #[test]
    fn equal_stakes_reduce_exactly_to_round_robin() {
        for n in 1..=6usize {
            let vs: Vec<Address> = (1..=n as u8).map(addr).collect();
            for stake in [0u64, 1, 100_000_000, u64::MAX / 8] {
                let stakes = vec![stake; n];
                for h in 0..200u64 {
                    let got = proposer_for_height(h, &vs, &stakes).unwrap();
                    let want = vs[(h as usize) % n];
                    assert_eq!(got, want, "n={n} stake={stake} height={h}");
                }
            }
        }
    }

    /// THE SYBIL PROPERTY: splitting stake across identities must gain exactly
    /// nothing. This is the whole reason for stake-weighting — with a
    /// count-based schedule, splitting multiplies your share.
    #[test]
    fn splitting_stake_across_identities_gains_nothing() {
        let whole = vec![addr(1), addr(2), addr(3)];
        let whole_stakes = vec![30u64, 10, 10];

        // The same 30 split into two identities of 15.
        let split = vec![addr(1), addr(4), addr(2), addr(3)];
        let split_stakes = vec![15u64, 15, 10, 10];

        let count_share = |vs: &[Address], st: &[u64], targets: &[Address]| -> f64 {
            let n = 20_000u64;
            let hits = (0..n)
                .filter(|h| {
                    proposer_for_height(*h, vs, st)
                        .map(|p| targets.contains(&p))
                        .unwrap_or(false)
                })
                .count();
            hits as f64 / n as f64
        };

        let before = count_share(&whole, &whole_stakes, &[addr(1)]);
        let after = count_share(&split, &split_stakes, &[addr(1), addr(4)]);
        assert!(
            (before - after).abs() < 0.02,
            "splitting changed share {before:.3} -> {after:.3}; \
             stake-weighting must make Sybil identities worthless"
        );
        assert!((before - 0.6).abs() < 0.02, "30/50 should be ~0.6, got {before:.3}");
    }

    /// Share must actually track stake — otherwise "weighted" is a lie.
    #[test]
    fn share_is_proportional_to_stake() {
        let vs = vec![addr(1), addr(2)];
        let stakes = vec![75u64, 25];
        let n = 20_000u64;
        let hits = (0..n)
            .filter(|h| proposer_for_height(*h, &vs, &stakes) == Some(addr(1)))
            .count();
        let share = hits as f64 / n as f64;
        assert!((share - 0.75).abs() < 0.02, "expected ~0.75, got {share:.3}");
    }

    /// The cycle is exactly periodic: each validator appears exactly its weight
    /// many times, which is what makes indexing by `height % W` correct and
    /// lets us carry NO state between heights.
    #[test]
    fn cycle_is_periodic_with_exact_counts() {
        let vs = vec![addr(1), addr(2), addr(3)];
        let weights = vec![5u64, 1, 1];
        let cycle = build_cycle(&vs, &weights);
        assert_eq!(cycle.len(), 7, "period is the sum of weights");
        assert_eq!(cycle.iter().filter(|a| **a == addr(1)).count(), 5);
        assert_eq!(cycle.iter().filter(|a| **a == addr(2)).count(), 1);
        assert_eq!(cycle.iter().filter(|a| **a == addr(3)).count(), 1);
        // nginx's documented {5,1,1} smooth-WRR sequence.
        let expect = [addr(1), addr(1), addr(2), addr(1), addr(3), addr(1), addr(1)];
        assert_eq!(cycle, expect, "must match smooth weighted round-robin");
    }

    /// Smoothness: a heavy validator must not take its slots in one run. A run
    /// of adjacent slots makes it its own view-change fallback, so an outage
    /// cannot be routed around — a liveness hazard, not just untidiness.
    #[test]
    fn a_heavy_validator_does_not_occupy_a_long_run() {
        let vs = vec![addr(1), addr(2), addr(3)];
        let cycle = build_cycle(&vs, &[3, 1, 1]);
        let mut longest = 1usize;
        let mut run = 1usize;
        for i in 1..cycle.len() {
            if cycle[i] == cycle[i - 1] {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 1;
            }
        }
        assert!(longest <= 2, "run of {longest} adjacent slots is too long: {cycle:?}");
    }

    /// Every eligible validator gets at least one slot — being scaled to zero
    /// would be an unannounced ejection from consensus.
    #[test]
    fn nobody_is_scaled_out_of_the_schedule() {
        // A stake ratio extreme enough to round a small holder to zero.
        let stakes = vec![u64::MAX / 2, 1];
        let w = scale_weights(&stakes);
        assert!(w.iter().all(|x| *x >= 1), "weights {w:?} must all be >= 1");
        let vs = vec![addr(1), addr(2)];
        let cycle = build_cycle(&vs, &w);
        assert!(cycle.contains(&addr(2)), "the small holder must still appear");
    }

    /// THE HAZARD, pinned. On a weighted cycle a heavy validator occupies
    /// adjacent slots, so a naive one-slot view advance hands the round back to
    /// the very node that just failed to produce — it becomes its own fallback
    /// and the outage cannot be routed around.
    #[test]
    fn a_view_change_never_hands_the_slot_back_to_the_same_validator() {
        let vs = vec![addr(1), addr(2), addr(3)];
        let cycle = build_cycle(&vs, &[5, 1, 1]);
        assert_eq!(cycle[0], cycle[1], "precondition: the heavy validator IS adjacent to itself");

        for h in 0..cycle.len() as u64 {
            let primary = proposer_after_views(&cycle, h, 0).unwrap();
            let naive = cycle[((h as usize) + 1) % cycle.len()];
            let first_fallback = proposer_after_views(&cycle, h, 1).unwrap();

            assert_ne!(
                first_fallback, primary,
                "height {h}: view 1 returned the SAME validator — a dead leader would be its own \
                 fallback"
            );
            // And demonstrate the naive rule really would have failed here.
            if naive == primary {
                assert_ne!(first_fallback, naive, "height {h}: must not repeat the naive mistake");
            }
        }
    }

    /// Successive views must keep advancing through DIFFERENT validators, so a
    /// second outage is also routed around.
    #[test]
    fn successive_views_walk_through_distinct_validators() {
        let vs = vec![addr(1), addr(2), addr(3)];
        let cycle = build_cycle(&vs, &[5, 1, 1]);
        let a = proposer_after_views(&cycle, 0, 0).unwrap();
        let b = proposer_after_views(&cycle, 0, 1).unwrap();
        let c = proposer_after_views(&cycle, 0, 2).unwrap();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c, "three views should reach all three validators");
    }

    /// LIVE SAFETY: with equal stakes the cycle has one slot per validator, so
    /// the view walk must reduce exactly to today's `(index + views) % n`.
    /// Our founder nodes hold equal bonds, so the flip must not change their
    /// view-change behaviour either — not just their primary schedule.
    #[test]
    fn equal_stakes_make_the_view_walk_identical_to_plain_rotation() {
        for n in 1..=6usize {
            let vs: Vec<Address> = (1..=n as u8).map(addr).collect();
            let cycle = build_cycle(&vs, &scale_weights(&vec![100u64; n]));
            assert_eq!(cycle.len(), n, "equal stakes give one slot each");
            for h in 0..40u64 {
                for views in 0..n {
                    let got = proposer_after_views(&cycle, h, views).unwrap();
                    let want = vs[((h as usize) + views) % n];
                    assert_eq!(got, want, "n={n} h={h} views={views}");
                }
            }
        }
    }

    /// A single validator has nobody to fall back to — must return itself
    /// rather than looping forever or returning None.
    #[test]
    fn a_lone_validator_falls_back_to_itself_without_hanging() {
        let cycle = vec![addr(1); 4];
        for views in 0..5 {
            assert_eq!(proposer_after_views(&cycle, 2, views), Some(addr(1)));
        }
        assert_eq!(proposer_after_views(&[], 0, 1), None, "empty cycle is safe");
    }

    /// Reducing weights by their GCD must not change ANY validator's share —
    /// it only expresses the same schedule in smaller terms. Found from live
    /// output: three near-equal stakes produced a 4095-entry cycle where 3
    /// entries say exactly the same thing.
    #[test]
    fn gcd_reduction_shrinks_the_cycle_without_changing_the_schedule() {
        let vs = vec![addr(1), addr(2), addr(3)];

        // Equal stakes — the common case for a small set.
        let equal = vec![1_000_000_000u64; 3];
        let w = scale_weights(&equal);
        assert_eq!(w, vec![1, 1, 1], "equal stakes reduce to the smallest terms");
        assert_eq!(build_cycle(&vs, &w).len(), 3, "3 slots, not thousands");

        // A 3:1:1 split must still be 3:1:1 after reduction.
        let split = vec![3_000_000_000u64, 1_000_000_000, 1_000_000_000];
        let ws = scale_weights(&split);
        let total: u64 = ws.iter().sum();
        assert!(total < 100, "cycle should be small, got {total}: {ws:?}");
        let cycle = build_cycle(&vs, &ws);
        let c1 = cycle.iter().filter(|a| **a == addr(1)).count() as f64 / cycle.len() as f64;
        assert!((c1 - 0.6).abs() < 0.05, "3-of-5 share must survive reduction, got {c1:.3}");

        // And the proposer sequence must be unchanged versus the unreduced
        // weighting — the reduction is invisible to consumers.
        let unreduced = vec![3u64, 1, 1];
        let reduced_cycle = build_cycle(&vs, &ws);
        let plain_cycle = build_cycle(&vs, &unreduced);
        assert_eq!(reduced_cycle, plain_cycle, "same schedule, smaller terms");
    }

    /// Bounded: a pathological spread must not allocate an enormous schedule.
    #[test]
    fn cycle_length_is_bounded() {
        let stakes: Vec<u64> = (1..=8u64).map(|i| i * 1_000_000_000).collect();
        let w = scale_weights(&stakes);
        let total: u64 = w.iter().sum();
        assert!(total <= MAX_CYCLE_LEN, "cycle {total} exceeds the cap");
    }

    /// Degenerate inputs must not panic — this runs on the consensus path.
    #[test]
    fn degenerate_inputs_are_safe() {
        assert_eq!(proposer_for_height(0, &[], &[]), None);
        assert_eq!(build_cycle(&[addr(1)], &[]), Vec::new(), "mismatched lengths");
        assert_eq!(proposer_for_height(0, &[addr(1)], &[0]), Some(addr(1)));
        assert_eq!(scale_weights(&[]), Vec::<u64>::new());
    }
}
