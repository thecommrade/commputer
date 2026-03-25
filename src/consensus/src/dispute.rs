use std::collections::HashMap;
use commputer_core::identity::Address;
use commputer_storage::job_pool::JobId;

/// Outcome of a dispute resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisputeOutcome {
    /// Original executor's result was correct.
    OriginalCorrect,
    /// Challenger was correct; executor should be slashed.
    ChallengerCorrect { slash_amount: u64 },
    /// No clear majority among re-executors.
    Inconclusive,
}

/// State of an active dispute.
#[derive(Debug, Clone)]
pub struct DisputeState {
    pub job_id: JobId,
    pub original_executor: Address,
    pub original_result_hash: [u8; 32],
    pub challenger: Address,
    pub re_executors: Vec<Address>,
    pub results: HashMap<Address, [u8; 32]>,
    pub resolved: bool,
    pub job_budget: u64,
}

/// Number of re-executors required for dispute resolution.
pub const DISPUTE_RE_EXECUTORS: usize = 3;

/// Slash percentage of job budget for incorrect executor (50%).
pub const SLASH_PERCENT: u64 = 50;

/// Resolve a dispute by majority vote of re-executors.
///
/// Rules:
/// - 3 re-executors re-run the job.
/// - The majority result wins.
/// - If original executor's result matches majority, OriginalCorrect.
/// - If challenger is correct (original doesn't match majority), ChallengerCorrect with slash.
/// - If no clear majority (all 3 different), Inconclusive.
pub fn resolve_dispute(state: &DisputeState) -> DisputeOutcome {
    if state.results.len() < DISPUTE_RE_EXECUTORS {
        return DisputeOutcome::Inconclusive;
    }

    // Count occurrences of each result hash
    let mut hash_counts: HashMap<[u8; 32], usize> = HashMap::new();
    for hash in state.results.values() {
        *hash_counts.entry(*hash).or_default() += 1;
    }

    // Find the majority result (>= 2 out of 3)
    let majority_hash = hash_counts
        .iter()
        .find(|(_, count)| **count >= 2)
        .map(|(hash, _)| *hash);

    match majority_hash {
        Some(hash) if hash == state.original_result_hash => {
            // Original was correct
            DisputeOutcome::OriginalCorrect
        }
        Some(_) => {
            // Original was wrong, slash the executor
            let slash_amount = state.job_budget * SLASH_PERCENT / 100;
            DisputeOutcome::ChallengerCorrect { slash_amount }
        }
        None => {
            // No majority — all 3 results are different
            DisputeOutcome::Inconclusive
        }
    }
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

    fn make_dispute(
        original_hash: [u8; 32],
        re_results: Vec<(u8, [u8; 32])>,
        budget: u64,
    ) -> DisputeState {
        let mut results = HashMap::new();
        for (addr_byte, hash) in re_results {
            results.insert(make_address(addr_byte), hash);
        }
        DisputeState {
            job_id: make_job_id(1),
            original_executor: make_address(0x01),
            original_result_hash: original_hash,
            challenger: make_address(0x02),
            re_executors: vec![make_address(10), make_address(11), make_address(12)],
            results,
            resolved: false,
            job_budget: budget,
        }
    }

    #[test]
    fn test_original_correct_unanimous() {
        let hash = [0xAA; 32];
        let dispute = make_dispute(
            hash,
            vec![(10, hash), (11, hash), (12, hash)],
            10000,
        );
        assert_eq!(resolve_dispute(&dispute), DisputeOutcome::OriginalCorrect);
    }

    #[test]
    fn test_original_correct_majority() {
        let original = [0xAA; 32];
        let different = [0xBB; 32];
        let dispute = make_dispute(
            original,
            vec![(10, original), (11, original), (12, different)],
            10000,
        );
        assert_eq!(resolve_dispute(&dispute), DisputeOutcome::OriginalCorrect);
    }

    #[test]
    fn test_challenger_correct_unanimous() {
        let original = [0xAA; 32];
        let correct = [0xBB; 32];
        let dispute = make_dispute(
            original,
            vec![(10, correct), (11, correct), (12, correct)],
            10000,
        );
        assert_eq!(
            resolve_dispute(&dispute),
            DisputeOutcome::ChallengerCorrect {
                slash_amount: 5000
            }
        );
    }

    #[test]
    fn test_challenger_correct_majority() {
        let original = [0xAA; 32];
        let correct = [0xBB; 32];
        let dispute = make_dispute(
            original,
            vec![(10, correct), (11, correct), (12, original)],
            10000,
        );
        assert_eq!(
            resolve_dispute(&dispute),
            DisputeOutcome::ChallengerCorrect {
                slash_amount: 5000
            }
        );
    }

    #[test]
    fn test_inconclusive_all_different() {
        let original = [0xAA; 32];
        let dispute = make_dispute(
            original,
            vec![(10, [0xBB; 32]), (11, [0xCC; 32]), (12, [0xDD; 32])],
            10000,
        );
        assert_eq!(resolve_dispute(&dispute), DisputeOutcome::Inconclusive);
    }

    #[test]
    fn test_insufficient_re_executors() {
        let original = [0xAA; 32];
        let dispute = make_dispute(original, vec![(10, original)], 10000);
        assert_eq!(resolve_dispute(&dispute), DisputeOutcome::Inconclusive);
    }

    #[test]
    fn test_slash_amount_calculation() {
        let original = [0xAA; 32];
        let correct = [0xBB; 32];
        let dispute = make_dispute(
            original,
            vec![(10, correct), (11, correct), (12, correct)],
            20000,
        );
        if let DisputeOutcome::ChallengerCorrect { slash_amount } = resolve_dispute(&dispute) {
            assert_eq!(slash_amount, 10000); // 50% of 20000
        } else {
            panic!("Expected ChallengerCorrect");
        }
    }

    #[test]
    fn test_zero_budget_slash() {
        let original = [0xAA; 32];
        let correct = [0xBB; 32];
        let dispute = make_dispute(
            original,
            vec![(10, correct), (11, correct), (12, correct)],
            0,
        );
        if let DisputeOutcome::ChallengerCorrect { slash_amount } = resolve_dispute(&dispute) {
            assert_eq!(slash_amount, 0);
        } else {
            panic!("Expected ChallengerCorrect");
        }
    }

    #[test]
    fn test_empty_results() {
        let dispute = make_dispute([0xAA; 32], vec![], 10000);
        assert_eq!(resolve_dispute(&dispute), DisputeOutcome::Inconclusive);
    }
}
