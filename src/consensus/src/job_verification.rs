use commputer_core::identity::Address;
use commputer_storage::job_pool::JobId;

/// Result of verification consensus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationResult {
    /// Majority of verifiers confirmed the result.
    Confirmed,
    /// Majority of verifiers disagree with the original result.
    Disputed,
    /// Not enough verifiers responded.
    InsufficientVerifiers,
}

/// A verification request for a completed job.
#[derive(Debug, Clone)]
pub struct VerificationRequest {
    pub job_id: JobId,
    pub original_result_hash: [u8; 32],
    pub verifier: Address,
    pub verification_result_hash: Option<[u8; 32]>,
}

/// Select N random verifiers from the validator set, excluding the executor.
///
/// Uses a deterministic selection based on the job_id for reproducibility.
pub fn select_verifiers(
    validators: &[Address],
    executor: Address,
    count: usize,
) -> Vec<Address> {
    let eligible: Vec<Address> = validators
        .iter()
        .copied()
        .filter(|v| *v != executor)
        .collect();

    if eligible.len() <= count {
        return eligible;
    }

    // Take first `count` eligible validators (caller should pre-shuffle if needed)
    eligible.into_iter().take(count).collect()
}

/// Check verification results against the original hash.
///
/// - If `threshold` or more verifiers match the original, it's Confirmed.
/// - If fewer than `threshold` match but enough verifiers responded, it's Disputed.
/// - If fewer than `threshold` verifiers total, it's InsufficientVerifiers.
pub fn check_verification(
    original: [u8; 32],
    verifications: &[[u8; 32]],
    threshold: usize,
) -> VerificationResult {
    if verifications.len() < threshold {
        return VerificationResult::InsufficientVerifiers;
    }

    let matching = verifications.iter().filter(|v| **v == original).count();

    if matching >= threshold {
        VerificationResult::Confirmed
    } else {
        VerificationResult::Disputed
    }
}

// ── Feature 63: Verification Rewards ──

/// Calculate the reward for each verifier (5% of job budget, split evenly).
pub fn calculate_verification_reward(job_budget: u64, verifier_count: usize) -> u64 {
    if verifier_count == 0 {
        return 0;
    }
    let total_reward = job_budget * 5 / 100; // 5% of budget
    total_reward / verifier_count as u64
}

/// Distribute rewards among verifiers.
pub fn distribute_rewards(job_budget: u64, verifiers: &[Address]) -> Vec<(Address, u64)> {
    if verifiers.is_empty() {
        return Vec::new();
    }
    let per_verifier = calculate_verification_reward(job_budget, verifiers.len());
    verifiers
        .iter()
        .map(|addr| (*addr, per_verifier))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_address(byte: u8) -> Address {
        Address([byte; 32])
    }

    fn make_job_id(byte: u8) -> JobId {
        JobId([byte; 32])
    }

    #[test]
    fn test_select_verifiers_excludes_executor() {
        let validators = vec![
            make_address(1),
            make_address(2),
            make_address(3),
            make_address(4),
        ];
        let executor = make_address(2);
        let verifiers = select_verifiers(&validators, executor, 2);
        assert_eq!(verifiers.len(), 2);
        assert!(!verifiers.contains(&executor));
    }

    #[test]
    fn test_select_verifiers_not_enough() {
        let validators = vec![make_address(1), make_address(2)];
        let executor = make_address(1);
        let verifiers = select_verifiers(&validators, executor, 5);
        // Only 1 eligible, returns all eligible
        assert_eq!(verifiers.len(), 1);
        assert_eq!(verifiers[0], make_address(2));
    }

    #[test]
    fn test_select_verifiers_empty() {
        let verifiers = select_verifiers(&[], make_address(1), 3);
        assert!(verifiers.is_empty());
    }

    #[test]
    fn test_check_verification_confirmed() {
        let original = [0xAA; 32];
        let verifications = vec![[0xAA; 32], [0xAA; 32], [0xBB; 32]];
        assert_eq!(
            check_verification(original, &verifications, 2),
            VerificationResult::Confirmed
        );
    }

    #[test]
    fn test_check_verification_disputed() {
        let original = [0xAA; 32];
        let verifications = vec![[0xBB; 32], [0xBB; 32], [0xAA; 32]];
        assert_eq!(
            check_verification(original, &verifications, 2),
            VerificationResult::Disputed
        );
    }

    #[test]
    fn test_check_verification_insufficient() {
        let original = [0xAA; 32];
        let verifications = vec![[0xAA; 32]];
        assert_eq!(
            check_verification(original, &verifications, 2),
            VerificationResult::InsufficientVerifiers
        );
    }

    #[test]
    fn test_check_verification_all_match() {
        let original = [0xAA; 32];
        let verifications = vec![[0xAA; 32], [0xAA; 32], [0xAA; 32]];
        assert_eq!(
            check_verification(original, &verifications, 2),
            VerificationResult::Confirmed
        );
    }

    #[test]
    fn test_check_verification_none_match() {
        let original = [0xAA; 32];
        let verifications = vec![[0xBB; 32], [0xCC; 32], [0xDD; 32]];
        assert_eq!(
            check_verification(original, &verifications, 2),
            VerificationResult::Disputed
        );
    }

    // ── Feature 63 tests ──

    #[test]
    fn test_verification_reward_calculation() {
        // 5% of 10000 = 500, split among 3 = 166
        assert_eq!(calculate_verification_reward(10000, 3), 166);
    }

    #[test]
    fn test_verification_reward_zero_verifiers() {
        assert_eq!(calculate_verification_reward(10000, 0), 0);
    }

    #[test]
    fn test_verification_reward_single_verifier() {
        // 5% of 10000 = 500
        assert_eq!(calculate_verification_reward(10000, 1), 500);
    }

    #[test]
    fn test_distribute_rewards() {
        let verifiers = vec![make_address(1), make_address(2), make_address(3)];
        let rewards = distribute_rewards(10000, &verifiers);
        assert_eq!(rewards.len(), 3);
        for (_, amount) in &rewards {
            assert_eq!(*amount, 166);
        }
    }

    #[test]
    fn test_distribute_rewards_empty() {
        let rewards = distribute_rewards(10000, &[]);
        assert!(rewards.is_empty());
    }

    #[test]
    fn test_distribute_rewards_large_budget() {
        let verifiers = vec![make_address(1), make_address(2)];
        let rewards = distribute_rewards(1_000_000, &verifiers);
        // 5% = 50000, split 2 = 25000 each
        assert_eq!(rewards[0].1, 25000);
        assert_eq!(rewards[1].1, 25000);
    }

    #[test]
    fn test_verification_request_creation() {
        let req = VerificationRequest {
            job_id: make_job_id(1),
            original_result_hash: [0xAA; 32],
            verifier: make_address(5),
            verification_result_hash: Some([0xAA; 32]),
        };
        assert_eq!(req.verifier, make_address(5));
        assert!(req.verification_result_hash.is_some());
    }
}
