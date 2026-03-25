//! Features 201-203: Fuzz-like deserialization tests for Block, Transaction, and ProofChallenge.
//! These feed malformed bytes and verify no panics occur.

#[cfg(test)]
mod tests {
    use crate::block::Block;
    use crate::transaction::Transaction;
    use crate::proof::ProofChallenge;

    // Feature 201: Fuzz target — block deserialization
    #[test]
    fn feature_201_fuzz_block_deserialization() {
        let test_inputs: Vec<&[u8]> = vec![
            // Empty
            b"",
            // Truncated JSON
            b"{",
            b"{\"header\":",
            b"{\"header\":{}}",
            // Invalid JSON
            b"not json at all",
            // Valid JSON but wrong types
            b"{\"height\": \"not a number\"}",
            // Null bytes
            &[0u8; 64],
            // Very large input (but bounded)
            &[0xFFu8; 1024],
            // Random-looking bytes
            &[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03],
            // Almost valid
            b"{\"header\":{\"height\":0},\"transactions\":[]}",
            // Unicode garbage
            b"\xC0\xC1\xF5\xF6\xF7\xF8\xF9\xFA\xFB\xFC\xFD\xFE\xFF",
            // Extremely nested
            b"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[",
            // Oversized number
            b"{\"height\": 99999999999999999999999999999999}",
        ];

        for input in &test_inputs {
            // Should return Err, never panic
            let result = serde_json::from_slice::<Block>(input);
            assert!(
                result.is_err(),
                "Expected Err for malformed input, got Ok"
            );
        }
    }

    // Feature 202: Fuzz target — transaction deserialization
    #[test]
    fn feature_202_fuzz_transaction_deserialization() {
        let test_inputs: Vec<&[u8]> = vec![
            b"",
            b"{",
            b"null",
            b"[]",
            &[0u8; 128],
            &[0xFFu8; 256],
            b"{\"from\":\"invalid\"}",
            b"{\"kind\":\"Transfer\",\"amount\":-1}",
            b"\x00\x01\x02\x03\x04\x05\x06\x07",
            b"{\"from\":[0],\"nonce\":0,\"kind\":{\"Transfer\":{\"to\":[0],\"amount\":0}},\"fee\":0}",
            // Borsh-like random bytes
            &[1, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0],
            // Truncated borsh
            &[0u8; 3],
        ];

        for input in &test_inputs {
            // JSON deserialization should not panic
            let _ = serde_json::from_slice::<Transaction>(input);
        }

        // Also test borsh deserialization
        for input in &test_inputs {
            let _ = borsh::from_slice::<Transaction>(input);
        }
    }

    // Feature 203: Fuzz target — proof challenge parsing
    #[test]
    fn feature_203_fuzz_proof_challenge_deserialization() {
        let test_inputs: Vec<&[u8]> = vec![
            b"",
            b"{}",
            b"null",
            &[0u8; 64],
            &[0xFFu8; 128],
            b"{\"channel\":\"InvalidChannel\"}",
            b"{\"challenge_id\":[]}",
            b"\xDE\xAD\xBE\xEF",
            // Partially valid
            b"{\"channel\":\"Processing\",\"challenge_id\":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}",
            // Borsh random
            &[0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8],
        ];

        for input in &test_inputs {
            // Should not panic
            let _ = serde_json::from_slice::<ProofChallenge>(input);
        }

        for input in &test_inputs {
            let _ = borsh::from_slice::<ProofChallenge>(input);
        }
    }
}
