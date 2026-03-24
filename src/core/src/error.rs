use thiserror::Error;

#[derive(Error, Debug)]
pub enum CommpError {
    #[error("invalid block: {0}")]
    InvalidBlock(String),

    #[error("invalid transaction: {0}")]
    InvalidTransaction(String),

    #[error("invalid proof: {0}")]
    InvalidProof(String),

    #[error("compliance violation: {0}")]
    ComplianceViolation(String),

    #[error("insufficient balance: have {have}, need {need}")]
    InsufficientBalance { have: u64, need: u64 },

    #[error("unknown validator: {0}")]
    UnknownValidator(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("storage error: {0}")]
    Storage(String),
}
