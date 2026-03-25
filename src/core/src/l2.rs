//! L2 registration and state commitment types (#43, #44)
use serde::{Deserialize, Serialize};
use crate::identity::Address;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct L2Registration {
    pub l2_name: String,
    pub l2_chain_id: String,
    pub bridge_address: Address,
    pub operator: Address,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct L2StateCommitment {
    pub l2_chain_id: String,
    pub state_root: [u8; 32],
    pub block_height: u64,
    pub timestamp: u64,
}

/// The flagship L2 identifier that gets 51% capacity priority.
pub const FLAGSHIP_L2_ID: &str = "commputer-analytics-l2";

/// Extended L2 registration with height and flagship status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct L2RegistrationExtended {
    pub l2_id: String,
    pub operator: Address,
    pub registered_at_height: u64,
    pub is_flagship: bool,
}

impl L2RegistrationExtended {
    /// Create a new registration, automatically detecting flagship status.
    pub fn new(l2_id: String, operator: Address, height: u64) -> Self {
        let is_flagship = is_flagship(&l2_id);
        Self {
            l2_id,
            operator,
            registered_at_height: height,
            is_flagship,
        }
    }
}

/// Check if an L2 ID is the flagship L2.
pub fn is_flagship(l2_id: &str) -> bool {
    l2_id == FLAGSHIP_L2_ID
}

pub fn verify_commitment(commitment: &L2StateCommitment) -> bool {
    !commitment.l2_chain_id.is_empty() && commitment.state_root != [0u8; 32] && commitment.block_height > 0 && commitment.timestamp > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    fn addr() -> Address { Address([1u8; 32]) }
    fn reg() -> L2Registration { L2Registration { l2_name: "TestL2".into(), l2_chain_id: "test-001".into(), bridge_address: addr(), operator: addr() } }
    fn commit() -> L2StateCommitment { L2StateCommitment { l2_chain_id: "test-001".into(), state_root: [0xAB; 32], block_height: 100, timestamp: 1700000000 } }
    #[test] fn test_reg_fields() { let r = reg(); assert_eq!(r.l2_name, "TestL2"); }
    #[test] fn test_reg_serde() { let r = reg(); let j = serde_json::to_string(&r).unwrap(); let d: L2Registration = serde_json::from_str(&j).unwrap(); assert_eq!(r, d); }
    #[test] fn test_valid_commit() { assert!(verify_commitment(&commit())); }
    #[test] fn test_empty_chain() { let mut c = commit(); c.l2_chain_id = String::new(); assert!(!verify_commitment(&c)); }
    #[test] fn test_zero_root() { let mut c = commit(); c.state_root = [0u8; 32]; assert!(!verify_commitment(&c)); }
    #[test] fn test_zero_height() { let mut c = commit(); c.block_height = 0; assert!(!verify_commitment(&c)); }
    #[test] fn test_zero_ts() { let mut c = commit(); c.timestamp = 0; assert!(!verify_commitment(&c)); }
    #[test] fn test_commit_serde() { let c = commit(); let j = serde_json::to_string(&c).unwrap(); let d: L2StateCommitment = serde_json::from_str(&j).unwrap(); assert_eq!(c, d); }

    #[test]
    fn test_flagship_detection() {
        assert!(is_flagship("commputer-analytics-l2"));
        assert!(!is_flagship("other-l2"));
        assert!(!is_flagship(""));
    }

    #[test]
    fn test_flagship_constant() {
        assert_eq!(FLAGSHIP_L2_ID, "commputer-analytics-l2");
    }

    #[test]
    fn test_l2_registration_extended_flagship() {
        let reg = L2RegistrationExtended::new(
            FLAGSHIP_L2_ID.to_string(),
            addr(),
            100,
        );
        assert!(reg.is_flagship);
        assert_eq!(reg.registered_at_height, 100);
    }

    #[test]
    fn test_l2_registration_extended_non_flagship() {
        let reg = L2RegistrationExtended::new(
            "custom-l2".to_string(),
            addr(),
            200,
        );
        assert!(!reg.is_flagship);
        assert_eq!(reg.l2_id, "custom-l2");
    }

    #[test]
    fn test_l2_registration_extended_serde() {
        let reg = L2RegistrationExtended::new(
            "test-l2".to_string(),
            addr(),
            50,
        );
        let json = serde_json::to_string(&reg).unwrap();
        let decoded: L2RegistrationExtended = serde_json::from_str(&json).unwrap();
        assert_eq!(reg, decoded);
    }
}
