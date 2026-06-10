//! Tier B (B-3) — Snowball voter edge-case property tests (active subset).
//!
//! Properties of the `SnowballVoter` state machine over arbitrary parameters:
//! unanimous quorum finalizes in exactly β rounds; sub-quorum never finalizes;
//! finalization is sticky (frozen against later conflicting rounds); a
//! no-quorum round resets accumulated progress.
//!
//! New file, zero runtime behavior change. The roadmap's #[ignore]d cases that
//! need a deferred SnowballVoter API extension are intentionally NOT included.
//! (Roadmap: src/staging/docs/wirein_roadmap.md B-3.)

use std::collections::HashMap;

use commputer_consensus::snowball::{SnowballParams, SnowballVoter};
use commputer_core::block::BlockHash;
use proptest::prelude::*;

fn bh(n: u8) -> BlockHash {
    BlockHash([n; 32])
}

/// (sample_size k, quorum α with k/2 < α ≤ k, decision_threshold β in 1..=15).
fn params() -> impl Strategy<Value = (usize, usize, u32)> {
    (3usize..=20usize).prop_flat_map(|k| (Just(k), (k / 2 + 1)..=k, 1u32..=15u32))
}

proptest! {
    /// A unanimous full-quorum response every round finalizes on EXACTLY the
    /// β-th round, with finalized_hash == the agreed choice.
    #[test]
    fn unanimous_finalizes_in_exactly_beta_rounds(
        (k, quorum, beta) in params(),
        cb in any::<u8>(),
    ) {
        let mut v = SnowballVoter::new(SnowballParams {
            sample_size: k, quorum, decision_threshold: beta,
        });
        let choice = bh(cb);
        let responses = HashMap::from([(choice, k)]); // full unanimous quorum

        let mut finalized_on = None;
        for round in 1..=(beta + 5) {
            if v.record_round(&responses) {
                finalized_on = Some(round);
                break;
            }
        }
        prop_assert_eq!(finalized_on, Some(beta), "must finalize on the β-th round");
        prop_assert!(v.is_finalized());
        prop_assert_eq!(v.finalized_hash(), Some(choice));
    }

    /// A response that stays one short of quorum never finalizes.
    #[test]
    fn below_quorum_never_finalizes(
        (k, quorum, beta) in params(),
        cb in any::<u8>(),
    ) {
        let mut v = SnowballVoter::new(SnowballParams {
            sample_size: k, quorum, decision_threshold: beta,
        });
        let responses = HashMap::from([(bh(cb), quorum - 1)]); // qmin >= 2 ⇒ count >= 1

        for _ in 0..(beta + 10) {
            prop_assert!(!v.record_round(&responses), "sub-quorum must never finalize");
        }
        prop_assert!(!v.is_finalized());
        prop_assert_eq!(v.finalized_hash(), None);
    }

    /// Once finalized, the voter is frozen: later conflicting unanimous rounds
    /// neither re-finalize nor change the decided hash.
    #[test]
    fn finalization_is_sticky(
        (k, quorum, beta) in params(),
        cb in any::<u8>(),
        ob in any::<u8>(),
    ) {
        prop_assume!(cb != ob);
        let mut v = SnowballVoter::new(SnowballParams {
            sample_size: k, quorum, decision_threshold: beta,
        });
        let choice = bh(cb);
        let win = HashMap::from([(choice, k)]);
        for _ in 0..beta {
            v.record_round(&win);
        }
        prop_assert!(v.is_finalized());
        let decided = v.finalized_hash();
        prop_assert_eq!(decided, Some(choice));

        let conflict = HashMap::from([(bh(ob), k)]);
        for _ in 0..10 {
            prop_assert!(!v.record_round(&conflict), "finalized voter must not re-finalize");
        }
        prop_assert_eq!(v.finalized_hash(), decided, "decided hash must be immutable");
    }
}

/// A no-quorum round resets the consecutive counter: β-1 good rounds, then an
/// empty round, then it takes a FULL β more good rounds to finalize.
#[test]
fn no_quorum_round_resets_progress() {
    let beta = 5u32;
    let mut v = SnowballVoter::new(SnowballParams {
        sample_size: 5, quorum: 4, decision_threshold: beta,
    });
    let win = HashMap::from([(bh(1), 5usize)]);
    let empty: HashMap<BlockHash, usize> = HashMap::new();

    for _ in 0..(beta - 1) {
        assert!(!v.record_round(&win));
    }
    assert!(!v.is_finalized(), "β-1 rounds is not enough");

    // No quorum → progress reset to zero.
    assert!(!v.record_round(&empty));

    // β-1 good rounds again still not enough (the reset really happened)...
    for _ in 0..(beta - 1) {
        assert!(!v.record_round(&win));
    }
    assert!(!v.is_finalized(), "reset means the earlier progress was lost");

    // ...the β-th good round after the reset finalizes.
    assert!(v.record_round(&win));
    assert!(v.is_finalized());
    assert_eq!(v.finalized_hash(), Some(bh(1)));
}
