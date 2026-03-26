#![allow(dead_code)]
use serde::{Deserialize, Serialize};

/// Result of a job cancellation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancellationResult {
    pub job_id_hex: String,
    pub refund_amount: u64,
    pub cancellation_fee: u64,
    pub success: bool,
}

/// Calculate the refund and fee for a cancellation.
/// 98% refund, 2% burned as cancellation fee.
pub fn calculate_cancellation(comme_budget: u64) -> (u64, u64) {
    let fee = comme_budget * 2 / 100;
    let refund = comme_budget - fee;
    (refund, fee)
}

/// Process a job cancellation request.
pub fn process_cancellation(
    job_id_hex: &str,
    _submitter_hex: &str,
    comme_budget: u64,
) -> CancellationResult {
    let (refund, fee) = calculate_cancellation(comme_budget);
    CancellationResult {
        job_id_hex: job_id_hex.to_string(),
        refund_amount: refund,
        cancellation_fee: fee,
        success: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cancellation_fee() {
        let (refund, fee) = calculate_cancellation(100_000_000);
        assert_eq!(fee, 2_000_000);
        assert_eq!(refund, 98_000_000);
        assert_eq!(refund + fee, 100_000_000);
    }

    #[test]
    fn test_cancellation_zero_budget() {
        let (refund, fee) = calculate_cancellation(0);
        assert_eq!(refund, 0);
        assert_eq!(fee, 0);
    }

    #[test]
    fn test_cancellation_small_budget() {
        // Budget of 50 raw units: 2% = 1, refund = 49
        let (refund, fee) = calculate_cancellation(50);
        assert_eq!(fee, 1);
        assert_eq!(refund, 49);
    }

    #[test]
    fn test_process_cancellation() {
        let result = process_cancellation("abc123", "submitter_hex", 500_000_000);
        assert!(result.success);
        assert_eq!(result.job_id_hex, "abc123");
        assert_eq!(result.refund_amount, 490_000_000);
        assert_eq!(result.cancellation_fee, 10_000_000);
    }
}
