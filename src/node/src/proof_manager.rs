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
    /// Feature 113: Rejection broadcast when a proof is invalid.
    Rejection {
        challenge_id: [u8; 32],
        validator: Address,
        reason: String,
    },
}

/// Feature 115: Per-validator per-channel proof statistics.
#[derive(Debug, Clone, Default)]
pub struct ProofChannelStats {
    pub challenges_issued: u64,
    pub challenges_passed: u64,
    pub challenges_failed: u64,
    pub avg_response_time_ms: u64,
    /// Accumulated response times for averaging.
    total_response_time_ms: u64,
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
    /// Feature 112: Contribution percent caps resource usage (1-100).
    pub contribution_percent: u8,
    /// Feature 115: Per-validator per-channel statistics.
    pub channel_stats: HashMap<(Address, ResourceChannel), ProofChannelStats>,
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
            contribution_percent: 100,
            channel_stats: HashMap::new(),
        }
    }

    /// Feature 113: Get a pending challenge by ID (for cross-node verification).
    pub fn get_pending_challenge(&self, challenge_id: &[u8; 32]) -> Option<ProofChallenge> {
        self.pending_challenges.get(challenge_id).cloned()
    }

    /// Generate one challenge per ResourceChannel for the given target validator.
    /// `difficulty_multipliers` optionally scales difficulty per channel (Feature 114).
    /// `block_hash` is used for deterministic randomness (Feature 120).
    pub fn generate_challenges(
        &mut self,
        epoch: u64,
        epoch_seed: &[u8; 32],
        target: Address,
        deadline_block: u64,
    ) -> Vec<ProofChallenge> {
        self.generate_challenges_with_difficulty(epoch, epoch_seed, target, deadline_block, &HashMap::new())
    }

    /// Generate challenges with per-channel difficulty multipliers.
    pub fn generate_challenges_with_difficulty(
        &mut self,
        epoch: u64,
        epoch_seed: &[u8; 32],
        target: Address,
        deadline_block: u64,
        difficulty_multipliers: &HashMap<ResourceChannel, f64>,
    ) -> Vec<ProofChallenge> {
        let mut challenges = Vec::new();
        for channel in ResourceChannel::ALL {
            let difficulty = difficulty_multipliers.get(&channel).copied().unwrap_or(1.0);
            // Feature 112: Scale difficulty by contribution_percent.
            let scaled_difficulty = difficulty * (self.contribution_percent as f64 / 100.0);
            let challenge = ChallengeGenerator::generate_with_difficulty(
                epoch, epoch_seed, target, channel, deadline_block, scaled_difficulty,
            );
            self.pending_challenges.insert(challenge.challenge_id, challenge.clone());
            // Feature 115: Track challenge issuance.
            let stats = self.channel_stats.entry((target, channel)).or_default();
            stats.challenges_issued += 1;
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
    /// Feature 116: Weights scores by difficulty_multiplier and penalizes slow responses.
    pub fn finalize_epoch(&mut self) -> Vec<(Address, EpochProofSummary)> {
        self.finalize_epoch_with_difficulty(&HashMap::new())
    }

    /// Finalize with per-channel difficulty multipliers for weighted scoring.
    pub fn finalize_epoch_with_difficulty(
        &mut self,
        difficulty_multipliers: &HashMap<ResourceChannel, f64>,
    ) -> Vec<(Address, EpochProofSummary)> {
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

                    let base_score = match verdict {
                        ProofVerdict::Valid => 100u32,
                        ProofVerdict::Suspicious => 50,
                        ProofVerdict::Invalid | ProofVerdict::TimedOut => 0,
                    };

                    // Feature 116: Weight by difficulty multiplier.
                    let difficulty = difficulty_multipliers
                        .get(&challenge.channel)
                        .copied()
                        .unwrap_or(1.0);
                    let mut score = (base_score as f64 * difficulty).round() as u32;

                    // Feature 116: Penalize slow responses (> 5s gets a 20% penalty per extra second).
                    if base_score > 0 && resp.compute_time_ms > 5000 {
                        let extra_secs = (resp.compute_time_ms - 5000) / 1000;
                        let penalty = (extra_secs as f64 * 0.2).min(0.9); // Max 90% penalty
                        score = (score as f64 * (1.0 - penalty)).round() as u32;
                    }

                    // Feature 115: Update channel stats.
                    let stats = self.channel_stats
                        .entry((validator, challenge.channel))
                        .or_default();
                    if score > 0 {
                        stats.challenges_passed += 1;
                        channels_contributed += 1;
                    } else {
                        stats.challenges_failed += 1;
                    }
                    stats.total_response_time_ms += resp.compute_time_ms;
                    let total_responses = stats.challenges_passed + stats.challenges_failed;
                    if total_responses > 0 {
                        stats.avg_response_time_ms = stats.total_response_time_ms / total_responses;
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
