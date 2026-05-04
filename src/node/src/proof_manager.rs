#![allow(dead_code)]
use commputer_core::proof::{
    ProofChallenge, ProofResponse, ResourceChannel, EpochProofSummary, ProofVerdict,
};
use commputer_core::identity::Address;
use commputer_proofs::{
    CpuProver, GpuProver, RamProver, BandwidthProver, StorageProver,
    ChallengeGenerator, ProofVerifier,
};
use serde::{Serialize, Deserialize};
use std::collections::{HashMap, HashSet};

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

/// Item 150: Proof history entry for a validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofHistoryEntry {
    pub epoch: u64,
    pub channel: String,
    pub score: u32,
    pub compute_time_ms: u64,
    pub passed: bool,
}

/// Item 160: Leaderboard entry for a proof channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderboardEntry {
    pub rank: u32,
    pub validator: String,
    pub score: u32,
    pub epochs_participated: u64,
}

/// Default storage data size: 1 MB assigned to each validator.
const STORAGE_DATA_SIZE: usize = 1_048_576;

/// Manages proof challenge generation, solving, and epoch finalization for all 5 channels.
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
    /// Item 150: Proof history per validator.
    proof_history: HashMap<Address, Vec<ProofHistoryEntry>>,
    /// Item 157: Dedup set — (validator, channel, challenge_window) -> already submitted.
    submission_dedup: HashSet<(Address, ResourceChannel, [u8; 32])>,
    /// Item 159: Expired challenge IDs.
    expired_challenges: HashSet<[u8; 32]>,
    /// Item 160: Accumulated scores per validator per channel across epochs.
    accumulated_scores: HashMap<(Address, ResourceChannel), (u32, u64)>, // (total_score, epochs)
}

impl ProofManager {
    /// Create a new proof manager for the given validator address.
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
            proof_history: HashMap::new(),
            submission_dedup: HashSet::new(),
            expired_challenges: HashSet::new(),
            accumulated_scores: HashMap::new(),
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

    /// Static solver — usable from `tokio::task::spawn_blocking` without `&self`.
    /// Dispatches to the appropriate prover based on the challenge's `ResourceChannel`.
    pub fn solve_challenge_pure(
        challenge: &ProofChallenge,
        storage_data: &[u8],
        our_address: Address,
    ) -> ProofResponse {
        match challenge.channel {
            ResourceChannel::Processing => CpuProver::solve(challenge, our_address),
            ResourceChannel::Gpu => GpuProver::solve(challenge, our_address),
            ResourceChannel::Ram => RamProver::solve(challenge, our_address),
            ResourceChannel::Bandwidth => BandwidthProver::solve(challenge, our_address),
            ResourceChannel::Storage => StorageProver::solve(challenge, storage_data, our_address),
        }
    }

    /// Solve a challenge directed at us, dispatching to the appropriate prover.
    pub fn solve_challenge(&self, challenge: &ProofChallenge) -> ProofResponse {
        Self::solve_challenge_pure(challenge, &self.storage_data, self.our_address)
    }

    /// Clone of the per-validator storage data (1 MB). Used by spawn_blocking workers
    /// that need to solve `ResourceChannel::Storage` challenges off the runtime.
    pub fn storage_data_clone(&self) -> Vec<u8> {
        self.storage_data.clone()
    }

    /// Record a proof response for later verification at epoch end.
    /// Item 157: Enforces max 1 proof submission per channel per challenge window per validator.
    pub fn record_response(&mut self, response: ProofResponse) -> bool {
        // Item 157: Dedup check.
        if let Some(challenge) = self.pending_challenges.get(&response.challenge_id) {
            let key = (response.validator, challenge.channel, response.challenge_id);
            if self.submission_dedup.contains(&key) {
                return false; // Duplicate submission — reject.
            }
            self.submission_dedup.insert(key);
        }
        self.responses.push(response);
        true
    }

    /// Item 159: Check for expired challenges and mark them.
    /// Challenges not responded to within their deadline_block are marked as failed.
    pub fn expire_challenges(&mut self) {
        let expired: Vec<[u8; 32]> = self.pending_challenges
            .iter()
            .filter(|(_, c)| c.deadline_block > 0 && self.current_height > c.deadline_block)
            .map(|(id, _)| *id)
            .collect();

        for id in &expired {
            // Check if we have a response for this challenge.
            let has_response = self.responses.iter().any(|r| r.challenge_id == *id);
            if !has_response {
                self.expired_challenges.insert(*id);
            }
        }
    }

    /// Item 159: Get all expired (unanswered) challenge IDs.
    pub fn get_expired_challenges(&self) -> &HashSet<[u8; 32]> {
        &self.expired_challenges
    }

    /// Item 150: Get proof history for a specific validator.
    pub fn get_proof_history(&self, validator: &Address) -> Vec<ProofHistoryEntry> {
        self.proof_history.get(validator).cloned().unwrap_or_default()
    }

    /// Item 160: Get leaderboard for a specific channel.
    pub fn get_leaderboard(&self, channel: ResourceChannel, top_n: usize) -> Vec<LeaderboardEntry> {
        let mut scores: Vec<(Address, u32, u64)> = self.accumulated_scores
            .iter()
            .filter(|((_, ch), _)| *ch == channel)
            .map(|((addr, _), (score, epochs))| (*addr, *score, *epochs))
            .collect();

        // Sort by score descending.
        scores.sort_by(|a, b| b.1.cmp(&a.1));
        scores.truncate(top_n);

        scores
            .iter()
            .enumerate()
            .map(|(i, (addr, score, epochs))| LeaderboardEntry {
                rank: (i + 1) as u32,
                validator: hex::encode(addr.0),
                score: *score,
                epochs_participated: *epochs,
            })
            .collect()
    }

    /// Verify all collected responses against pending challenges and produce
    /// per-validator EpochProofSummary results. Clears state for next epoch.
    /// Feature 116: Weights scores by difficulty_multiplier and penalizes slow responses.
    pub fn finalize_epoch(&mut self) -> Vec<(Address, EpochProofSummary)> {
        self.finalize_epoch_with_difficulty(&HashMap::new())
    }

    /// Pure verdict computation: for each response, decide whether the
    /// corresponding challenge passed/failed/timed-out. This is the only
    /// expensive part of epoch finalization — `ProofVerifier::verify` re-runs
    /// the full prover work for each response (CpuProver does the iterative
    /// hash again, GPU/RAM/Bandwidth do the same per-channel) so verification
    /// cost is approximately equal to proving cost.
    ///
    /// Static so it's callable from `tokio::task::spawn_blocking` without
    /// holding `&self`. The event loop calls this off-runtime in
    /// `handle_epoch_tick` to keep the libp2p swarm poll responsive during
    /// the verification window.
    pub fn compute_epoch_verdicts(
        pending_challenges: &HashMap<[u8; 32], ProofChallenge>,
        responses: &[ProofResponse],
        expired_challenges: &HashSet<[u8; 32]>,
        current_height: u64,
    ) -> HashMap<[u8; 32], ProofVerdict> {
        // Item 152: rayon::par_iter for parallel verification — each
        // ProofVerifier::verify call is CPU-bound and independent, so this
        // gets near-linear speedup with core count. With block_in_place
        // wrapping at the call site (event_loop.rs handle_epoch_tick) the
        // entire parallel verification runs on tokio's worker pool without
        // blocking the swarm task.
        use rayon::prelude::*;
        responses
            .par_iter()
            .filter_map(|resp| {
                pending_challenges.get(&resp.challenge_id).map(|challenge| {
                    let expired = expired_challenges.contains(&resp.challenge_id);
                    let timed_out = expired
                        || (challenge.deadline_block > 0
                            && current_height > challenge.deadline_block);
                    let verdict = if timed_out {
                        ProofVerdict::TimedOut
                    } else {
                        ProofVerifier::verify(challenge, resp)
                    };
                    (resp.challenge_id, verdict)
                })
            })
            .collect()
    }

    /// Convenience wrapper: snapshot current state and compute verdicts.
    /// Returned map is suitable for passing to
    /// `finalize_epoch_with_precomputed_verdicts`. Designed to be wrapped in
    /// `tokio::task::block_in_place` at the event-loop call site so the
    /// parallel verification (rayon) runs without pinning the swarm task.
    pub fn compute_current_epoch_verdicts(&self) -> HashMap<[u8; 32], ProofVerdict> {
        Self::compute_epoch_verdicts(
            &self.pending_challenges,
            &self.responses,
            &self.expired_challenges,
            self.current_height,
        )
    }

    /// Finalize the epoch using a precomputed verdict map. Same body as
    /// `finalize_epoch_with_difficulty` but skips the inner
    /// `ProofVerifier::verify` calls — verdicts are looked up from the map
    /// instead. The map is typically produced by
    /// `compute_epoch_verdicts` running in `spawn_blocking`.
    ///
    /// Falls back to `ProofVerdict::Invalid` for any challenge_id not present
    /// in the map (defensive — should not happen if the map was generated
    /// from the same snapshot of `pending_challenges` / `responses`).
    pub fn finalize_epoch_with_precomputed_verdicts(
        &mut self,
        verdicts: &HashMap<[u8; 32], ProofVerdict>,
        difficulty_multipliers: &HashMap<ResourceChannel, f64>,
    ) -> Vec<(Address, EpochProofSummary)> {
        self.finalize_epoch_inner(Some(verdicts), difficulty_multipliers)
    }

    /// Finalize with per-channel difficulty multipliers for weighted scoring.
    /// Item 148: Enhanced — weights proof scores by difficulty level.
    /// Item 152: Uses parallel verification via iterators (rayon would be used in production).
    pub fn finalize_epoch_with_difficulty(
        &mut self,
        difficulty_multipliers: &HashMap<ResourceChannel, f64>,
    ) -> Vec<(Address, EpochProofSummary)> {
        self.finalize_epoch_inner(None, difficulty_multipliers)
    }

    /// Shared implementation. If `precomputed_verdicts` is `Some`, the inner
    /// loop skips `ProofVerifier::verify` and looks up verdicts there.
    /// If `None`, computes verdicts inline (legacy synchronous path).
    fn finalize_epoch_inner(
        &mut self,
        precomputed_verdicts: Option<&HashMap<[u8; 32], ProofVerdict>>,
        difficulty_multipliers: &HashMap<ResourceChannel, f64>,
    ) -> Vec<(Address, EpochProofSummary)> {
        // Item 159: Mark expired challenges before finalization.
        self.expire_challenges();

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
        // Item 152: In production, this loop would use rayon::par_iter for parallel
        // verification. Here we use sequential iteration for correctness.
        let all_validators: Vec<Address> = challenged_validators.keys().copied().collect();
        for validator in all_validators {
            let mut processing_score: u32 = 0;
            let mut gpu_score: u32 = 0;
            let mut storage_score: u32 = 0;
            let mut ram_score: u32 = 0;
            let mut bandwidth_score: u32 = 0;
            let mut channels_contributed: u32 = 0;

            let empty_responses = vec![];
            let responses = by_validator.get(&validator).unwrap_or(&empty_responses);

            // Check each response against its challenge.
            for resp in responses {
                if let Some(challenge) = self.pending_challenges.get(&resp.challenge_id) {
                    // Item 159: Check if challenge expired.
                    let expired = self.expired_challenges.contains(&resp.challenge_id);

                    // Check if response came after the deadline.
                    let timed_out = expired
                        || (challenge.deadline_block > 0
                            && self.current_height > challenge.deadline_block);

                    let verdict = if timed_out {
                        ProofVerdict::TimedOut
                    } else if let Some(map) = precomputed_verdicts {
                        // Use precomputed verdict (came from spawn_blocking).
                        // Defensive fallback: unknown challenge_ids treated as Invalid.
                        map.get(&resp.challenge_id).copied().unwrap_or(ProofVerdict::Invalid)
                    } else {
                        ProofVerifier::verify(challenge, resp)
                    };

                    let base_score = match verdict {
                        ProofVerdict::Valid => 100u32,
                        ProofVerdict::Suspicious => 50,
                        ProofVerdict::Invalid | ProofVerdict::TimedOut => 0,
                    };

                    // Feature 116 + Item 148: Weight by difficulty multiplier.
                    // Harder proofs (higher difficulty) earn more credit.
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

                    // Item 148: Bonus for high-difficulty proofs.
                    if difficulty > 1.5 && base_score > 0 {
                        let bonus = ((difficulty - 1.0) * 10.0).round() as u32;
                        score = score.saturating_add(bonus);
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

                    // Item 150: Record proof history.
                    let channel_name = match challenge.channel {
                        ResourceChannel::Processing => "Processing",
                        ResourceChannel::Gpu => "Gpu",
                        ResourceChannel::Storage => "Storage",
                        ResourceChannel::Ram => "Ram",
                        ResourceChannel::Bandwidth => "Bandwidth",
                    };
                    self.proof_history.entry(validator).or_default().push(
                        ProofHistoryEntry {
                            epoch: challenge.epoch,
                            channel: channel_name.to_string(),
                            score,
                            compute_time_ms: resp.compute_time_ms,
                            passed: score > 0,
                        },
                    );

                    // Item 160: Accumulate scores for leaderboard.
                    let acc = self.accumulated_scores
                        .entry((validator, challenge.channel))
                        .or_insert((0, 0));
                    acc.0 = acc.0.saturating_add(score);
                    acc.1 += 1;

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
            let diversity_bonus = (channels_contributed.saturating_mul(10)).min(50) as u8;

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

        // Clear per-epoch state (but keep history and leaderboard data).
        self.pending_challenges.clear();
        self.responses.clear();
        self.submission_dedup.clear();
        self.expired_challenges.clear();

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

    // Item 148: Test difficulty-weighted scoring.
    #[test]
    fn item_148_difficulty_weighted_scoring() {
        let mut pm = ProofManager::new(test_addr(1));
        let challenge = ChallengeGenerator::generate(
            0, &[42u8; 32], test_addr(2), ResourceChannel::Processing, 100,
        );
        pm.pending_challenges.insert(challenge.challenge_id, challenge.clone());
        let response = CpuProver::solve(&challenge, test_addr(2));
        pm.record_response(response);

        let mut diffs = HashMap::new();
        diffs.insert(ResourceChannel::Processing, 2.0);
        let summaries = pm.finalize_epoch_with_difficulty(&diffs);

        assert_eq!(summaries.len(), 1);
        // Score should be higher than base 100 due to 2x difficulty + bonus.
        assert!(summaries[0].1.processing_score > 100);
    }

    // Item 150: Test proof history.
    #[test]
    fn item_150_proof_history() {
        let mut pm = ProofManager::new(test_addr(1));
        let challenges = pm.generate_challenges(0, &[42u8; 32], test_addr(1), 100);

        for challenge in &challenges {
            let response = pm.solve_challenge(challenge);
            pm.record_response(response);
        }

        pm.finalize_epoch();

        let history = pm.get_proof_history(&test_addr(1));
        assert_eq!(history.len(), 5); // One per channel.
        assert!(history.iter().all(|h| h.passed));
    }

    // Item 157: Test spam prevention (dedup).
    #[test]
    fn item_157_dedup_prevents_double_submission() {
        let mut pm = ProofManager::new(test_addr(1));
        let challenge = ChallengeGenerator::generate(
            0, &[42u8; 32], test_addr(2), ResourceChannel::Processing, 100,
        );
        pm.pending_challenges.insert(challenge.challenge_id, challenge.clone());

        let response = CpuProver::solve(&challenge, test_addr(2));
        assert!(pm.record_response(response.clone()));
        // Second submission of same proof should be rejected.
        assert!(!pm.record_response(response));
    }

    // Item 159: Test challenge expiry.
    #[test]
    fn item_159_challenge_expiry() {
        let mut pm = ProofManager::new(test_addr(1));
        let challenge = ChallengeGenerator::generate(
            0, &[42u8; 32], test_addr(2), ResourceChannel::Processing, 50,
        );
        pm.pending_challenges.insert(challenge.challenge_id, challenge.clone());

        // Don't submit a response. Set height past deadline.
        pm.current_height = 100;
        pm.expire_challenges();

        assert!(pm.get_expired_challenges().contains(&challenge.challenge_id));
    }

    // Item 160: Test leaderboard.
    #[test]
    fn item_160_leaderboard() {
        let mut pm = ProofManager::new(test_addr(1));

        // Run two validators through finalization.
        for v in 1..=3u8 {
            let challenges = pm.generate_challenges(0, &[42u8; 32], test_addr(v), 100);
            for challenge in &challenges {
                let response = pm.solve_challenge(challenge);
                pm.record_response(response);
            }
        }
        // Manually set our_address so solve_challenge works for each
        pm.finalize_epoch();

        let leaderboard = pm.get_leaderboard(ResourceChannel::Processing, 10);
        assert!(!leaderboard.is_empty());
        assert_eq!(leaderboard[0].rank, 1);
    }

    // Item 152: Test that finalize_epoch handles many validators.
    #[test]
    fn item_152_parallel_verification_readiness() {
        let mut pm = ProofManager::new(test_addr(1));

        // Generate challenges for 10 validators.
        for v in 1..=10u8 {
            let challenges = pm.generate_challenges(0, &[42u8; 32], test_addr(v), 100);
            for challenge in &challenges {
                let response = pm.solve_challenge(challenge);
                pm.record_response(response);
            }
        }

        let summaries = pm.finalize_epoch();
        assert_eq!(summaries.len(), 10);
    }

    #[test]
    fn precomputed_verdicts_match_inline_path() {
        // Two parallel ProofManagers: A finalizes via the legacy inline-verify
        // path; B does it via compute_epoch_verdicts (the spawn_blocking-friendly
        // pure path) followed by finalize_epoch_with_precomputed_verdicts.
        // Their summaries should match on the deterministic fields.
        let our_addr = test_addr(7);
        let target = test_addr(8);

        let mut pm_a = ProofManager::new(our_addr);
        let mut pm_b = ProofManager::new(our_addr);

        // Generate identical challenges in both managers.
        let challenges_a = pm_a.generate_challenges(0, &[42u8; 32], target, 100);
        let challenges_b = pm_b.generate_challenges(0, &[42u8; 32], target, 100);
        assert_eq!(challenges_a.len(), challenges_b.len());

        // Solve all channels and record in both managers.
        let storage = pm_a.storage_data_clone();
        for (ca, cb) in challenges_a.iter().zip(challenges_b.iter()) {
            let resp_a = ProofManager::solve_challenge_pure(ca, &storage, target);
            let resp_b = ProofManager::solve_challenge_pure(cb, &storage, target);
            pm_a.record_response(resp_a);
            pm_b.record_response(resp_b);
        }

        // Path A: legacy inline verification.
        let multipliers = HashMap::new();
        let summaries_a = pm_a.finalize_epoch_with_difficulty(&multipliers);

        // Path B: precompute verdicts (spawn_blocking-friendly), then apply.
        let verdicts = ProofManager::compute_epoch_verdicts(
            &pm_b.pending_challenges,
            &pm_b.responses,
            &pm_b.expired_challenges,
            pm_b.current_height,
        );
        let summaries_b = pm_b.finalize_epoch_with_precomputed_verdicts(&verdicts, &multipliers);

        assert_eq!(summaries_a.len(), summaries_b.len(), "summary count mismatch");
        // Summaries are sorted by HashMap iteration order which is randomised, so
        // sort both by validator addr for stable comparison.
        let mut sa: Vec<_> = summaries_a.into_iter().collect();
        let mut sb: Vec<_> = summaries_b.into_iter().collect();
        sa.sort_by_key(|(a, _)| a.0);
        sb.sort_by_key(|(a, _)| a.0);
        for ((addr_a, sum_a), (addr_b, sum_b)) in sa.iter().zip(sb.iter()) {
            assert_eq!(addr_a, addr_b, "validator address mismatch");
            assert_eq!(sum_a.epoch, sum_b.epoch);
            assert_eq!(sum_a.processing_score, sum_b.processing_score, "processing_score mismatch");
            assert_eq!(sum_a.gpu_score, sum_b.gpu_score);
            assert_eq!(sum_a.storage_score, sum_b.storage_score);
            assert_eq!(sum_a.ram_score, sum_b.ram_score);
            assert_eq!(sum_a.bandwidth_score, sum_b.bandwidth_score);
            assert_eq!(sum_a.diversity_bonus, sum_b.diversity_bonus);
        }

        // Both paths should clear per-epoch state.
        assert!(pm_a.pending_challenges.is_empty());
        assert!(pm_b.pending_challenges.is_empty());
    }

    #[test]
    fn solve_challenge_pure_works_for_all_channels() {
        // Verifies the new static helper dispatches correctly for each ResourceChannel
        // and propagates challenge_id + validator. Result bytes are not asserted equal
        // across calls because some provers (PoW-style) use random nonces and are
        // non-deterministic between invocations.
        let our_address = test_addr(7);
        let mut pm = ProofManager::new(our_address);
        let epoch_seed = [42u8; 32];
        let challenges = pm.generate_challenges(0, &epoch_seed, our_address, 100);
        let storage_data = pm.storage_data_clone();

        assert_eq!(challenges.len(), 5, "expected 5 challenges (one per ResourceChannel)");

        for challenge in &challenges {
            let via_static = ProofManager::solve_challenge_pure(
                challenge, &storage_data, our_address,
            );
            assert_eq!(
                via_static.challenge_id, challenge.challenge_id,
                "challenge_id mismatch on channel {:?}", challenge.channel,
            );
            assert_eq!(
                via_static.validator, our_address,
                "validator mismatch on channel {:?}", challenge.channel,
            );
            assert!(
                !via_static.result.is_empty(),
                "empty result on channel {:?}", challenge.channel,
            );
        }
    }
}
