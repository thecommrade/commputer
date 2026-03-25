use commputer_core::proof::{ProofChallenge, ProofResponse, ProofVerdict, ResourceChannel};
use crate::cpu::CpuProver;
use crate::gpu::GpuProver;
use crate::ram::RamProver;
use crate::bandwidth::BandwidthProver;

/// Unified proof verifier for all resource channels.
pub struct ProofVerifier;

impl ProofVerifier {
    /// Verify a proof response against its challenge.
    /// Returns a verdict indicating validity.
    pub fn verify(challenge: &ProofChallenge, response: &ProofResponse) -> ProofVerdict {
        // Basic checks.
        if challenge.challenge_id != response.challenge_id {
            return ProofVerdict::Invalid;
        }

        // Channel-specific verification.
        match challenge.channel {
            ResourceChannel::Processing => {
                if CpuProver::verify_full(challenge, response) {
                    // Check timing — if it's suspiciously fast, flag it.
                    if Self::is_timing_suspicious(challenge, response) {
                        ProofVerdict::Suspicious
                    } else {
                        ProofVerdict::Valid
                    }
                } else {
                    ProofVerdict::Invalid
                }
            }
            ResourceChannel::Gpu => {
                if GpuProver::verify(challenge, response) {
                    ProofVerdict::Valid
                } else {
                    ProofVerdict::Invalid
                }
            }
            ResourceChannel::Storage => {
                // Storage verification without the underlying data can only
                // confirm the result field is present; full verification
                // requires the data and is done via StorageProver::verify directly.
                if response.result.is_empty() {
                    ProofVerdict::Invalid
                } else {
                    ProofVerdict::Valid
                }
            }
            ResourceChannel::Ram => {
                if RamProver::verify(challenge, response) {
                    ProofVerdict::Valid
                } else {
                    ProofVerdict::Invalid
                }
            }
            ResourceChannel::Bandwidth => {
                if BandwidthProver::verify(challenge, response) {
                    ProofVerdict::Valid
                } else {
                    ProofVerdict::Invalid
                }
            }
        }
    }

    /// Check if the response timing is suspicious.
    /// A response that's too fast for the reported hardware suggests
    /// the validator has more powerful hardware than claimed.
    fn is_timing_suspicious(challenge: &ProofChallenge, response: &ProofResponse) -> bool {
        match challenge.channel {
            ResourceChannel::Processing => {
                let iterations = u32::from_le_bytes(
                    challenge.payload[..4].try_into().unwrap_or([0; 4])
                );
                // Rough baseline: 10,000 iterations should take at least 1ms
                // on any real hardware. If it reports 0ms for 10k+ iterations,
                // something is wrong.
                if iterations >= 10_000 && response.compute_time_ms == 0 {
                    return true;
                }
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::identity::Address;

    fn test_addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    fn make_cpu_challenge(iterations: u32) -> ProofChallenge {
        let mut payload = iterations.to_le_bytes().to_vec();
        payload.extend_from_slice(&[42u8; 32]);
        ProofChallenge {
            channel: ResourceChannel::Processing,
            challenge_id: [1u8; 32],
            epoch: 0,
            target: test_addr(1),
            payload,
            deadline_block: 100,
        }
    }

    #[test]
    fn valid_cpu_proof() {
        let challenge = make_cpu_challenge(100);
        let response = CpuProver::solve(&challenge, test_addr(1));
        let verdict = ProofVerifier::verify(&challenge, &response);
        assert_eq!(verdict, ProofVerdict::Valid);
    }

    #[test]
    fn invalid_cpu_proof() {
        let challenge = make_cpu_challenge(100);
        let mut response = CpuProver::solve(&challenge, test_addr(1));
        response.result[0] ^= 0xFF;
        let verdict = ProofVerifier::verify(&challenge, &response);
        assert_eq!(verdict, ProofVerdict::Invalid);
    }

    #[test]
    fn mismatched_challenge_id_rejected() {
        let challenge = make_cpu_challenge(100);
        let mut response = CpuProver::solve(&challenge, test_addr(1));
        response.challenge_id = [99u8; 32]; // Wrong challenge ID.
        let verdict = ProofVerifier::verify(&challenge, &response);
        assert_eq!(verdict, ProofVerdict::Invalid);
    }
}
