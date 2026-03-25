use thiserror::Error;
use commputer_core::block::Block;
use commputer_core::transaction::Transaction;
use crate::message::NetworkMessage;

/// Maximum allowed message size in bytes (1 MB).
pub const MAX_MESSAGE_SIZE: usize = 1_048_576;

/// Maximum block message size in bytes (2 MB — blocks can be larger).
pub const MAX_BLOCK_MESSAGE_SIZE: usize = 2_097_152;

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("message exceeds maximum size of {max} bytes (got {actual})")]
    TooLarge { max: usize, actual: usize },
    #[error("failed to deserialize message: {0}")]
    DeserializationFailed(String),
    #[error("block has no transactions and is not genesis")]
    EmptyBlock,
}

/// Validate that raw bytes can be deserialized as a Block and meet size limits.
pub fn validate_block_message(data: &[u8]) -> Result<(), ValidationError> {
    if data.len() > MAX_BLOCK_MESSAGE_SIZE {
        return Err(ValidationError::TooLarge {
            max: MAX_BLOCK_MESSAGE_SIZE,
            actual: data.len(),
        });
    }
    let _block: Block = serde_json::from_slice(data)
        .map_err(|e| ValidationError::DeserializationFailed(e.to_string()))?;
    Ok(())
}

/// Validate that raw bytes can be deserialized as a Transaction and meet size limits.
pub fn validate_tx_message(data: &[u8]) -> Result<(), ValidationError> {
    if data.len() > MAX_MESSAGE_SIZE {
        return Err(ValidationError::TooLarge {
            max: MAX_MESSAGE_SIZE,
            actual: data.len(),
        });
    }
    let _tx: Transaction = serde_json::from_slice(data)
        .map_err(|e| ValidationError::DeserializationFailed(e.to_string()))?;
    Ok(())
}

/// Validate that raw bytes can be deserialized as a consensus (NetworkMessage) and meet size limits.
pub fn validate_consensus_message(data: &[u8]) -> Result<(), ValidationError> {
    if data.len() > MAX_MESSAGE_SIZE {
        return Err(ValidationError::TooLarge {
            max: MAX_MESSAGE_SIZE,
            actual: data.len(),
        });
    }
    let _msg: NetworkMessage = serde_json::from_slice(data)
        .map_err(|e| ValidationError::DeserializationFailed(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_oversized_block() {
        let data = vec![0u8; MAX_BLOCK_MESSAGE_SIZE + 1];
        let err = validate_block_message(&data).unwrap_err();
        assert!(matches!(err, ValidationError::TooLarge { .. }));
    }

    #[test]
    fn reject_oversized_tx() {
        let data = vec![0u8; MAX_MESSAGE_SIZE + 1];
        let err = validate_tx_message(&data).unwrap_err();
        assert!(matches!(err, ValidationError::TooLarge { .. }));
    }

    #[test]
    fn reject_invalid_json_block() {
        let data = b"not valid json";
        let err = validate_block_message(data).unwrap_err();
        assert!(matches!(err, ValidationError::DeserializationFailed(_)));
    }

    #[test]
    fn reject_invalid_json_tx() {
        let data = b"{\"garbage\": true}";
        let err = validate_tx_message(data).unwrap_err();
        assert!(matches!(err, ValidationError::DeserializationFailed(_)));
    }

    #[test]
    fn reject_invalid_consensus_message() {
        let data = b"not a consensus message";
        let err = validate_consensus_message(data).unwrap_err();
        assert!(matches!(err, ValidationError::DeserializationFailed(_)));
    }

    #[test]
    fn reject_oversized_consensus() {
        let data = vec![0u8; MAX_MESSAGE_SIZE + 1];
        let err = validate_consensus_message(&data).unwrap_err();
        assert!(matches!(err, ValidationError::TooLarge { .. }));
    }
}
