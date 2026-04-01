// leader_comprehensive_tests.rs — Comprehensive tests for src/node/src/leader.rs
//
// WHAT IT DOES:
//   Extended test suite covering distribution uniformity, edge cases, clock skew
//   boundaries, and determinism guarantees for the leader election module.
//
// WHERE IT SHOULD GO:
//   Copy into src/node/src/leader.rs under the existing #[cfg(test)] mod tests block,
//   or keep as a separate test file linked from the crate's test harness.
//
// WIRING REQUIRED:
//   Add to src/node/src/leader.rs:
//     #[cfg(test)]
//     mod comprehensive_tests { ... } // paste these tests

#[cfg(test)]
mod leader_comprehensive_tests {
    // Import from the actual module when wired in.
    // Replace the path below once placed inside the leader.rs file:
    use commputer::leader::{leader_for_height, fallback_leader, is_valid_leader};
    use commputer_core::identity::Address;

    fn addr(byte: u8) -> Address {
        let mut b = [0u8; 32];
        b[0] = byte;
        Address(b)
    }

    fn addr_n(n: u8) -> Address {
        addr(n)
    }

    // -----------------------------------------------------------------------
    // Task 1a: 25 validators — uniform distribution over 25,000 heights
    // -----------------------------------------------------------------------
    #[test]
    fn uniform_distribution_25_validators_25000_heights() {
        let validators: Vec<Address> = (1u8..=25).map(addr_n).collect();
        let mut counts = std::collections::HashMap::new();
        for h in 0u64..25_000 {
            let leader = leader_for_height(h, &validators).expect("should have a leader");
            *counts.entry(leader).or_insert(0u64) += 1;
        }
        assert_eq!(counts.len(), 25, "all 25 validators should appear");
        for (v, count) in &counts {
            assert_eq!(
                *count, 1000,
                "validator {:?} appeared {} times, expected 1000",
                v, count
            );
        }
    }

    // -----------------------------------------------------------------------
    // Task 1b: 2 validators — strict alternation
    // -----------------------------------------------------------------------
    #[test]
    fn two_validators_strict_alternation() {
        let validators = vec![addr(1), addr(2)];
        // After sort: [addr(1), addr(2)]
        // Even heights -> addr(1), odd heights -> addr(2)
        for h in 0u64..20 {
            let leader = leader_for_height(h, &validators).unwrap();
            if h % 2 == 0 {
                assert_eq!(leader, addr(1), "height {} should be addr(1)", h);
            } else {
                assert_eq!(leader, addr(2), "height {} should be addr(2)", h);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Task 1c: 1 validator — always selected
    // -----------------------------------------------------------------------
    #[test]
    fn single_validator_always_selected() {
        let validators = vec![addr(42)];
        for h in 0u64..100 {
            assert_eq!(
                leader_for_height(h, &validators),
                Some(addr(42)),
                "single validator should always be selected at height {}",
                h
            );
        }
        // Also at very large heights
        assert_eq!(leader_for_height(u64::MAX, &validators), Some(addr(42)));
    }

    // -----------------------------------------------------------------------
    // Task 1d: View change wrap-around with 5 validators
    // Offsets 0, 6, 12, 18, 24 seconds — and 30s wraps back to primary
    // -----------------------------------------------------------------------
    #[test]
    fn view_change_wraparound_5_validators() {
        // Validators sorted: addr(1) addr(2) addr(3) addr(4) addr(5)
        // At height 0, primary_idx = 0 → addr(1)
        let validators = vec![addr(1), addr(2), addr(3), addr(4), addr(5)];

        let expected_at: &[(u64, Address)] = &[
            (0,  addr(1)), // view 0: primary
            (6,  addr(2)), // view 1
            (12, addr(3)), // view 2
            (18, addr(4)), // view 3
            (24, addr(5)), // view 4
            (30, addr(1)), // view 5 → wraps back to idx 0
        ];
        for &(secs, expected) in expected_at {
            let got = fallback_leader(0, &validators, secs).unwrap();
            assert_eq!(
                got, expected,
                "at {}s expected {:?} got {:?}",
                secs, expected, got
            );
        }
    }

    // -----------------------------------------------------------------------
    // Task 1e: Clock skew boundary at exactly 3, 5, 6, 8, 9 seconds
    // With 3 validators: primary = addr(1) at height 0
    // Primary valid window: 0..=8 (slot 0-5 + 3s tolerance)
    // At 9s: addr(1) no longer valid, addr(2) takes over (was valid from 6s)
    // -----------------------------------------------------------------------
    #[test]
    fn clock_skew_boundary_seconds() {
        let validators = vec![addr(1), addr(2), addr(3)];

        // addr(1) is primary at height 0
        // is_valid_leader checks current view ± tolerance
        assert!(is_valid_leader(0, &addr(1), &validators, 3), "primary valid at 3s");
        assert!(is_valid_leader(0, &addr(1), &validators, 5), "primary valid at 5s");
        assert!(is_valid_leader(0, &addr(1), &validators, 6), "primary valid at 6s (tolerance)");
        assert!(is_valid_leader(0, &addr(1), &validators, 8), "primary valid at 8s (last tolerance)");
        assert!(!is_valid_leader(0, &addr(1), &validators, 9), "primary invalid at 9s");

        // addr(2) becomes fallback leader at 6s
        assert!(is_valid_leader(0, &addr(2), &validators, 6), "fallback valid at 6s");
        assert!(is_valid_leader(0, &addr(2), &validators, 8), "fallback valid at 8s");
        assert!(is_valid_leader(0, &addr(2), &validators, 9), "fallback valid at 9s");
    }

    // -----------------------------------------------------------------------
    // Task 1f: is_valid_leader with 100 validators and 1000 seconds elapsed
    // At 1000s, view = 1000/6 = 166, idx = (0 + 166) % 100 = 66
    // -----------------------------------------------------------------------
    #[test]
    fn valid_leader_100_validators_1000_seconds() {
        let validators: Vec<Address> = (1u8..=100).map(addr_n).collect();
        let height = 0u64;

        // Compute what leader we expect at 1000 seconds
        let view_offset = 1000u64 / 6;
        let primary_idx = 0usize; // height 0 % 100 = 0
        let mut sorted = validators.clone();
        sorted.sort();
        let effective_idx = (primary_idx + view_offset as usize) % sorted.len();
        let expected_leader = sorted[effective_idx];

        assert!(
            is_valid_leader(height, &expected_leader, &validators, 1000),
            "leader at 1000s should be valid"
        );

        // The primary (sorted[0]) should NOT be valid at 1000s
        // (far outside any tolerance window)
        // view_offset = 166, primary is only valid within tolerance 0..=8
        assert!(
            !is_valid_leader(height, &sorted[0], &validators, 1000),
            "original primary should be invalid at 1000s elapsed"
        );
    }

    // -----------------------------------------------------------------------
    // Task 1g: Determinism — 1000 "random" heights, same validators = same result
    // -----------------------------------------------------------------------
    #[test]
    fn determinism_same_validators_same_result() {
        let validators_a = vec![addr(10), addr(20), addr(30), addr(5)];
        let validators_b = vec![addr(30), addr(5), addr(20), addr(10)]; // different order

        // Generate 1000 "random" heights using a simple LCG
        let mut state: u64 = 0xdeadbeef_cafebabe;
        for _ in 0..1000 {
            state = state.wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
            let h = state >> 33;

            let leader_a = leader_for_height(h, &validators_a);
            let leader_b = leader_for_height(h, &validators_b);
            assert_eq!(
                leader_a, leader_b,
                "leaders must match regardless of input ordering at height {}",
                h
            );
        }
    }

    // -----------------------------------------------------------------------
    // Additional: Empty validators returns None
    // -----------------------------------------------------------------------
    #[test]
    fn empty_validators_returns_none() {
        assert_eq!(leader_for_height(0, &[]), None);
        assert_eq!(leader_for_height(u64::MAX, &[]), None);
        assert_eq!(fallback_leader(0, &[], 0), None);
        assert_eq!(fallback_leader(0, &[], 1000), None);
        assert!(!is_valid_leader(0, &addr(1), &[], 0));
    }

    // -----------------------------------------------------------------------
    // Additional: u64::MAX height doesn't panic
    // -----------------------------------------------------------------------
    #[test]
    fn max_height_no_panic() {
        let validators = vec![addr(1), addr(2), addr(3)];
        let _ = leader_for_height(u64::MAX, &validators);
        let _ = fallback_leader(u64::MAX, &validators, 0);
        let _ = is_valid_leader(u64::MAX, &addr(1), &validators, 0);
    }
}
