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
    /// PoUW P0 / G3: the on-chain job format with the linchpin identity —
    /// `program_hash = sha256(wasm)`, binary `input_hash`, and `da_root` (DA-sampling anchor) —
    /// replacing the opaque `job_spec_hash`. Enables enforced deterministic execution
    /// (`exec_adapter`) + DA sampling. Economically mirrors `SubmitJob` at P0 (burns comme_budget at
    /// submit); P1 converts V2 settlement to escrow-and-split. The legacy `SubmitJob` stays valid and
    /// drains at the migration height (open-Q#11 tx-format versioning).
    SubmitJobV2 {
        program_hash: [u8; 32],
        input_hash: [u8; 32],
        da_root: [u8; 32],
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

    /// Item 13: Mining reward — synthetic tx created during epoch processing.
    /// Shows up in transaction history so users can see their earnings.
    MiningReward {
        /// Recipient validator address.
        to: Address,
        /// Reward amount in raw units.
        amount: Amount,
        /// Epoch in which the reward was earned.
        epoch: u64,
    },

    /// Item 14: Validator deregistration — clean network leave.
    ValidatorDeregister,

    // NOTE: TxKind derives Borsh, which tags enum variants by DECLARATION POSITION. New variants
    // MUST be appended at the END so existing variants keep their discriminants — inserting in the
    // middle would re-tag every later variant and corrupt previously-serialized blocks. (Found by
    // the P2 adversarial review, 2026-06-22.)

    /// PoUW P2 / G2: a committee verifier commits to a job's result during the Committing phase.
    /// `commit = H(result_hash‖salt‖verifier)` (the frozen `commit_reveal::make_commitment`);
    /// `bond` is the verifier's stake, escrowed into the job pot on apply. The verifier is the
    /// tx `from`. Opened later by `Reveal`. Maps to `JobLifecycle::record_commit` once the
    /// committee draw (P2 protected wiring) is live; inert (accept + nonce, no escrow) until then.
    Commit {
        job_id: [u8; 32],
        commit: [u8; 32],
        bond: Amount,
    },
    /// PoUW P2 / G2: a committee verifier reveals `(result_hash, salt)` opening its `Commit`
    /// during the Revealing phase (the verifier is the tx `from`). Validated against the stored
    /// commitment by the frozen `commit_reveal::reveal_matches`. Maps to
    /// `JobLifecycle::record_reveal`; inert (accept + nonce) until the P2 wiring is live.
    Reveal {
        job_id: [u8; 32],
        result_hash: [u8; 32],
        salt: [u8; 32],
    },

    /// PoUW P2 / G4: bond `amount` from spendable balance into active bonded stake — the
    /// committee-selection weight (`stake_of`). Routes to `ChainState::bond`. Permissionless:
    /// any account may bond (bonding is the on-ramp to committee eligibility; the validator
    /// filter is applied later at the committee draw). Not a burn — value moves balance->bonded,
    /// supply is conserved. The staker is the tx `from`.
    Bond {
        amount: Amount,
    },
    /// PoUW P2 / G4: request to unbond `amount` — moves it from active bonded into a cooldown
    /// chunk maturing at `now + unbonding_blocks` (`now` = current chain tip height, NOT a tx
    /// field). Stops counting toward selection immediately but stays slashable. Routes to
    /// `ChainState::request_unbond`. The staker is the tx `from`.
    RequestUnbond {
        amount: Amount,
    },
    /// PoUW P2 / G4: withdraw ALL matured cooldown chunks back to spendable balance. `now` (the
    /// current chain tip height) is derived at apply time, NOT a tx field. Routes to
    /// `ChainState::withdraw_unbonded` (saturating; never errors). The staker is the tx `from`.
    WithdrawUnbonded,
}

/// Minimum transaction fee in raw units (0.0001 COMME = 100_000 raw units).
pub const MINIMUM_FEE: u64 = 100_000;

/// Feature 13: Account creation cost — new accounts require a higher fee (0.001 COMME).
pub const ACCOUNT_CREATION_FEE: u64 = 1_000_000;

/// Feature 14: Dust limit — transfers below this amount are rejected (0.0001 COMME).
pub const DUST_LIMIT: u64 = 10_000;

/// Minimum balance required to register as a validator (0.1 COMME = 10_000_000 raw units).
pub const MINIMUM_VALIDATOR_STAKE: u64 = 10_000_000;

/// During the first BOOTSTRAP_REGISTRATION_BLOCKS, validator registration is
/// exempt from the stake requirement so early joiners can register before
/// they have any COMME. After this height, stake is enforced.
pub const BOOTSTRAP_REGISTRATION_BLOCKS: u64 = 1000;

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
            | TxKind::SubmitJob { .. }
            | TxKind::SubmitJobV2 { .. } => true,
            TxKind::Batch { operations } => operations.iter().any(|op| matches!(op,
                TxKind::BurstCompute { .. } | TxKind::MilestoneBurn { .. } | TxKind::CharitableDonation { .. } | TxKind::SubmitJob { .. } | TxKind::SubmitJobV2 { .. }
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
            TxKind::SubmitJobV2 { comme_budget, .. } => *comme_budget,
            TxKind::Batch { operations } => {
                let mut total = Amount::ZERO;
                for op in operations {
                    match op {
                        TxKind::BurstCompute { burn_amount, .. }
                        | TxKind::MilestoneBurn { burn_amount, .. }
                        | TxKind::CharitableDonation { burn_amount, .. } => {
                            total = total.checked_add(*burn_amount).unwrap_or(total);
                        }
                        TxKind::SubmitJob { comme_budget, .. }
                        | TxKind::SubmitJobV2 { comme_budget, .. } => {
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

    /// W5.7 F-1: Maximum number of signers in a MultiSig tx.
    /// Picked so a worst-case Batch of 10 MultiSigs fits inside the 64 KiB
    /// body limit on /tx: 16 * (32 pubkey + 64 sig) = 1536 bytes payload,
    /// plus borsh framing < 2 KiB → 10 × 2 KiB = 20 KiB. Comfortable margin.
    pub const MAX_MULTISIG_SIGNERS: usize = 16;

    /// W5.7 F-1: Validate the structural invariants of this transaction
    /// BEFORE any expensive cryptographic work. Cheap, allocation-free,
    /// safe to call from RPC entry, mempool admission, or block apply.
    ///
    /// Rejects:
    ///   - public_key not 32 bytes / signature not 64 bytes
    ///   - memo > MAX_MEMO_LENGTH
    ///   - Batch.operations.len() > MAX_BATCH_SIZE
    ///   - any element of Batch.operations that is itself a Batch
    ///     (nested Batch is banned outright; clients must flatten)
    ///   - MultiSig with threshold == 0, threshold > signers.len(),
    ///     signers.len() > MAX_MULTISIG_SIGNERS, or
    ///     signatures.len() != threshold
    ///   - MultiSig with any signer != 32 bytes or any signature != 64 bytes
    ///   - KeyRotation with new_public_key.len() != 32
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if self.public_key.len() != 32 {
            return Err("public_key must be 32 bytes");
        }
        if self.signature.len() != 64 {
            return Err("signature must be 64 bytes");
        }
        if let Some(ref memo) = self.memo {
            if memo.len() > Self::MAX_MEMO_LENGTH {
                return Err("memo exceeds MAX_MEMO_LENGTH");
            }
        }
        Self::validate_kind_shape(&self.kind)
    }

    /// Inner helper: structural check on a TxKind. Used by validate_shape
    /// at the outer level and by Batch elements at depth 1 (depth 1 is
    /// the only legal depth — nested Batch is rejected).
    fn validate_kind_shape(kind: &TxKind) -> Result<(), &'static str> {
        match kind {
            TxKind::Batch { operations } => {
                if operations.len() > Self::MAX_BATCH_SIZE {
                    return Err("batch exceeds MAX_BATCH_SIZE");
                }
                for op in operations {
                    if matches!(op, TxKind::Batch { .. }) {
                        return Err("nested Batch is not allowed");
                    }
                    Self::validate_kind_shape(op)?;
                }
            }
            TxKind::MultiSig { threshold, signers, signatures } => {
                if *threshold == 0 {
                    return Err("multisig threshold must be > 0");
                }
                if signers.len() > Self::MAX_MULTISIG_SIGNERS {
                    return Err("multisig signers exceeds MAX_MULTISIG_SIGNERS");
                }
                if (*threshold as usize) > signers.len() {
                    return Err("multisig threshold > signers.len()");
                }
                if signatures.len() != *threshold as usize {
                    return Err("multisig signatures.len() != threshold");
                }
                for s in signers.iter() {
                    if s.len() != 32 {
                        return Err("multisig signer must be 32 bytes");
                    }
                }
                for s in signatures.iter() {
                    if s.len() != 64 {
                        return Err("multisig signature must be 64 bytes");
                    }
                }
            }
            TxKind::KeyRotation { new_public_key } => {
                if new_public_key.len() != 32 {
                    return Err("new_public_key must be 32 bytes");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimally-valid Transaction shell for shape testing.
    /// Signature is wrong on purpose — validate_shape() does NOT verify
    /// signatures, only structural invariants.
    fn shell_tx(kind: TxKind) -> Transaction {
        Transaction {
            from: Address([0u8; 32]),
            nonce: 0,
            kind,
            fee: 100_000,
            signature: vec![0u8; 64],
            public_key: vec![0u8; 32],
            memo: None,
            timelock: None,
        }
    }

    #[test]
    fn validate_shape_accepts_simple_transfer() {
        let tx = shell_tx(TxKind::Transfer {
            to: Address([1u8; 32]),
            amount: Amount::from_comme(1),
        });
        assert!(tx.validate_shape().is_ok());
    }

    #[test]
    fn validate_shape_accepts_bond_family() {
        // PoUW P2 / G4: Bond/RequestUnbond carry a fixed-size Amount; WithdrawUnbonded is a unit
        // variant. All three pass shape validation via the catch-all (no dedicated arm needed).
        assert!(shell_tx(TxKind::Bond { amount: Amount::from_comme(1) }).validate_shape().is_ok());
        assert!(shell_tx(TxKind::RequestUnbond { amount: Amount::from_comme(1) }).validate_shape().is_ok());
        assert!(shell_tx(TxKind::WithdrawUnbonded).validate_shape().is_ok());
    }

    #[test]
    fn validate_shape_rejects_oversized_batch() {
        let inner = TxKind::Transfer { to: Address([1u8; 32]), amount: Amount::from_comme(1) };
        let ops: Vec<TxKind> = (0..(Transaction::MAX_BATCH_SIZE + 1))
            .map(|_| inner.clone()).collect();
        let tx = shell_tx(TxKind::Batch { operations: ops });
        assert_eq!(tx.validate_shape(), Err("batch exceeds MAX_BATCH_SIZE"));
    }

    #[test]
    fn validate_shape_accepts_max_size_batch() {
        let inner = TxKind::Transfer { to: Address([1u8; 32]), amount: Amount::from_comme(1) };
        let ops: Vec<TxKind> = (0..Transaction::MAX_BATCH_SIZE).map(|_| inner.clone()).collect();
        let tx = shell_tx(TxKind::Batch { operations: ops });
        assert!(tx.validate_shape().is_ok());
    }

    #[test]
    fn validate_shape_rejects_nested_batch() {
        let inner = TxKind::Batch { operations: vec![] };
        let outer = TxKind::Batch { operations: vec![inner] };
        let tx = shell_tx(outer);
        assert_eq!(tx.validate_shape(), Err("nested Batch is not allowed"));
    }

    #[test]
    fn validate_shape_rejects_oversized_multisig_signers() {
        let signers = vec![vec![0u8; 32]; Transaction::MAX_MULTISIG_SIGNERS + 1];
        let signatures = vec![vec![0u8; 64]; 1];
        let tx = shell_tx(TxKind::MultiSig { threshold: 1, signers, signatures });
        assert_eq!(tx.validate_shape(),
                   Err("multisig signers exceeds MAX_MULTISIG_SIGNERS"));
    }

    #[test]
    fn validate_shape_rejects_zero_threshold() {
        let tx = shell_tx(TxKind::MultiSig {
            threshold: 0,
            signers: vec![vec![0u8; 32]],
            signatures: vec![],
        });
        assert_eq!(tx.validate_shape(), Err("multisig threshold must be > 0"));
    }

    #[test]
    fn validate_shape_rejects_threshold_exceeds_signers() {
        let tx = shell_tx(TxKind::MultiSig {
            threshold: 5,
            signers: vec![vec![0u8; 32]; 2],
            signatures: vec![vec![0u8; 64]; 5],
        });
        assert_eq!(tx.validate_shape(), Err("multisig threshold > signers.len()"));
    }

    #[test]
    fn validate_shape_rejects_signature_count_mismatch() {
        let tx = shell_tx(TxKind::MultiSig {
            threshold: 2,
            signers: vec![vec![0u8; 32]; 3],
            signatures: vec![vec![0u8; 64]; 1],
        });
        assert_eq!(tx.validate_shape(), Err("multisig signatures.len() != threshold"));
    }

    #[test]
    fn validate_shape_rejects_bad_multisig_signer_len() {
        let tx = shell_tx(TxKind::MultiSig {
            threshold: 1,
            signers: vec![vec![0u8; 31]],
            signatures: vec![vec![0u8; 64]],
        });
        assert_eq!(tx.validate_shape(), Err("multisig signer must be 32 bytes"));
    }

    #[test]
    fn validate_shape_rejects_bad_keyrotation_pubkey_len() {
        let tx = shell_tx(TxKind::KeyRotation { new_public_key: vec![0u8; 33] });
        assert_eq!(tx.validate_shape(), Err("new_public_key must be 32 bytes"));
    }

    #[test]
    fn validate_shape_rejects_oversized_memo() {
        let mut tx = shell_tx(TxKind::Transfer {
            to: Address([1u8; 32]),
            amount: Amount::from_comme(1),
        });
        tx.memo = Some(vec![0u8; Transaction::MAX_MEMO_LENGTH + 1]);
        assert_eq!(tx.validate_shape(), Err("memo exceeds MAX_MEMO_LENGTH"));
    }
}
