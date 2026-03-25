use serde::{Deserialize, Serialize};

/// Access level based on COMME holdings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComputeAccessLevel {
    None,
    ReadOnly,        // < 1 COMME
    StorageJobs,     // 1-9 COMME (can submit storage jobs)
    ComputeJobs,     // 10-19 COMME (can submit compute jobs)
    FullAccess,      // 20-32 COMME (can submit any job including GPU)
    UnlimitedAccess, // 33+ COMME (champion tier, all access + priority)
}

/// Balance thresholds for each tier (in raw units, 1 COMME = 100_000_000).
pub const TIER_READ_ONLY: u64 = 0;
pub const TIER_STORAGE: u64 = 100_000_000; // 1 COMME
pub const TIER_COMPUTE: u64 = 1_000_000_000; // 10 COMME
pub const TIER_FULL: u64 = 2_000_000_000; // 20 COMME
pub const TIER_UNLIMITED: u64 = 3_300_000_000; // 33 COMME

/// Determine the compute access level for a given balance.
pub fn access_level(balance: u64) -> ComputeAccessLevel {
    if balance >= TIER_UNLIMITED {
        ComputeAccessLevel::UnlimitedAccess
    } else if balance >= TIER_FULL {
        ComputeAccessLevel::FullAccess
    } else if balance >= TIER_COMPUTE {
        ComputeAccessLevel::ComputeJobs
    } else if balance >= TIER_STORAGE {
        ComputeAccessLevel::StorageJobs
    } else if balance > 0 {
        ComputeAccessLevel::ReadOnly
    } else {
        ComputeAccessLevel::None
    }
}

/// Check if a balance qualifies for GPU job submission.
pub fn can_submit_gpu_job(balance: u64) -> bool {
    matches!(
        access_level(balance),
        ComputeAccessLevel::FullAccess | ComputeAccessLevel::UnlimitedAccess
    )
}

/// Check if a balance qualifies for compute job submission.
pub fn can_submit_compute_job(balance: u64) -> bool {
    matches!(
        access_level(balance),
        ComputeAccessLevel::ComputeJobs
            | ComputeAccessLevel::FullAccess
            | ComputeAccessLevel::UnlimitedAccess
    )
}

/// Check if a balance qualifies for storage job submission.
pub fn can_submit_storage_job(balance: u64) -> bool {
    !matches!(
        access_level(balance),
        ComputeAccessLevel::None | ComputeAccessLevel::ReadOnly
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_access_levels() {
        assert_eq!(access_level(0), ComputeAccessLevel::None);
        assert_eq!(access_level(1), ComputeAccessLevel::ReadOnly);
        assert_eq!(access_level(99_999_999), ComputeAccessLevel::ReadOnly);
        assert_eq!(access_level(100_000_000), ComputeAccessLevel::StorageJobs);
        assert_eq!(access_level(999_999_999), ComputeAccessLevel::StorageJobs);
        assert_eq!(access_level(1_000_000_000), ComputeAccessLevel::ComputeJobs);
        assert_eq!(access_level(1_999_999_999), ComputeAccessLevel::ComputeJobs);
        assert_eq!(access_level(2_000_000_000), ComputeAccessLevel::FullAccess);
        assert_eq!(access_level(3_299_999_999), ComputeAccessLevel::FullAccess);
        assert_eq!(access_level(3_300_000_000), ComputeAccessLevel::UnlimitedAccess);
        assert_eq!(access_level(10_000_000_000), ComputeAccessLevel::UnlimitedAccess);
    }

    #[test]
    fn test_gpu_access() {
        assert!(!can_submit_gpu_job(0));
        assert!(!can_submit_gpu_job(100_000_000));
        assert!(!can_submit_gpu_job(1_000_000_000));
        assert!(can_submit_gpu_job(2_000_000_000));
        assert!(can_submit_gpu_job(3_300_000_000));
    }

    #[test]
    fn test_compute_access() {
        assert!(!can_submit_compute_job(0));
        assert!(!can_submit_compute_job(99_999_999));
        assert!(can_submit_compute_job(1_000_000_000));
        assert!(can_submit_compute_job(2_000_000_000));
        assert!(can_submit_compute_job(3_300_000_000));
    }

    #[test]
    fn test_storage_access() {
        assert!(!can_submit_storage_job(0));
        assert!(!can_submit_storage_job(50_000_000));
        assert!(can_submit_storage_job(100_000_000));
        assert!(can_submit_storage_job(1_000_000_000));
        assert!(can_submit_storage_job(5_000_000_000));
    }
}
