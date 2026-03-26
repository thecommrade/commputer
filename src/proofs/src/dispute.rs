//! Item 153: Proof result dispute mechanism.
//!
//! If a validator disagrees with their proof score, they can request
//! re-verification. The dispute manager tracks disputes and handles
//! re-verification with independent verifiers.

use commputer_core::identity::Address;
use commputer_core::proof::ResourceChannel;
use std::collections::HashMap;

/// Status of a dispute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeStatus {
    /// Dispute filed, awaiting re-verification.
    Pending,
    /// Re-verification in progress.
    InProgress,
    /// Original score upheld.
    Upheld,
    /// Score revised in validator's favor.
    Revised,
    /// Dispute rejected (invalid or frivolous).
    Rejected,
    /// Dispute expired (not resolved in time).
    Expired,
}

/// A filed dispute.
#[derive(Debug, Clone)]
pub struct Dispute {
    /// Unique dispute ID.
    pub dispute_id: u64,
    /// The validator filing the dispute.
    pub validator: Address,
    /// Challenge that was disputed.
    pub challenge_id: [u8; 32],
    /// Channel of the challenged proof.
    pub channel: ResourceChannel,
    /// Original score given.
    pub original_score: u32,
    /// Claimed correct score (by validator).
    pub claimed_score: u32,
    /// Reason for dispute.
    pub reason: String,
    /// Current status.
    pub status: DisputeStatus,
    /// Block height when filed.
    pub filed_at_block: u64,
    /// Revised score (if status is Revised).
    pub revised_score: Option<u32>,
}

/// Maximum disputes per validator per epoch.
const MAX_DISPUTES_PER_EPOCH: usize = 3;
/// Maximum blocks before a dispute expires.
const DISPUTE_EXPIRY_BLOCKS: u64 = 300;

/// Manages proof result disputes.
pub struct DisputeManager {
    /// All disputes, keyed by dispute_id.
    disputes: HashMap<u64, Dispute>,
    /// Next dispute ID.
    next_id: u64,
    /// Count of disputes per validator per epoch.
    dispute_counts: HashMap<(Address, u64), usize>,
    /// Current block height.
    pub current_block: u64,
}

impl DisputeManager {
    /// Create a new dispute manager.
    pub fn new() -> Self {
        Self {
            disputes: HashMap::new(),
            next_id: 1,
            dispute_counts: HashMap::new(),
            current_block: 0,
        }
    }

    /// File a new dispute.
    pub fn file_dispute(
        &mut self,
        validator: Address,
        challenge_id: [u8; 32],
        channel: ResourceChannel,
        original_score: u32,
        claimed_score: u32,
        reason: String,
        epoch: u64,
    ) -> Result<u64, String> {
        // Rate limit: max disputes per epoch.
        let count = self.dispute_counts.entry((validator, epoch)).or_insert(0);
        if *count >= MAX_DISPUTES_PER_EPOCH {
            return Err(format!(
                "Max {} disputes per epoch exceeded",
                MAX_DISPUTES_PER_EPOCH
            ));
        }

        // Claimed score must differ from original.
        if claimed_score == original_score {
            return Err("Claimed score matches original — no dispute needed".into());
        }

        let dispute_id = self.next_id;
        self.next_id += 1;
        *count += 1;

        let dispute = Dispute {
            dispute_id,
            validator,
            challenge_id,
            channel,
            original_score,
            claimed_score,
            reason,
            status: DisputeStatus::Pending,
            filed_at_block: self.current_block,
            revised_score: None,
        };

        self.disputes.insert(dispute_id, dispute);
        Ok(dispute_id)
    }

    /// Begin re-verification of a dispute.
    pub fn start_reverification(&mut self, dispute_id: u64) -> Result<(), String> {
        let dispute = self.disputes.get_mut(&dispute_id)
            .ok_or_else(|| "Dispute not found".to_string())?;

        if dispute.status != DisputeStatus::Pending {
            return Err(format!("Dispute is {:?}, not Pending", dispute.status));
        }

        dispute.status = DisputeStatus::InProgress;
        Ok(())
    }

    /// Resolve a dispute with a re-verified score.
    pub fn resolve(
        &mut self,
        dispute_id: u64,
        reverified_score: u32,
    ) -> Result<DisputeStatus, String> {
        let dispute = self.disputes.get_mut(&dispute_id)
            .ok_or_else(|| "Dispute not found".to_string())?;

        if dispute.status != DisputeStatus::InProgress {
            return Err(format!("Dispute is {:?}, not InProgress", dispute.status));
        }

        // If re-verified score differs from original, revise.
        if reverified_score != dispute.original_score {
            dispute.status = DisputeStatus::Revised;
            dispute.revised_score = Some(reverified_score);
        } else {
            dispute.status = DisputeStatus::Upheld;
        }

        Ok(dispute.status)
    }

    /// Reject a dispute as invalid.
    pub fn reject(&mut self, dispute_id: u64, reason: &str) -> Result<(), String> {
        let dispute = self.disputes.get_mut(&dispute_id)
            .ok_or_else(|| "Dispute not found".to_string())?;

        dispute.status = DisputeStatus::Rejected;
        dispute.reason = format!("{} — rejected: {}", dispute.reason, reason);
        Ok(())
    }

    /// Expire old disputes that haven't been resolved.
    pub fn expire_old_disputes(&mut self) {
        for dispute in self.disputes.values_mut() {
            if matches!(dispute.status, DisputeStatus::Pending | DisputeStatus::InProgress) {
                if self.current_block > dispute.filed_at_block + DISPUTE_EXPIRY_BLOCKS {
                    dispute.status = DisputeStatus::Expired;
                }
            }
        }
    }

    /// Get a dispute by ID.
    pub fn get_dispute(&self, dispute_id: u64) -> Option<&Dispute> {
        self.disputes.get(&dispute_id)
    }

    /// Get all disputes for a validator.
    pub fn get_validator_disputes(&self, validator: &Address) -> Vec<&Dispute> {
        self.disputes.values()
            .filter(|d| d.validator == *validator)
            .collect()
    }

    /// Get all pending disputes.
    pub fn pending_disputes(&self) -> Vec<&Dispute> {
        self.disputes.values()
            .filter(|d| d.status == DisputeStatus::Pending)
            .collect()
    }

    /// Total number of disputes.
    pub fn total_disputes(&self) -> usize {
        self.disputes.len()
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
    fn item_153_file_dispute() {
        let mut dm = DisputeManager::new();
        let id = dm.file_dispute(
            test_addr(1), [1u8; 32], ResourceChannel::Processing,
            50, 100, "I should have scored higher".into(), 0,
        ).unwrap();
        assert_eq!(id, 1);

        let dispute = dm.get_dispute(id).unwrap();
        assert_eq!(dispute.status, DisputeStatus::Pending);
        assert_eq!(dispute.original_score, 50);
    }

    #[test]
    fn item_153_resolve_dispute_revised() {
        let mut dm = DisputeManager::new();
        let id = dm.file_dispute(
            test_addr(1), [1u8; 32], ResourceChannel::Gpu,
            50, 90, "Bug in GPU scoring".into(), 0,
        ).unwrap();

        dm.start_reverification(id).unwrap();
        let status = dm.resolve(id, 85).unwrap();
        assert_eq!(status, DisputeStatus::Revised);

        let dispute = dm.get_dispute(id).unwrap();
        assert_eq!(dispute.revised_score, Some(85));
    }

    #[test]
    fn item_153_resolve_dispute_upheld() {
        let mut dm = DisputeManager::new();
        let id = dm.file_dispute(
            test_addr(1), [1u8; 32], ResourceChannel::Storage,
            50, 90, "Test".into(), 0,
        ).unwrap();

        dm.start_reverification(id).unwrap();
        let status = dm.resolve(id, 50).unwrap(); // Same score
        assert_eq!(status, DisputeStatus::Upheld);
    }

    #[test]
    fn item_153_rate_limit() {
        let mut dm = DisputeManager::new();
        for i in 0..3 {
            dm.file_dispute(
                test_addr(1), [i as u8; 32], ResourceChannel::Processing,
                50, 80, "Test".into(), 0,
            ).unwrap();
        }
        // Fourth should fail.
        let result = dm.file_dispute(
            test_addr(1), [99u8; 32], ResourceChannel::Processing,
            50, 80, "Test".into(), 0,
        );
        assert!(result.is_err());
    }

    #[test]
    fn item_153_expire_disputes() {
        let mut dm = DisputeManager::new();
        dm.current_block = 0;
        let id = dm.file_dispute(
            test_addr(1), [1u8; 32], ResourceChannel::Ram,
            50, 80, "Test".into(), 0,
        ).unwrap();

        dm.current_block = 400; // Beyond expiry
        dm.expire_old_disputes();

        assert_eq!(dm.get_dispute(id).unwrap().status, DisputeStatus::Expired);
    }

    #[test]
    fn item_153_same_score_rejected() {
        let mut dm = DisputeManager::new();
        let result = dm.file_dispute(
            test_addr(1), [1u8; 32], ResourceChannel::Bandwidth,
            50, 50, "Same score".into(), 0,
        );
        assert!(result.is_err());
    }

    #[test]
    fn item_153_reject_dispute() {
        let mut dm = DisputeManager::new();
        let id = dm.file_dispute(
            test_addr(1), [1u8; 32], ResourceChannel::Processing,
            50, 80, "Test".into(), 0,
        ).unwrap();

        dm.reject(id, "frivolous").unwrap();
        assert_eq!(dm.get_dispute(id).unwrap().status, DisputeStatus::Rejected);
    }

    #[test]
    fn item_153_pending_disputes() {
        let mut dm = DisputeManager::new();
        dm.file_dispute(test_addr(1), [1u8; 32], ResourceChannel::Processing, 50, 80, "A".into(), 0).unwrap();
        dm.file_dispute(test_addr(2), [2u8; 32], ResourceChannel::Gpu, 30, 70, "B".into(), 0).unwrap();

        assert_eq!(dm.pending_disputes().len(), 2);
    }
}
