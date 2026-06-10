//! Tier B (B-1) — proof verifier property tests.
//!
//! Properties of the CPU proof channel + the unified verifier: a correctly
//! solved proof is Valid, a corrupted result or mismatched challenge_id is
//! Invalid, challenge generation is deterministic and input-sensitive, and a
//! response does not cross-verify against a different challenge.
//!
//! Focused on the Processing (CPU) channel — it is deterministic and timing-
//! robust at sub-10k iterations (the verifier only flags Suspicious when a
//! >=10k-iteration proof reports 0ms). New file, zero runtime behavior change.
//! (Roadmap: src/staging/docs/wirein_roadmap.md B-1.)

use commputer_core::identity::Address;
use commputer_core::proof::{ProofVerdict, ResourceChannel};
use commputer_proofs::challenge::ChallengeGenerator;
use commputer_proofs::cpu::CpuProver;
use commputer_proofs::verifier::ProofVerifier;
use proptest::prelude::*;

fn addr(b: u8) -> Address {
    Address([b; 32])
}

proptest! {
    /// A correctly solved CPU proof is Valid. Difficulty 0.5 → 5000 iterations
    /// (< 10k), so the timing-suspicion path can never fire — the verdict is
    /// deterministically Valid.
    #[test]
    fn correct_cpu_proof_is_valid(
        epoch in any::<u64>(),
        seed in proptest::array::uniform32(any::<u8>()),
        tb in any::<u8>(),
        deadline in any::<u64>(),
    ) {
        let target = addr(tb);
        let ch = ChallengeGenerator::generate_with_difficulty(
            epoch, &seed, target, ResourceChannel::Processing, deadline, 0.5);
        let resp = CpuProver::solve(&ch, target);
        prop_assert!(CpuProver::verify_full(&ch, &resp));
        prop_assert_eq!(ProofVerifier::verify(&ch, &resp), ProofVerdict::Valid);
    }

    /// Corrupting any byte of the result makes the proof Invalid.
    #[test]
    fn corrupted_cpu_result_is_invalid(
        epoch in any::<u64>(),
        seed in proptest::array::uniform32(any::<u8>()),
        tb in any::<u8>(),
        flip in 0usize..32,
    ) {
        let target = addr(tb);
        let ch = ChallengeGenerator::generate(epoch, &seed, target, ResourceChannel::Processing, 100);
        let mut resp = CpuProver::solve(&ch, target);
        let i = flip % resp.result.len();
        resp.result[i] ^= 0xFF;
        prop_assert!(!CpuProver::verify_full(&ch, &resp));
        prop_assert_eq!(ProofVerifier::verify(&ch, &resp), ProofVerdict::Invalid);
    }

    /// A response carrying the wrong challenge_id is Invalid regardless of result.
    #[test]
    fn mismatched_challenge_id_is_invalid(
        epoch in any::<u64>(),
        seed in proptest::array::uniform32(any::<u8>()),
        tb in any::<u8>(),
    ) {
        let target = addr(tb);
        let ch = ChallengeGenerator::generate(epoch, &seed, target, ResourceChannel::Processing, 100);
        let mut resp = CpuProver::solve(&ch, target);
        resp.challenge_id[0] ^= 0xFF;
        prop_assert_eq!(ProofVerifier::verify(&ch, &resp), ProofVerdict::Invalid);
    }

    /// Challenge generation is deterministic in all of (epoch, seed, target).
    #[test]
    fn challenge_generation_is_deterministic(
        epoch in any::<u64>(),
        seed in proptest::array::uniform32(any::<u8>()),
        tb in any::<u8>(),
        deadline in any::<u64>(),
    ) {
        let target = addr(tb);
        let c1 = ChallengeGenerator::generate(epoch, &seed, target, ResourceChannel::Processing, deadline);
        let c2 = ChallengeGenerator::generate(epoch, &seed, target, ResourceChannel::Processing, deadline);
        prop_assert_eq!(c1.challenge_id, c2.challenge_id);
        prop_assert_eq!(c1.payload, c2.payload);
    }

    /// Distinct targets yield distinct challenge IDs.
    #[test]
    fn distinct_targets_distinct_challenge_ids(
        seed in proptest::array::uniform32(any::<u8>()),
        a in any::<u8>(),
        b in any::<u8>(),
    ) {
        prop_assume!(a != b);
        let ca = ChallengeGenerator::generate(0, &seed, addr(a), ResourceChannel::Processing, 100);
        let cb = ChallengeGenerator::generate(0, &seed, addr(b), ResourceChannel::Processing, 100);
        prop_assert_ne!(ca.challenge_id, cb.challenge_id);
    }

    /// A response solved for one challenge does not verify against a different one.
    #[test]
    fn response_does_not_cross_verify(
        seed in proptest::array::uniform32(any::<u8>()),
        a in any::<u8>(),
        b in any::<u8>(),
    ) {
        prop_assume!(a != b);
        let ta = addr(a);
        let tb = addr(b);
        let ca = ChallengeGenerator::generate(0, &seed, ta, ResourceChannel::Processing, 100);
        let cb = ChallengeGenerator::generate(0, &seed, tb, ResourceChannel::Processing, 100);
        let ra = CpuProver::solve(&ca, ta);
        prop_assert!(!CpuProver::verify_full(&cb, &ra), "response must not cross-verify");
    }
}

/// All five resource channels produce distinct challenge IDs for the same
/// (epoch, seed, target) — the channel tag is mixed into the ID.
#[test]
fn all_channels_produce_distinct_challenge_ids() {
    let seed = [7u8; 32];
    let target = addr(1);
    let channels = [
        ResourceChannel::Processing,
        ResourceChannel::Gpu,
        ResourceChannel::Storage,
        ResourceChannel::Ram,
        ResourceChannel::Bandwidth,
    ];
    let ids: Vec<[u8; 32]> = channels
        .iter()
        .map(|&c| ChallengeGenerator::generate(0, &seed, target, c, 100).challenge_id)
        .collect();
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(ids[i], ids[j], "channels {i} and {j} must have distinct IDs");
        }
    }
}
