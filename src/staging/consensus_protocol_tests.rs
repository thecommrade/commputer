// consensus_protocol_tests.rs — Tests for src/network/src/consensus_protocol.rs
//
// WHAT IT DOES:
//   Round-trip serialization tests for ConsensusRequest and ConsensusResponse,
//   including edge cases (large payloads, empty bytes, all fields preserved).
//
// WHERE IT SHOULD GO:
//   Paste into src/network/src/consensus_protocol.rs under #[cfg(test)] mod tests,
//   or compile as a standalone integration test.
//
// WIRING REQUIRED:
//   None — these tests use serde_json directly without the async codec.

#[cfg(test)]
mod consensus_protocol_tests {
    use commputer_network::consensus_protocol::{ConsensusRequest, ConsensusResponse};

    fn round_trip_request(req: &ConsensusRequest) -> ConsensusRequest {
        let json = serde_json::to_vec(req).expect("serialize request");
        serde_json::from_slice(&json).expect("deserialize request")
    }

    fn round_trip_response(resp: &ConsensusResponse) -> ConsensusResponse {
        let json = serde_json::to_vec(resp).expect("serialize response");
        serde_json::from_slice(&json).expect("deserialize response")
    }

    fn sample_hash(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    // -----------------------------------------------------------------------
    // Task 4a: Round-trip BlockProposal
    // -----------------------------------------------------------------------
    #[test]
    fn round_trip_block_proposal() {
        let req = ConsensusRequest::BlockProposal {
            block_bytes: vec![1, 2, 3, 4, 5],
            height: 42,
        };
        let rt = round_trip_request(&req);
        match rt {
            ConsensusRequest::BlockProposal { block_bytes, height } => {
                assert_eq!(block_bytes, vec![1, 2, 3, 4, 5]);
                assert_eq!(height, 42);
            }
            _ => panic!("wrong variant after round-trip"),
        }
    }

    // -----------------------------------------------------------------------
    // Task 4b: Round-trip Vote
    // -----------------------------------------------------------------------
    #[test]
    fn round_trip_vote() {
        let resp = ConsensusResponse::Vote {
            height: 100,
            preference: sample_hash(0xAB),
            accept: true,
        };
        let rt = round_trip_response(&resp);
        match rt {
            ConsensusResponse::Vote { height, preference, accept } => {
                assert_eq!(height, 100);
                assert_eq!(preference, sample_hash(0xAB));
                assert!(accept);
            }
            _ => panic!("wrong variant after round-trip"),
        }
    }

    #[test]
    fn round_trip_vote_reject() {
        let resp = ConsensusResponse::Vote {
            height: 999,
            preference: sample_hash(0xFF),
            accept: false,
        };
        let rt = round_trip_response(&resp);
        match rt {
            ConsensusResponse::Vote { accept, .. } => assert!(!accept),
            _ => panic!("wrong variant"),
        }
    }

    // -----------------------------------------------------------------------
    // Task 4c: Round-trip NotReady
    // -----------------------------------------------------------------------
    #[test]
    fn round_trip_not_ready() {
        let resp = ConsensusResponse::NotReady { height: 77 };
        let rt = round_trip_response(&resp);
        match rt {
            ConsensusResponse::NotReady { height } => assert_eq!(height, 77),
            _ => panic!("wrong variant after round-trip"),
        }
    }

    // -----------------------------------------------------------------------
    // Task 4d: Round-trip VoteRequest
    // -----------------------------------------------------------------------
    #[test]
    fn round_trip_vote_request() {
        let req = ConsensusRequest::VoteRequest {
            height: 55,
            block_hash: sample_hash(0x12),
        };
        let rt = round_trip_request(&req);
        match rt {
            ConsensusRequest::VoteRequest { height, block_hash } => {
                assert_eq!(height, 55);
                assert_eq!(block_hash, sample_hash(0x12));
            }
            _ => panic!("wrong variant after round-trip"),
        }
    }

    // -----------------------------------------------------------------------
    // Task 4e: Large block_bytes (1 MB)
    // -----------------------------------------------------------------------
    #[test]
    fn large_block_bytes_round_trip() {
        let large = vec![0xABu8; 1_024 * 1_024]; // 1 MB
        let req = ConsensusRequest::BlockProposal {
            block_bytes: large.clone(),
            height: 1,
        };
        let rt = round_trip_request(&req);
        match rt {
            ConsensusRequest::BlockProposal { block_bytes, height } => {
                assert_eq!(block_bytes.len(), 1_024 * 1_024);
                assert_eq!(block_bytes, large);
                assert_eq!(height, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    // -----------------------------------------------------------------------
    // Task 4f: Empty block_bytes
    // -----------------------------------------------------------------------
    #[test]
    fn empty_block_bytes_round_trip() {
        let req = ConsensusRequest::BlockProposal {
            block_bytes: vec![],
            height: 0,
        };
        let rt = round_trip_request(&req);
        match rt {
            ConsensusRequest::BlockProposal { block_bytes, height } => {
                assert!(block_bytes.is_empty(), "empty bytes should survive round-trip");
                assert_eq!(height, 0);
            }
            _ => panic!("wrong variant"),
        }
    }

    // -----------------------------------------------------------------------
    // Task 4g: All fields preserved — max height, all-ones hash
    // -----------------------------------------------------------------------
    #[test]
    fn all_fields_preserved_max_values() {
        let req = ConsensusRequest::VoteRequest {
            height: u64::MAX,
            block_hash: [0xFF; 32],
        };
        let rt = round_trip_request(&req);
        match rt {
            ConsensusRequest::VoteRequest { height, block_hash } => {
                assert_eq!(height, u64::MAX);
                assert_eq!(block_hash, [0xFF; 32]);
            }
            _ => panic!("wrong variant"),
        }

        let resp = ConsensusResponse::Vote {
            height: u64::MAX,
            preference: [0xFF; 32],
            accept: true,
        };
        let rt = round_trip_response(&resp);
        match rt {
            ConsensusResponse::Vote { height, preference, accept } => {
                assert_eq!(height, u64::MAX);
                assert_eq!(preference, [0xFF; 32]);
                assert!(accept);
            }
            _ => panic!("wrong variant"),
        }
    }

    // -----------------------------------------------------------------------
    // Additional: invalid JSON is rejected gracefully
    // -----------------------------------------------------------------------
    #[test]
    fn invalid_json_returns_error() {
        let result: Result<ConsensusRequest, _> = serde_json::from_slice(b"not json {");
        assert!(result.is_err(), "invalid JSON should fail to deserialize");
    }

    #[test]
    fn wrong_variant_name_rejected() {
        let result: Result<ConsensusResponse, _> =
            serde_json::from_slice(br#"{"UnknownVariant": {"height": 1}}"#);
        assert!(result.is_err(), "unknown variant should fail");
    }
}
