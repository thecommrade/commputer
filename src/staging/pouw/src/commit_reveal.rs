//! Commit-reveal for verifier votes: a binding + hiding commitment to a
//! `result_hash`, opened later by a `Reveal`. See spec §6 (commit-reveal phase).

use crate::ids::{ParticipantId, hash_parts};
use crate::job::{Commitment, Reveal};

/// commit = H(result_hash ‖ salt ‖ verifier). Binding + hiding (salt is secret until reveal).
pub fn make_commitment(verifier: &ParticipantId, result_hash: &[u8; 32], salt: &[u8; 32], bond: u64) -> Commitment {
    Commitment { verifier: *verifier, commit: hash_parts(&[result_hash, salt, &verifier.0]), bond }
}

pub fn reveal_matches(c: &Commitment, r: &Reveal) -> bool {
    r.verifier == c.verifier
        && hash_parts(&[&r.result_hash, &r.salt, &r.verifier.0]) == c.commit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    use crate::job::Reveal;
    fn pid(n: u8) -> ParticipantId { ParticipantId([n; 32]) }

    #[test]
    fn valid_reveal_matches_its_commitment() {
        let c = make_commitment(&pid(1), &[7; 32], &[3; 32], 20);
        let r = Reveal { verifier: pid(1), result_hash: [7; 32], salt: [3; 32] };
        assert!(reveal_matches(&c, &r));
    }
    #[test]
    fn tampered_reveal_is_rejected() {
        let c = make_commitment(&pid(1), &[7; 32], &[3; 32], 20);
        let bad = Reveal { verifier: pid(1), result_hash: [8; 32], salt: [3; 32] };
        assert!(!reveal_matches(&c, &bad));
    }
    #[test]
    fn commitment_hides_the_result() {
        // Two different results under the same salt/verifier produce different commitments,
        // and the commitment reveals nothing about the result without the salt.
        let a = make_commitment(&pid(1), &[7; 32], &[3; 32], 20);
        let b = make_commitment(&pid(1), &[8; 32], &[3; 32], 20);
        assert_ne!(a.commit, b.commit);
    }
}
