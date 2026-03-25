use commputer_core::proof::{
    ProofChallenge, ProofResponse, ResourceChannel, EpochProofSummary, ProofVerdict,
};
use commputer_core::identity::Address;
use commputer_proofs::{
    CpuProver, GpuProver, RamProver, BandwidthProver, StorageProver,
    ChallengeGenerator, ProofVerifier,
};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProofMessage {
    Challenge(ProofChallenge),
    Response(ProofResponse),
}

/// Default storage data size: 1 MB assigned to each validator.
const STORAGE_DATA_SIZE: usize = 1_048_576;

pub struct ProofManager {
    pending_challenges: HashMap<[u8; 32], ProofChallenge>,
    responses: Vec<ProofResponse>,
    our_address: Address,
    /// Current chain height, updated from the event loop.
    pub current_height: u64,
    /// Our assigned storage data (generated deterministically from address).
    storage_data: Vec<u8>,
}

impl ProofManager {
    pub fn new(our_address: Address) -> Self {
        // Generate deterministic storage data from our address (serves as seed).
        let storage_data = StorageProver::generate_test_data(&our_address.0, STORAGE_DATA_SIZE);
        Self {
            pending_challenges: HashMap::new(),
            responses: Vec::new(),
            our_address,
            current_height: 0,
            storage_data,
        }
    }

    /// Generate one challenge per ResourceChannel for the given target validator.
    pub fn generate_challenges(
        &mut self,
        epoch: u64,
        epoch_seed: &[u8; 32],
        target: Address,
        deadline_block: u64,
    ) -> Vec<ProofChallenge> {
        let mut challenges = Vec::new();
        for channel in ResourceChannel::ALL {
            let challenge = ChallengeGenerator::generate(
                epoch, epoch_seed, target, channel, deadline_block,
            );
            self.pending_challenges.insert(challenge.challenge_id, challenge.clone());
            challenges.push(challenge);
        }
        challenges
    }

    /// Solve a challenge directed at us, dispatching to the appropriate prover.
    pub fn solve_challenge(&self, challenge: &ProofChallenge) -> ProofResponse {
        match challenge.channel {
            ResourceChannel::Processing => {
                CpuProver::solve(challenge, self.our_address)
            }
            ResourceChannel::Gpu => {
                GpuProver::solve(challenge, self.our_address)
            }
            ResourceChannel::Ram => {
                RamProver::solve(challenge, self.our_address)
            }
            ResourceChannel::Bandwidth => {
                BandwidthProver::solve(challenge, self.our_address)
            }
            ResourceChannel::Storage => {
                StorageProver::solve(challenge, &self.storage_data, self.our_address)
            }
        }
    }

    /// Record a proof response for later verification at epoch end.
    pub fn record_response(&mut self, response: ProofResponse) {
        self.responses.push(response);
    }

    /// Verify all collected responses against pending challenges and produce
    /// per-validator EpochProofSummary results. Clears state for next epoch.
    pub fn finalize_epoch(&mut self) -> Vec<(Address, EpochProofSummary)> {
        // Group responses by validator.
        let mut by_validator: HashMap<Address, Vec<&ProofResponse>> = HashMap::new();
        for resp in &self.responses {
            by_validator.entry(resp.validator).or_default().push(resp);
        }

        // Also collect which validators were challenged (from pending challenges).
        let mut challenged_validators: HashMap<Address, Vec<&ProofChallenge>> = HashMap::new();
        for challenge in self.pending_challenges.values() {
            challenged_validators
                .entry(challenge.target)
                .or_default()
                .push(challenge);
        }

        let mut summaries = Vec::new();

        // Build a summary for every validator that was challenged.
        let all_validators: Vec<Address> = challenged_validators.keys().copied().collect();
        for validator in all_validators {
            let mut processing_score: u32 = 0;
            let mut gpu_score: u32 = 0;
            let mut storage_score: u32 = 0;
            let mut ram_score: u32 = 0;
            let mut bandwidth_score: u32 = 0;
            let mut channels_contributed: u8 = 0;

            let empty_responses = vec![];
            let responses = by_validator.get(&validator).unwrap_or(&empty_responses);

            // Check each response against its challenge.
            for resp in responses {
                if let Some(challenge) = self.pending_challenges.get(&resp.challenge_id) {
                    // Check if response came after the deadline.
                    let timed_out = challenge.deadline_block > 0
                        && self.current_height > challenge.deadline_block;

                    let verdict = if timed_out {
                        ProofVerdict::TimedOut
                    } else {
                        ProofVerifier::verify(challenge, resp)
                    };
                    let score = match verdict {
                        ProofVerdict::Valid => 100,
                        ProofVerdict::Suspicious => 50,
                        ProofVerdict::Invalid | ProofVerdict::TimedOut => 0,
                    };

                    if score > 0 {
                        channels_contributed += 1;
                    }

                    match challenge.channel {
                        ResourceChannel::Processing => processing_score = score,
                        ResourceChannel::Gpu => gpu_score = score,
                        ResourceChannel::Storage => storage_score = score,
                        ResourceChannel::Ram => ram_score = score,
                        ResourceChannel::Bandwidth => bandwidth_score = score,
                    }
                }
            }

            // Diversity bonus: 10 points per channel contributed, max 50.
            let diversity_bonus = (channels_contributed * 10).min(50);

            // Determine epoch from the first challenge for this validator.
            let epoch = challenged_validators
                .get(&validator)
                .and_then(|cs| cs.first())
                .map(|c| c.epoch)
                .unwrap_or(0);

            summaries.push((
                validator,
                EpochProofSummary {
                    validator,
                    epoch,
                    processing_score,
                    gpu_score,
                    storage_score,
                    ram_score,
                    bandwidth_score,
                    diversity_bonus,
                },
            ));
        }

        // Clear state for next epoch.
        self.pending_challenges.clear();
        self.responses.clear();

        summaries
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
    fn generate_challenges_for_all_channels() {
        let mut pm = ProofManager::new(test_addr(1));
        let challenges = pm.generate_challenges(0, &[42u8; 32], test_addr(2), 100);
        assert_eq!(challenges.len(), 5);
    }

    #[test]
    fn solve_own_cpu_challenge() {
        let pm = ProofManager::new(test_addr(1));
        let challenge = ChallengeGenerator::generate(
            0, &[42u8; 32], test_addr(1), ResourceChannel::Processing, 100,
        );
        let response = pm.solve_challenge(&challenge);
        assert!(!response.result.is_empty());
        assert_eq!(response.validator, test_addr(1));
    }

    #[test]
    fn finalize_epoch_with_valid_responses() {
        let mut pm = ProofManager::new(test_addr(1));
        // Generate a CPU challenge for validator 2
        let challenge = ChallengeGenerator::generate(
            0, &[42u8; 32], test_addr(2), ResourceChannel::Processing, 100,
        );
        pm.pending_challenges.insert(challenge.challenge_id, challenge.clone());

        // Validator 2 solves it correctly
        let response = CpuProver::solve(&challenge, test_addr(2));
        pm.record_response(response);

        let summaries = pm.finalize_epoch();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].0, test_addr(2));
        assert!(summaries[0].1.processing_score > 0);
    }

    #[test]
    fn finalize_epoch_clears_state() {
        let mut pm = ProofManager::new(test_addr(1));
        pm.generate_challenges(0, &[42u8; 32], test_addr(2), 100);
        let _ = pm.finalize_epoch();
        assert!(pm.pending_challenges.is_empty());
        assert!(pm.responses.is_empty());
    }

    #[test]
    fn solve_all_channel_types() {
        let mut pm = ProofManager::new(test_addr(1));
        let challenges = pm.generate_challenges(0, &[42u8; 32], test_addr(1), 100);

        for challenge in &challenges {
            let response = pm.solve_challenge(challenge);
            assert!(!response.result.is_empty());
            assert_eq!(response.validator, test_addr(1));
        }
    }

    #[test]
    fn full_cycle_generate_solve_finalize() {
        let mut pm = ProofManager::new(test_addr(1));
        let challenges = pm.generate_challenges(0, &[42u8; 32], test_addr(1), 100);

        for challenge in &challenges {
            let response = pm.solve_challenge(challenge);
            pm.record_response(response);
        }

        let summaries = pm.finalize_epoch();
        assert_eq!(summaries.len(), 1);
        let (addr, summary) = &summaries[0];
        assert_eq!(*addr, test_addr(1));
        assert!(summary.processing_score > 0);
        assert!(summary.gpu_score > 0);
        assert!(summary.ram_score > 0);
        assert!(summary.bandwidth_score > 0);
        // Storage is placeholder, verifier accepts non-empty result
        assert!(summary.storage_score > 0);
        assert!(summary.diversity_bonus > 0);
    }
}
