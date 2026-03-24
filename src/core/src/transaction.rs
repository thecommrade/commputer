use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Sha256, Digest};
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
}

/// A signed transaction on the Commputer network.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Transaction {
    pub from: Address,
    pub nonce: u64,
    pub kind: TxKind,
    /// Signature over (from || nonce || kind).
    pub signature: Vec<u8>,
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
        matches!(
            self.kind,
            TxKind::BurstCompute { .. }
            | TxKind::MilestoneBurn { .. }
            | TxKind::CharitableDonation { .. }
        )
    }

    /// The amount of $COMME burned by this transaction, if any.
    pub fn burn_amount(&self) -> Amount {
        match &self.kind {
            TxKind::BurstCompute { burn_amount, .. } => *burn_amount,
            TxKind::MilestoneBurn { burn_amount, .. } => *burn_amount,
            TxKind::CharitableDonation { burn_amount, .. } => *burn_amount,
            _ => Amount::ZERO,
        }
    }
}
