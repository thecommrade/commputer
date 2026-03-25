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
        Self::generate_with_difficulty(epoch, epoch_seed, target, channel, deadline_block, 1.0)
    }

    /// Feature 114: Generate a challenge with a difficulty scaling parameter.
    /// Feature 120: Uses SHA-256(block_hash || epoch || validator_address) as seed.
    pub fn generate_with_difficulty(
        epoch: u64,
        epoch_seed: &[u8; 32],
        target: Address,
        channel: ResourceChannel,
        deadline_block: u64,
        difficulty: f64,
    ) -> ProofChallenge {
        // Feature 120: Deterministic randomness from block_hash + epoch + validator_address.
        let deterministic_seed = Self::derive_deterministic_seed(epoch_seed, epoch, &target);

        // Deterministic challenge ID from deterministic seed + channel.
        let challenge_id = Self::derive_challenge_id(&deterministic_seed, &target, &channel);

        // Channel-specific payload, scaled by difficulty.
        let payload = Self::generate_payload_with_difficulty(&challenge_id, &channel, difficulty);

        ProofChallenge {
            channel,
            challenge_id,
            epoch,
            target,
            payload,
            deadline_block,
        }
    }

    /// Feature 120: Derive deterministic seed from SHA-256(block_hash || epoch || validator_address).
    fn derive_deterministic_seed(block_hash: &[u8; 32], epoch: u64, target: &Address) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(block_hash);
        hasher.update(epoch.to_le_bytes());
        hasher.update(target.0);
        let result = hasher.finalize();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&result);
        seed
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

    /// Generate channel-specific challenge payload with difficulty scaling.
    fn generate_payload_with_difficulty(
        challenge_id: &[u8; 32],
        channel: &ResourceChannel,
        difficulty: f64,
    ) -> Vec<u8> {
        match channel {
            ResourceChannel::Processing => {
                let base_iterations: u32 = 10_000;
                let iterations = (base_iterations as f64 * difficulty).round() as u32;
                let mut payload = iterations.to_le_bytes().to_vec();
                payload.extend_from_slice(challenge_id);
                payload
            }
            ResourceChannel::Gpu => {
                let mut payload = vec![0x02]; // GPU challenge type marker
                payload.extend_from_slice(challenge_id);
                payload
            }
            ResourceChannel::Storage => {
                // Feature 118: Storage challenge with offset and length.
                let mut payload = vec![0x03]; // Storage challenge type marker
                // Derive offset and length from challenge_id.
                let offset = u32::from_le_bytes(challenge_id[0..4].try_into().unwrap()) % (1_048_576 - 4096);
                let length = ((u32::from_le_bytes(challenge_id[4..8].try_into().unwrap()) % 4096) + 64)
                    .min(4096);
                payload.extend_from_slice(&offset.to_le_bytes());
                payload.extend_from_slice(&length.to_le_bytes());
                payload.extend_from_slice(challenge_id);
                payload
            }
            ResourceChannel::Ram => {
                let base_mb: u32 = 256;
                let required_mb = (base_mb as f64 * difficulty).round() as u32;
                let mut payload = required_mb.to_le_bytes().to_vec();
                payload.extend_from_slice(challenge_id);
                payload
            }
            ResourceChannel::Bandwidth => {
                let base_kb: u32 = 1024;
                let data_size_kb = (base_kb as f64 * difficulty).round() as u32;
                let mut payload = data_size_kb.to_le_bytes().to_vec();
                payload.extend_from_slice(challenge_id);
                payload
            }
        }
    }

    /// Legacy payload generation (no difficulty scaling).
    fn generate_payload(challenge_id: &[u8; 32], channel: &ResourceChannel) -> Vec<u8> {
        Self::generate_payload_with_difficulty(challenge_id, channel, 1.0)
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

    // Feature 205: Deterministic test mode — same seed produces same challenges
    #[test]
    fn feature_205_deterministic_challenges() {
        use rand::SeedableRng;
        use rand::rngs::StdRng;

        // Create two deterministic RNGs with the same seed
        let seed = 42u64;
        let mut rng1 = StdRng::seed_from_u64(seed);
        let mut rng2 = StdRng::seed_from_u64(seed);

        // Generate epoch seeds deterministically
        let mut seed_bytes1 = [0u8; 32];
        let mut seed_bytes2 = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rng1, &mut seed_bytes1);
        rand::RngCore::fill_bytes(&mut rng2, &mut seed_bytes2);
        assert_eq!(seed_bytes1, seed_bytes2, "Same seed should produce same epoch seed");

        // Generate challenges with same inputs
        let addr = test_addr(1);
        let c1 = ChallengeGenerator::generate(0, &seed_bytes1, addr, ResourceChannel::Processing, 100);
        let c2 = ChallengeGenerator::generate(0, &seed_bytes2, addr, ResourceChannel::Processing, 100);

        assert_eq!(c1.challenge_id, c2.challenge_id, "Same seed must produce same challenge ID");
        assert_eq!(c1.payload, c2.payload, "Same seed must produce same payload");

        // Different seed -> different challenges
        let mut rng3 = StdRng::seed_from_u64(99);
        let mut seed_bytes3 = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rng3, &mut seed_bytes3);
        let c3 = ChallengeGenerator::generate(0, &seed_bytes3, addr, ResourceChannel::Processing, 100);
        assert_ne!(c1.challenge_id, c3.challenge_id, "Different seed must produce different challenge");
    }

    #[test]
    fn different_validators_different_ids() {
        let seed = [42u8; 32];

        let c1 = ChallengeGenerator::generate(0, &seed, test_addr(1), ResourceChannel::Processing, 100);
        let c2 = ChallengeGenerator::generate(0, &seed, test_addr(2), ResourceChannel::Processing, 100);

        assert_ne!(c1.challenge_id, c2.challenge_id);
    }
}
