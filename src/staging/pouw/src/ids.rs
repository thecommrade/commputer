use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ParticipantId(pub [u8; 32]);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct JobId(pub [u8; 32]);

/// SHA-256 over a sequence of byte slices.
pub fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts { h.update(p); }
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}
pub fn hash2(a: &[u8], b: &[u8]) -> [u8; 32] { hash_parts(&[a, b]) }

impl JobId {
    pub fn derive(spec_hash: &[u8; 32], input_hash: &[u8; 32], submitter: &ParticipantId, nonce: u64) -> JobId {
        JobId(hash_parts(&[spec_hash, input_hash, &submitter.0, &nonce.to_le_bytes()]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn job_id_is_deterministic_and_input_sensitive() {
        let a = JobId::derive(&[1; 32], &[2; 32], &ParticipantId([3; 32]), 0);
        let b = JobId::derive(&[1; 32], &[2; 32], &ParticipantId([3; 32]), 0);
        let c = JobId::derive(&[1; 32], &[2; 32], &ParticipantId([3; 32]), 1);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
    #[test]
    fn hash_helper_is_stable() {
        assert_eq!(hash2(b"x", b"y"), hash2(b"x", b"y"));
        assert_ne!(hash2(b"x", b"y"), hash2(b"y", b"x"));
    }
}
