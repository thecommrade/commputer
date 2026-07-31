// leader.rs — deterministic round-robin leader election with view change fallback
//
// WHAT IT DOES:
//   Implements BFT-style leader election for consensus. Only one validator
//   produces a block per height, preventing simultaneous forks.
//
// WHERE IT SHOULD GO:
//   src/node/src/leader.rs
//
// WIRING REQUIRED (founder task — do not modify these files as an agent):
//   1. Add `pub mod leader;` to src/node/src/lib.rs
//   2. In the block production path (event_loop.rs or wherever blocks are
//      proposed), gate block creation behind:
//      `if leader::is_valid_leader(height, &my_address, &validators, seconds_waiting)`

use commputer_core::identity::Address;

/// Returns the expected leader for `height` using deterministic round-robin.
///
/// Validators are sorted by address bytes (ascending) before indexing, so the
/// result is independent of the order in which the caller provides the slice.
/// Returns `None` if `validators` is empty.
pub fn leader_for_height(height: u64, validators: &[Address]) -> Option<Address> {
    if validators.is_empty() {
        return None;
    }
    let mut sorted = validators.to_vec();
    sorted.sort();
    let idx = (height as usize) % sorted.len();
    Some(sorted[idx])
}

/// Returns the effective leader after a view-change timeout.
///
/// If the expected leader has not produced a block within the expected window,
/// the network advances to the next validator every `VIEW_CHANGE_INTERVAL`
/// seconds. This function returns whoever is the current "active" leader given
/// how many seconds have elapsed since the block was due.
///
/// `seconds_since_expected` == 0 means the primary is still within their slot.
/// At 6 s the network advances to the first fallback, at 12 s the second, etc.
///
/// Returns `None` if `validators` is empty.
pub fn fallback_leader(
    height: u64,
    validators: &[Address],
    seconds_since_expected: u64,
) -> Option<Address> {
    if validators.is_empty() {
        return None;
    }
    let mut sorted = validators.to_vec();
    sorted.sort();

    const VIEW_CHANGE_INTERVAL: u64 = 6;
    let view_offset = seconds_since_expected / VIEW_CHANGE_INTERVAL;
    let primary_idx = (height as usize) % sorted.len();
    let effective_idx = (primary_idx + view_offset as usize) % sorted.len();
    Some(sorted[effective_idx])
}

/// Returns `true` if `address` is a valid leader for `height` right now.
///
/// An address is valid if it matches the primary leader (`seconds_waiting == 0`)
/// *or* if enough time has elapsed for a view change to advance leadership to
/// that address.  A 3-second clock-skew tolerance is applied: an address is
/// accepted for the current view window plus an extra 3 seconds into the next
/// window (so a slow node that hasn't yet timed out will still accept blocks
/// from the newly promoted leader a little early).
///
/// Returns `false` if `validators` is empty.
pub fn is_valid_leader(
    height: u64,
    address: &Address,
    validators: &[Address],
    seconds_waiting: u64,
) -> bool {
    if validators.is_empty() {
        return false;
    }

    const CLOCK_SKEW_TOLERANCE: u64 = 3;

    // Check the current view (based on seconds_waiting).
    if let Some(current) = fallback_leader(height, validators, seconds_waiting) {
        if &current == address {
            return true;
        }
    }

    // Also accept the previous view within tolerance (outgoing leader's clock
    // may be slightly slow, so they still think it's their slot).
    if seconds_waiting >= CLOCK_SKEW_TOLERANCE {
        let prev_view_time = seconds_waiting.saturating_sub(CLOCK_SKEW_TOLERANCE);
        if let Some(prev) = fallback_leader(height, validators, prev_view_time) {
            if &prev == address {
                return true;
            }
        }
    } else {
        // seconds_waiting < tolerance: also check view at 0 (primary always valid early)
        if let Some(primary) = fallback_leader(height, validators, 0) {
            if &primary == address {
                return true;
            }
        }
    }

    // Also accept the next view within tolerance (incoming leader's clock
    // may be slightly fast).
    let next_view_time = seconds_waiting.saturating_add(CLOCK_SKEW_TOLERANCE);
    if let Some(next) = fallback_leader(height, validators, next_view_time) {
        if &next == address {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        let mut b = [0u8; 32];
        b[0] = byte;
        Address(b)
    }

    // -----------------------------------------------------------------------
    // round_robin_basic: heights 0,1,2,3 cycle through 3 validators
    // -----------------------------------------------------------------------
    #[test]
    fn round_robin_basic() {
        // Provide validators pre-sorted so expected indices are predictable.
        let validators = vec![addr(1), addr(2), addr(3)];
        assert_eq!(leader_for_height(0, &validators), Some(addr(1)));
        assert_eq!(leader_for_height(1, &validators), Some(addr(2)));
        assert_eq!(leader_for_height(2, &validators), Some(addr(3)));
        assert_eq!(leader_for_height(3, &validators), Some(addr(1)));
    }

    // -----------------------------------------------------------------------
    // round_robin_deterministic: unsorted input produces same result
    // -----------------------------------------------------------------------
    #[test]
    fn round_robin_deterministic() {
        let sorted_order = vec![addr(1), addr(2), addr(3)];
        let shuffled = vec![addr(3), addr(1), addr(2)];

        for h in 0..6u64 {
            assert_eq!(
                leader_for_height(h, &sorted_order),
                leader_for_height(h, &shuffled),
                "height {} produced different leaders for different input orderings",
                h
            );
        }
    }

    // -----------------------------------------------------------------------
    // round_robin_empty: empty validators returns None
    // -----------------------------------------------------------------------
    #[test]
    fn round_robin_empty() {
        assert_eq!(leader_for_height(0, &[]), None);
        assert_eq!(leader_for_height(42, &[]), None);
    }

    // -----------------------------------------------------------------------
    // round_robin_single: single validator always selected
    // -----------------------------------------------------------------------
    #[test]
    fn round_robin_single() {
        let validators = vec![addr(7)];
        for h in 0..10u64 {
            assert_eq!(leader_for_height(h, &validators), Some(addr(7)));
        }
    }

    // -----------------------------------------------------------------------
    // fallback_leader: 0s=primary, 6s=next, 12s=third, 18s=wraps
    // -----------------------------------------------------------------------
    #[test]
    fn test_fallback_leader() {
        // Height 0 with sorted validators [1, 2, 3]: primary = addr(1)
        let validators = vec![addr(1), addr(2), addr(3)];
        assert_eq!(fallback_leader(0, &validators, 0),  Some(addr(1)), "0s: primary");
        assert_eq!(fallback_leader(0, &validators, 5),  Some(addr(1)), "5s: still primary");
        assert_eq!(fallback_leader(0, &validators, 6),  Some(addr(2)), "6s: first fallback");
        assert_eq!(fallback_leader(0, &validators, 11), Some(addr(2)), "11s: still first fallback");
        assert_eq!(fallback_leader(0, &validators, 12), Some(addr(3)), "12s: second fallback");
        assert_eq!(fallback_leader(0, &validators, 17), Some(addr(3)), "17s: still second fallback");
        assert_eq!(fallback_leader(0, &validators, 18), Some(addr(1)), "18s: wraps back to primary");
    }

    // -----------------------------------------------------------------------
    // is_valid_leader_primary: primary valid for first ~9 seconds
    //   (6 s slot + 3 s clock-skew tolerance before advancing)
    // -----------------------------------------------------------------------
    #[test]
    fn is_valid_leader_primary() {
        let validators = vec![addr(1), addr(2), addr(3)];
        // addr(1) is primary at height 0.
        // It should be valid from 0 s up through (and including) 8 s.
        for s in 0..=8u64 {
            assert!(
                is_valid_leader(0, &addr(1), &validators, s),
                "primary should be valid at {}s",
                s
            );
        }
        // At 9 s the view has advanced one full slot (6s) and the tolerance
        // window (3s) has also expired, so addr(1) is no longer valid.
        assert!(
            !is_valid_leader(0, &addr(1), &validators, 9),
            "primary should be invalid at 9s"
        );
    }

    // -----------------------------------------------------------------------
    // is_valid_leader_rejects_wrong: non-leader rejected at time 0
    // -----------------------------------------------------------------------
    #[test]
    fn is_valid_leader_rejects_wrong() {
        let validators = vec![addr(1), addr(2), addr(3)];
        // At height 0, time 0: only addr(1) is the valid leader.
        assert!(!is_valid_leader(0, &addr(2), &validators, 0));
        assert!(!is_valid_leader(0, &addr(3), &validators, 0));
    }
}

/// How many view-changes past the primary is `producer` for `height`?
///
/// 0 = the primary leader for this height, 1 = the first fallback, and so on.
/// `None` if the producer is not in the validator set (or the set is empty).
///
/// ANTI-GRINDING. `is_valid_leader` deliberately accepts three views at once
/// (current, and ±3s of clock-skew tolerance), so up to THREE addresses can be
/// legal leaders at one height. If the vote then arbitrates between their
/// candidates by BLOCK HASH — a field the producer re-rolls for free via
/// timestamp or tx ordering — any of them can grind a header until it sorts
/// lowest and steal the round from the primary. That attack does not need an
/// open validator set; it works inside the pinned trio.
///
/// Ranking by `(view_offset, hash)` removes the incentive: the primary's
/// candidate always outranks a fallback's, and the hash decides only BETWEEN
/// candidates of the same view. View offset is a pure function of the height
/// and the validator set — never of block content — which is precisely why
/// CometBFT's proposer selection is grinding-proof.
pub fn view_offset_of(height: u64, validators: &[Address], producer: &Address) -> Option<usize> {
    if validators.is_empty() {
        return None;
    }
    let mut sorted = validators.to_vec();
    sorted.sort();
    let n = sorted.len();
    let producer_idx = sorted.iter().position(|a| a == producer)?;
    let primary_idx = (height as usize) % n;
    Some((producer_idx + n - primary_idx) % n)
}

#[cfg(test)]
mod view_offset_tests {
    use super::*;

    fn addr(n: u8) -> Address {
        Address([n; 32])
    }

    /// The primary for a height has offset 0; fallbacks increase in schedule
    /// order. This is what lets the vote prefer the primary WITHOUT consulting
    /// any grindable field.
    #[test]
    fn offset_is_zero_for_the_primary_and_increases_by_view() {
        let vs = vec![addr(1), addr(2), addr(3)];
        // height 0 -> primary is sorted[0] = addr(1)
        assert_eq!(view_offset_of(0, &vs, &addr(1)), Some(0));
        assert_eq!(view_offset_of(0, &vs, &addr(2)), Some(1));
        assert_eq!(view_offset_of(0, &vs, &addr(3)), Some(2));
        // height 1 -> primary rotates to sorted[1] = addr(2)
        assert_eq!(view_offset_of(1, &vs, &addr(2)), Some(0));
        assert_eq!(view_offset_of(1, &vs, &addr(3)), Some(1));
        assert_eq!(view_offset_of(1, &vs, &addr(1)), Some(2));
    }

    /// It agrees with the leader schedule itself — offset 0 is exactly
    /// `leader_for_height`, so ranking by it cannot disagree with the rotation.
    #[test]
    fn offset_zero_matches_leader_for_height() {
        let vs = vec![addr(7), addr(3), addr(9), addr(1)];
        for h in 0..12u64 {
            let primary = leader_for_height(h, &vs).unwrap();
            assert_eq!(view_offset_of(h, &vs, &primary), Some(0), "height {h}");
        }
    }

    /// A producer outside the set has no view — its candidate must not be
    /// rankable ahead of a legitimate one.
    #[test]
    fn unknown_producer_and_empty_set_have_no_view() {
        let vs = vec![addr(1), addr(2)];
        assert_eq!(view_offset_of(0, &vs, &addr(42)), None);
        assert_eq!(view_offset_of(0, &[], &addr(1)), None);
    }
}
