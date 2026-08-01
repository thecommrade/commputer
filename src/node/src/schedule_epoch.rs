//! Height-derived epochs for the proposer schedule, and the shadow-mode digest.
//!
//! WHAT: decides WHICH past state the proposer schedule is computed from, and
//! produces a single hash that lets every node prove it derived the same
//! schedule.
//! WHERE IT WILL BE WIRED: `event_loop.rs` (log the digest each epoch),
//! `leader.rs` (schedule input) once the per-epoch snapshot store lands.
//! NOT WIRED YET — this module is inert.
//! WHY IT LIVES HERE: pure functions of `(height, set, stakes)`, testable
//! without a node.
//!
//! ## Why the schedule must NOT read the current tip
//!
//! Today the validator set is derived from live state. That is two problems:
//!   * GRINDABLE — a bond landing at height h changes who may propose at h, so
//!     a participant can influence the schedule by timing a transaction.
//!   * DIVERGENT — two nodes with different in-flight state derive different
//!     sets, disagree on the leader, and split the vote. This chain has already
//!     forked twice from exactly that class of disagreement.
//!
//! The fix everyone uses is a LAG: compute the schedule from a set that was
//! settled some distance back, so it is identical on every node and already
//! fixed before anyone could aim a transaction at it. Solana computes the
//! schedule for an epoch from the stakes at the start of the previous one and
//! calls the result "predictable but unbiasable"; Ethereum fixes an epoch's
//! seed two epochs back for the same reason.
//!
//! ## Why HEIGHT-derived, never wall-clock
//!
//! We already have an `EpochState` on a 3600-second timer, and it is unusable
//! here: two nodes at the same height can sit in different wall-clock epochs,
//! so a schedule keyed off it forks on clock skew alone. `header.epoch` is
//! worse — it is attacker-supplied and only loosely clamped. Height is the one
//! clock every node agrees on by construction.

use commputer_core::identity::Address;
use sha2::{Digest, Sha256};

/// Blocks per schedule epoch. Matches `CHECKPOINT_INTERVAL`, which the chain
/// already treats as un-reorgable, so an epoch boundary is a height that is
/// settled by the same rule the rest of the system already trusts.
/// Kept equal to `storage::SCHEDULE_EPOCH_BLOCKS` — the write cadence and the
/// read cadence are the same protocol constant seen from two crates, and a
/// test pins them equal. Both shorten together under `formation-test` so the
/// harness can cross epoch boundaries (see the storage constant for why).
pub const EPOCH_BLOCKS: u64 = commputer_storage::state::SCHEDULE_EPOCH_BLOCKS;

/// Which schedule epoch a height belongs to.
pub fn epoch_of(height: u64) -> u64 {
    height / EPOCH_BLOCKS
}

/// The height whose committed post-state supplies the validator set for
/// `epoch`.
///
/// One full epoch of LAG: epoch E uses the last block of epoch E-2, so the set
/// is settled a minimum of `EPOCH_BLOCKS` blocks (and at most `2*EPOCH_BLOCKS`)
/// before any height that uses it. A bond therefore cannot affect the schedule
/// for at least a full epoch — long enough that the timing is not a lever.
///
/// Epochs 0 and 1 clamp to genesis: no earlier state exists, and every chain
/// does the same (Ethereum's genesis validators sign the first epochs;
/// CometBFT's InitChain set signs heights 1 and 2).
pub fn snapshot_height_for(epoch: u64) -> u64 {
    if epoch < 2 {
        0
    } else {
        (epoch - 1) * EPOCH_BLOCKS - 1
    }
}

/// The snapshot height a given block height should use.
pub fn snapshot_height_for_block(height: u64) -> u64 {
    snapshot_height_for(epoch_of(height))
}

/// A single hash proving which schedule a node derived.
///
/// SHADOW MODE: every node logs this each epoch; if two nodes ever print
/// different digests for the same epoch, they would have disagreed about who
/// may propose — caught as a log mismatch instead of as a fork.
///
/// It commits to the DERIVED CYCLE, not merely the input set. That is
/// deliberate: hashing only the inputs would miss a divergence in weight
/// scaling, in the cycle-length clamp, or in tie-breaking — precisely the
/// places where two implementations drift apart while agreeing on their
/// inputs.
///
/// Domain-separated and versioned so a future change to the schedule rule
/// cannot silently compare equal to the old one.
pub fn schedule_digest(
    epoch: u64,
    snapshot_height: u64,
    fallback_used: bool,
    validators: &[Address],
    stakes: &[u64],
    weights: &[u64],
    cycle: &[Address],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"COMMPUTER/schedule/v1");
    h.update([1u8]); // rule version
    h.update(epoch.to_le_bytes());
    h.update(snapshot_height.to_le_bytes());
    h.update([u8::from(fallback_used)]);
    h.update((validators.len() as u64).to_le_bytes());
    for (i, v) in validators.iter().enumerate() {
        h.update(v.0);
        h.update(stakes.get(i).copied().unwrap_or(0).to_le_bytes());
        h.update(weights.get(i).copied().unwrap_or(0).to_le_bytes());
    }
    h.update((cycle.len() as u64).to_le_bytes());
    for c in cycle {
        h.update(c.0);
    }
    h.finalize().into()
}

/// Short form for logs.
pub fn digest_hex(d: &[u8; 32]) -> String {
    hex::encode(&d[..8])
}

/// The schedule a node derives for one epoch, plus the digest proving it.
#[derive(Debug, Clone)]
pub struct EpochSchedule {
    pub epoch: u64,
    pub snapshot_height: u64,
    /// True when no snapshot was available and we fell back to the live set.
    pub fallback_used: bool,
    pub validators: Vec<Address>,
    pub stakes: Vec<u64>,
    pub weights: Vec<u64>,
    pub cycle: Vec<Address>,
    pub digest: [u8; 32],
}

impl EpochSchedule {
    /// The proposer this schedule assigns to `height`. `None` if empty.
    pub fn proposer_for(&self, height: u64) -> Option<Address> {
        if self.cycle.is_empty() {
            return None;
        }
        Some(self.cycle[(height % self.cycle.len() as u64) as usize])
    }
}

/// Build the schedule for `epoch` from an already-filtered eligible set.
///
/// The caller supplies the eligible `(address, stake)` pairs — eligibility is a
/// rule that lives in `consensus_set`, and the stored snapshot deliberately
/// holds the raw pool rather than a filtered set, so that rule can change
/// without re-snapshotting the chain.
///
/// `eligible` MUST be in canonical (address-sorted) order: the same membership
/// in a different order builds a different cycle, so order is part of the
/// schedule's identity.
pub fn build_schedule(
    epoch: u64,
    snapshot_height: u64,
    fallback_used: bool,
    eligible: &[(Address, u64)],
) -> EpochSchedule {
    let validators: Vec<Address> = eligible.iter().map(|(a, _)| *a).collect();
    let stakes: Vec<u64> = eligible.iter().map(|(_, s)| *s).collect();
    let weights = crate::weighted_schedule::scale_weights(&stakes);
    let cycle = crate::weighted_schedule::build_cycle(&validators, &weights);
    let digest = schedule_digest(
        epoch,
        snapshot_height,
        fallback_used,
        &validators,
        &stakes,
        &weights,
        &cycle,
    );
    EpochSchedule {
        epoch,
        snapshot_height,
        fallback_used,
        validators,
        stakes,
        weights,
        cycle,
        digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> Address {
        Address([n; 32])
    }

    /// The write cadence (storage) and the read cadence (here) can no longer
    /// drift: `EPOCH_BLOCKS` IS the storage constant, so there is one
    /// definition rather than two kept in step by a test. An equality
    /// assertion would now be tautological — it would look like coverage while
    /// checking nothing.
    ///
    /// What is still worth asserting is that the value is USABLE: a zero would
    /// divide by zero in `epoch_of`, and the production cadence should remain
    /// the checkpoint interval, which the chain already treats as un-reorgable.
    #[test]
    fn the_epoch_length_is_usable_and_matches_the_checkpoint_interval() {
        assert!(EPOCH_BLOCKS > 0, "a zero epoch length divides by zero in epoch_of");

        #[cfg(not(feature = "formation-test"))]
        assert_eq!(
            EPOCH_BLOCKS,
            commputer_storage::state::CHECKPOINT_INTERVAL,
            "production epochs should land on the already-un-reorgable checkpoint boundary"
        );

        // Whatever the cadence, the lag must still be a whole epoch — the
        // property the whole design rests on.
        assert!(snapshot_height_for(2) < 2 * EPOCH_BLOCKS);
    }

    #[test]
    fn epochs_are_height_derived_and_contiguous() {
        assert_eq!(epoch_of(0), 0);
        assert_eq!(epoch_of(EPOCH_BLOCKS - 1), 0);
        assert_eq!(epoch_of(EPOCH_BLOCKS), 1);
        assert_eq!(epoch_of(EPOCH_BLOCKS * 7 + 3), 7);
    }

    /// THE LAG IS THE POINT: the set for any height must come from a height at
    /// least a full epoch earlier, so a bond cannot be aimed at the schedule
    /// that governs it.
    #[test]
    fn the_snapshot_always_lags_by_at_least_one_full_epoch() {
        for h in (EPOCH_BLOCKS * 2)..(EPOCH_BLOCKS * 12) {
            let snap = snapshot_height_for_block(h);
            assert!(snap < h, "height {h} must not use its own state");
            let lag = h - snap;
            assert!(
                lag >= EPOCH_BLOCKS,
                "height {h} used a snapshot only {lag} blocks back — a bond \
                 could still be timed to influence its own schedule"
            );
            assert!(lag <= 2 * EPOCH_BLOCKS + 1, "lag {lag} unexpectedly large at {h}");
        }
    }

    /// Every height inside one epoch must share a snapshot, or the schedule
    /// could shift mid-epoch.
    #[test]
    fn the_snapshot_is_constant_within_an_epoch() {
        for e in 0..8u64 {
            let want = snapshot_height_for(e);
            for off in 0..EPOCH_BLOCKS {
                assert_eq!(snapshot_height_for_block(e * EPOCH_BLOCKS + off), want, "epoch {e}");
            }
        }
    }

    /// Bootstrap: the first two epochs have no earlier state and must clamp to
    /// genesis rather than underflow.
    #[test]
    fn early_epochs_clamp_to_genesis_without_underflow() {
        assert_eq!(snapshot_height_for(0), 0);
        assert_eq!(snapshot_height_for(1), 0);
        assert_eq!(snapshot_height_for(2), EPOCH_BLOCKS - 1);
        for h in 0..(EPOCH_BLOCKS * 2) {
            assert_eq!(snapshot_height_for_block(h), 0, "height {h} must use genesis");
        }
    }

    /// The digest must change if ANY input that could differ between nodes
    /// differs — that is the entire value of shadow mode.
    #[test]
    fn the_digest_detects_every_divergence_it_is_meant_to() {
        let vs = vec![addr(1), addr(2)];
        let stakes = vec![10u64, 20];
        let weights = vec![1u64, 2];
        let cycle = vec![addr(2), addr(1), addr(2)];
        let base = schedule_digest(3, 199, false, &vs, &stakes, &weights, &cycle);

        assert_eq!(base, schedule_digest(3, 199, false, &vs, &stakes, &weights, &cycle), "stable");

        let cases: Vec<(&str, [u8; 32])> = vec![
            ("epoch", schedule_digest(4, 199, false, &vs, &stakes, &weights, &cycle)),
            ("snapshot height", schedule_digest(3, 99, false, &vs, &stakes, &weights, &cycle)),
            ("fallback flag", schedule_digest(3, 199, true, &vs, &stakes, &weights, &cycle)),
            ("membership", schedule_digest(3, 199, false, &[addr(1), addr(3)], &stakes, &weights, &cycle)),
            ("stakes", schedule_digest(3, 199, false, &vs, &[10, 21], &weights, &cycle)),
            // Same set and stakes, but a different derived weighting — this is
            // the case a set-only hash would MISS.
            ("weights", schedule_digest(3, 199, false, &vs, &stakes, &[1, 3], &cycle)),
            // Same inputs, different cycle ORDER — a tie-break divergence.
            ("cycle order", schedule_digest(3, 199, false, &vs, &stakes, &weights, &[addr(1), addr(2), addr(2)])),
        ];
        for (what, d) in cases {
            assert_ne!(base, d, "digest must change when {what} differs");
        }
    }

    /// A node that fell back to the live set must NEVER produce the same digest
    /// as one reading a real snapshot, even with identical validators and
    /// stakes. Otherwise shadow mode reports agreement between two nodes that
    /// derived their schedule from different sources — the instrument would be
    /// lying in exactly the case it exists to catch.
    #[test]
    fn a_fallback_schedule_never_matches_a_snapshot_schedule() {
        let eligible = vec![(addr(1), 100u64), (addr(2), 100u64)];
        let from_snapshot = build_schedule(4, 299, false, &eligible);
        let from_fallback = build_schedule(4, 299, true, &eligible);
        assert_ne!(
            from_snapshot.digest, from_fallback.digest,
            "fallback must be distinguishable from a real snapshot"
        );
    }

    /// The built schedule must agree with the weighted-schedule module it wraps,
    /// or shadow mode would validate a schedule nobody will actually use.
    #[test]
    fn build_schedule_agrees_with_the_weighted_schedule_module() {
        let eligible = vec![(addr(1), 300u64), (addr(2), 100u64), (addr(3), 100u64)];
        let sched = build_schedule(2, 99, false, &eligible);
        let validators: Vec<Address> = eligible.iter().map(|(a, _)| *a).collect();
        let stakes: Vec<u64> = eligible.iter().map(|(_, s)| *s).collect();
        for h in 0..50u64 {
            assert_eq!(
                sched.proposer_for(h),
                crate::weighted_schedule::proposer_for_height(h, &validators, &stakes),
                "height {h}"
            );
        }
    }

    /// An empty eligible set must yield an empty schedule that reports no
    /// proposer — never a panic, and never a silent index into nothing.
    #[test]
    fn an_empty_eligible_set_is_safe() {
        let sched = build_schedule(1, 0, true, &[]);
        assert!(sched.cycle.is_empty());
        assert_eq!(sched.proposer_for(7), None);
    }

    /// Validator ORDER is part of the identity: two nodes that agree on
    /// membership but order it differently build different cycles, so they must
    /// not produce equal digests.
    #[test]
    fn digest_is_order_sensitive() {
        let stakes = vec![10u64, 20];
        let weights = vec![1u64, 2];
        let cycle = vec![addr(1)];
        let a = schedule_digest(1, 0, false, &[addr(1), addr(2)], &stakes, &weights, &cycle);
        let b = schedule_digest(1, 0, false, &[addr(2), addr(1)], &stakes, &weights, &cycle);
        assert_ne!(a, b, "a set is not a schedule — order must be committed to");
    }
}
