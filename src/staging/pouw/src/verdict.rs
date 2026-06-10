//! Verdict (spec §6): quorum over the committee's revealed result hashes.
//!
//! Group revealed `result_hash`es into equivalence classes via the
//! [`EquivalenceOracle`](crate::oracle::EquivalenceOracle); take the largest
//! class; if it reaches `quorum` it is the committee value — compare to the
//! executor's claimed hash to yield `Confirmed`/`Disputed`, else `NoQuorum`.

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
