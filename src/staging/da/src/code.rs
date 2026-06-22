//! Erasure coding (spec §5.2): systematic rate-1/2 RS. The encoder output is NOT a
//! consensus artifact (da_root + the sha256 re-bind are), but it MUST be deterministic
//! so independent encoders agree on da_root. v1 uses pure-Rust GF(2^8); GF(2^16) is a
//! deferred feature. Behind a trait so the coder is swappable.
use crate::params::DaError;
use reed_solomon_erasure::galois_8::ReedSolomon;

pub trait ErasureCoder {
    /// N data chunks (equal length) -> N parity chunks. Err(TooLarge) if N > 128.
    fn encode_parity(&self, data: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, DaError>;
    /// Any N present of the 2N coded chunks -> all 2N (fills None). Err(Reconstruct) if < N present.
    fn reconstruct(&self, present: &[Option<Vec<u8>>]) -> Result<Vec<Vec<u8>>, DaError>;
}

pub struct Rs8Coder;

impl ErasureCoder for Rs8Coder {
    fn encode_parity(&self, data: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, DaError> {
        let n = data.len();
        if n == 0 || n > 128 {
            return Err(DaError::TooLarge { n_data: n });
        }
        let rs = ReedSolomon::new(n, n).map_err(|_| DaError::TooLarge { n_data: n })?;
        let mut shards: Vec<Vec<u8>> = data.to_vec();
        shards.extend((0..n).map(|_| vec![0u8; data[0].len()]));
        rs.encode(&mut shards).map_err(|_| DaError::Reconstruct)?;
        Ok(shards.split_off(n))
    }

    fn reconstruct(&self, present: &[Option<Vec<u8>>]) -> Result<Vec<Vec<u8>>, DaError> {
        let two_n = present.len();
        if two_n == 0 || two_n % 2 != 0 {
            return Err(DaError::BadAttestation("coded count must be even and > 0"));
        }
        let n = two_n / 2;
        let rs = ReedSolomon::new(n, n).map_err(|_| DaError::TooLarge { n_data: n })?;
        let mut shards: Vec<Option<Vec<u8>>> = present.to_vec();
        rs.reconstruct(&mut shards).map_err(|_| DaError::Reconstruct)?;
        shards.into_iter().map(|o| o.ok_or(DaError::Reconstruct)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parity_then_reconstruct_from_any_n() {
        let coder = Rs8Coder;
        let data: Vec<Vec<u8>> = vec![vec![1u8; 8], vec![2; 8], vec![3; 8]];
        let parity = coder.encode_parity(&data).unwrap(); // N=3 parity
        assert_eq!(parity.len(), 3);
        // determinism
        assert_eq!(parity, coder.encode_parity(&data).unwrap());
        // 2N = 6 coded; keep any 3 (drop 0,2,4), reconstruct all 6
        let mut present: Vec<Option<Vec<u8>>> =
            data.iter().chain(parity.iter()).cloned().map(Some).collect();
        present[0] = None; present[2] = None; present[4] = None;
        let full = coder.reconstruct(&present).unwrap();
        let expected: Vec<Vec<u8>> = data.iter().chain(parity.iter()).cloned().collect();
        assert_eq!(full, expected);
    }
    #[test]
    fn too_large_is_rejected() {
        let coder = Rs8Coder;
        let data: Vec<Vec<u8>> = vec![vec![0u8; 1]; 129]; // 129 data > 128 ceiling (would need 258 shards)
        assert_eq!(coder.encode_parity(&data), Err(DaError::TooLarge { n_data: 129 }));
    }
}
