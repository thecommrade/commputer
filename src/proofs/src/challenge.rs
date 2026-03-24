use commputer_core::identity::Address;
use commputer_core::proof::{ProofChallenge, ResourceChannel};
use sha2::{Sha256, Digest};

/// Generates deterministic proof challenges for validators.
/// Challenges are derived from the epoch seed + validator address,
/// so all honest nodes agree on which challenges are valid.
pub struct ChallengeGenerator;

impl ChallengeGenerator {
    /// Generate a challenge for a specific validator and channel.
    pub fn generate(
        epoch: u64,
        epoch_seed: &[u8; 32],
        target: Address,
        channel: ResourceChannel,
        deadline_block: u64,
    ) -> ProofChallenge {
        // Deterministic challenge ID from seed + target + channel.
        let challenge_id = Self::derive_challenge_id(epoch_seed, &target, &channel);

        // Channel-specific payload.
        let payload = Self::generate_payload(&challenge_id, &channel);

        ProofChallenge {
            channel,
            challenge_id,
            epoch,
            target,
            payload,
            deadline_block,
        }
    }

    /// Derive a deterministic challenge ID.
    fn derive_challenge_id(
        seed: &[u8; 32],
        target: &Address,
        channel: &ResourceChannel,
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(seed);
        hasher.update(target.0);
        hasher.update(Self::channel_tag(channel));
        let result = hasher.finalize();
        let mut id = [0u8; 32];
        id.copy_from_slice(&result);
        id
    }

    /// Generate channel-specific challenge payload.
    fn generate_payload(challenge_id: &[u8; 32], channel: &ResourceChannel) -> Vec<u8> {
        match channel {
            ResourceChannel::Processing => {
                // CPU challenge: iterative hashing puzzle.
                // The validator must perform N rounds of SHA-256 on the challenge ID.
                // N is encoded in the first 4 bytes of payload.
                let iterations: u32 = 10_000;
                let mut payload = iterations.to_le_bytes().to_vec();
                payload.extend_from_slice(challenge_id);
                payload
            }
            ResourceChannel::Gpu => {
                // GPU challenge: matrix multiplication seed.
                // Full implementation will use ML micro-benchmarks.
                let mut payload = vec![0x02]; // GPU challenge type marker
                payload.extend_from_slice(challenge_id);
                payload
            }
            ResourceChannel::Storage => {
                // Storage challenge: request specific data chunks.
                // The challenge ID determines which chunks to retrieve.
                let mut payload = vec![0x03]; // Storage challenge type marker
                // In production: chunk indices derived from challenge_id.
                payload.extend_from_slice(challenge_id);
                payload
            }
            ResourceChannel::Ram => {
                // RAM challenge: memory-hard computation.
                // Must allocate and use a large buffer to solve in time.
                let required_mb: u32 = 256; // 256MB working set
                let mut payload = required_mb.to_le_bytes().to_vec();
                payload.extend_from_slice(challenge_id);
                payload
            }
            ResourceChannel::Bandwidth => {
                // Bandwidth challenge: timed data transfer.
                // Payload specifies the data size to transfer.
                let data_size_kb: u32 = 1024; // 1MB transfer
                let mut payload = data_size_kb.to_le_bytes().to_vec();
                payload.extend_from_slice(challenge_id);
                payload
            }
        }
    }

    fn channel_tag(channel: &ResourceChannel) -> &'static [u8] {
        match channel {
            ResourceChannel::Processing => b"cpu",
            ResourceChannel::Gpu => b"gpu",
            ResourceChannel::Storage => b"sto",
            ResourceChannel::Ram => b"ram",
            ResourceChannel::Bandwidth => b"bw",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    #[test]
    fn deterministic_challenge_ids() {
        let seed = [42u8; 32];
        let addr = test_addr(1);

        let c1 = ChallengeGenerator::generate(0, &seed, addr, ResourceChannel::Processing, 100);
        let c2 = ChallengeGenerator::generate(0, &seed, addr, ResourceChannel::Processing, 100);

        assert_eq!(c1.challenge_id, c2.challenge_id);
    }

    #[test]
    fn different_channels_different_ids() {
        let seed = [42u8; 32];
        let addr = test_addr(1);

        let cpu = ChallengeGenerator::generate(0, &seed, addr, ResourceChannel::Processing, 100);
        let gpu = ChallengeGenerator::generate(0, &seed, addr, ResourceChannel::Gpu, 100);

        assert_ne!(cpu.challenge_id, gpu.challenge_id);
    }

    #[test]
    fn different_validators_different_ids() {
        let seed = [42u8; 32];

        let c1 = ChallengeGenerator::generate(0, &seed, test_addr(1), ResourceChannel::Processing, 100);
        let c2 = ChallengeGenerator::generate(0, &seed, test_addr(2), ResourceChannel::Processing, 100);

        assert_ne!(c1.challenge_id, c2.challenge_id);
    }
}
