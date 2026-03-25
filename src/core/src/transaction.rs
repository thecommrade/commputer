use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Sha256, Digest};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use crate::identity::Address;
use crate::token::Amount;
use crate::proof::ResourceChannel;

/// A 32-byte transaction hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct TxHash(pub [u8; 32]);

/// The different types of transactions on the Commputer network.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum TxKind {
    /// Standard transfer between wallets.
    Transfer {
        to: Address,
        amount: Amount,
    },

    /// Register as a new validator on the network.
    ValidatorRegister {
        /// Initial hardware fingerprint.
        hardware_fingerprint_hash: [u8; 32],
        /// Initial resource capacity declaration.
        contribution_percent: u8,
    },

    /// Update validator resource allocation (e.g., changing contribution slider).
    ValidatorUpdate {
        contribution_percent: u8,
    },

    /// Deregister as a validator.
    ValidatorExit,

    /// Spend $COMME on burst compute. The spent amount is BURNED.
    BurstCompute {
        /// What resource type to burst.
        channel: ResourceChannel,
        /// Amount of $COMME to burn for burst capacity.
        burn_amount: Amount,
        /// Job specification hash (details stored off-chain).
        job_hash: [u8; 32],
    },

    /// Milestone burn — triggered by the protocol when a capacity milestone is reached.
    MilestoneBurn {
        milestone_id: u64,
        burn_amount: Amount,
        description_hash: [u8; 32],
    },

    /// Annual charitable donation vote.
    CharitableVote {
        /// Epoch of the annual vote.
        vote_epoch: u64,
        /// Hash of the charity proposal being voted for.
        proposal_hash: [u8; 32],
    },

    /// Execute a charitable donation (protocol-triggered after vote concludes).
    CharitableDonation {
        vote_epoch: u64,
        /// Amount sold for donation.
        sell_amount: Amount,
        /// Matching amount burned.
        burn_amount: Amount,
        /// Hash of the receiving organization details.
        recipient_hash: [u8; 32],
    },

    /// Storage will — designate contacts for data in case of extended absence or death.
    StorageWill {
        /// Email address hashes for notification.
        contact_hashes: Vec<[u8; 32]>,
        /// Custom execution options hash.
        options_hash: [u8; 32],
    },

    /// Feature 144: Compliance appeal — nerfed validator submits proof of compliance.
    ComplianceAppeal {
        /// Hash of the compliance proof data.
        proof_hash: [u8; 32],
    },

    /// Feature 246: Batch multiple operations into a single transaction.
    Batch {
        operations: Vec<TxKind>,
    },

    /// Feature 258: Key rotation — allow validators to rotate signing key.
    KeyRotation {
        new_public_key: Vec<u8>,
    },

    /// Feature 259: Multi-signature transaction.
    MultiSig {
        threshold: u8,
        signers: Vec<Vec<u8>>,
        signatures: Vec<Vec<u8>>,
    },

    /// Feature 52: Submit a compute job to the network. Burns comme_budget.
    SubmitJob {
        job_spec_hash: [u8; 32],
        resources: crate::compute::ResourceRequirements,
        max_duration_secs: u64,
        comme_budget: Amount,
        l2_id: Option<String>,
    },

    /// Feature 53: Validator claims a pending compute job.
    ClaimJob {
        job_id: [u8; 32],
    },

    /// Feature 54: Executor submits result hash for a compute job.
    CompleteJob {
        job_id: [u8; 32],
        result_hash: [u8; 32],
    },

    /// Feature 55: Verifier disputes a job result.
    DisputeJob {
        job_id: [u8; 32],
        evidence_hash: [u8; 32],
    },
}

/// Minimum transaction fee in raw units (0.0001 COMME = 100_000 raw units).
pub const MINIMUM_FEE: u64 = 100_000;

/// Feature 13: Account creation cost — new accounts require a higher fee (0.001 COMME).
pub const ACCOUNT_CREATION_FEE: u64 = 1_000_000;

/// Feature 14: Dust limit — transfers below this amount are rejected (0.0001 COMME).
pub const DUST_LIMIT: u64 = 10_000;

/// Minimum balance required to register as a validator (0.1 COMME = 10_000_000 raw units).
pub const MINIMUM_VALIDATOR_STAKE: u64 = 10_000_000;

/// Number of blocks a newly registered validator must wait before participating in consensus.
pub const VALIDATOR_COOLDOWN_BLOCKS: u64 = 10;

/// A signed transaction on the Commputer network.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Transaction {
    pub from: Address,
    pub nonce: u64,
    pub kind: TxKind,
    /// Transaction fee in raw units. Burned on inclusion (not paid to validators).
    pub fee: u64,
    /// Signature over (from || nonce || kind || fee).
    pub signature: Vec<u8>,
    /// Sender's ed25519 public key (32 bytes). Required for signature verification.
    pub public_key: Vec<u8>,
    /// Feature 251: Optional memo (max 256 bytes).
    #[serde(default)]
    pub memo: Option<Vec<u8>>,
    /// Feature 260: Optional timelock — transaction valid only after this block height.
    #[serde(default)]
    pub timelock: Option<u64>,
}

impl Transaction {
    /// Compute the hash of this transaction.
    pub fn hash(&self) -> TxHash {
        let encoded = borsh::to_vec(self).expect("tx serialization should not fail");
        let hash = Sha256::digest(&encoded);
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        TxHash(out)
    }

    /// Whether this transaction burns $COMME.
    pub fn is_burn(&self) -> bool {
        match &self.kind {
            TxKind::BurstCompute { .. }
            | TxKind::MilestoneBurn { .. }
            | TxKind::CharitableDonation { .. }
            | TxKind::SubmitJob { .. } => true,
            TxKind::Batch { operations } => operations.iter().any(|op| matches!(op,
                TxKind::BurstCompute { .. } | TxKind::MilestoneBurn { .. } | TxKind::CharitableDonation { .. } | TxKind::SubmitJob { .. }
            )),
            _ => false,
        }
    }

    /// Verify the transaction's ed25519 signature using the embedded public key.
    /// Returns true only if: public key is valid, matches the sender address, and signature checks out.
    pub fn verify(&self) -> bool {
        if self.public_key.len() != 32 || self.signature.len() != 64 {
            return false;
        }
        let pk_bytes: &[u8; 32] = match self.public_key.as_slice().try_into() {
            Ok(b) => b,
            Err(_) => return false,
        };
        let vk = match VerifyingKey::from_bytes(pk_bytes) {
            Ok(vk) => vk,
            Err(_) => return false,
        };
        // Verify the public key matches the sender address (prevents key substitution)
        if Address::from_public_key(&vk) != self.from {
            return false;
        }
        let sig_bytes: &[u8; 64] = match self.signature.as_slice().try_into() {
            Ok(b) => b,
            Err(_) => return false,
        };
        let sig = Signature::from_bytes(sig_bytes);
        // Sign over (from || nonce || kind || fee)
        let mut bytes = Vec::new();
        borsh::BorshSerialize::serialize(&self.from, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.nonce, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.kind, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.fee, &mut bytes).unwrap();
        vk.verify(&bytes, &sig).is_ok()
    }

    /// The amount of $COMME burned by this transaction, if any.
    pub fn burn_amount(&self) -> Amount {
        match &self.kind {
            TxKind::BurstCompute { burn_amount, .. } => *burn_amount,
            TxKind::MilestoneBurn { burn_amount, .. } => *burn_amount,
            TxKind::CharitableDonation { burn_amount, .. } => *burn_amount,
            TxKind::SubmitJob { comme_budget, .. } => *comme_budget,
            TxKind::Batch { operations } => {
                let mut total = Amount::ZERO;
                for op in operations {
                    match op {
                        TxKind::BurstCompute { burn_amount, .. }
                        | TxKind::MilestoneBurn { burn_amount, .. }
                        | TxKind::CharitableDonation { burn_amount, .. } => {
                            total = total.checked_add(*burn_amount).unwrap_or(total);
                        }
                        TxKind::SubmitJob { comme_budget, .. } => {
                            total = total.checked_add(*comme_budget).unwrap_or(total);
                        }
                        _ => {}
                    }
                }
                total
            }
            _ => Amount::ZERO,
        }
    }

    /// Feature 251: Maximum memo length in bytes.
    pub const MAX_MEMO_LENGTH: usize = 256;

    /// Feature 246: Maximum batch size.
    pub const MAX_BATCH_SIZE: usize = 10;
}
