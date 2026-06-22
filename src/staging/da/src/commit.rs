//! Build a DaAttestation from raw bytes, and verify a single fetched chunk against it.
use crate::chunk::split_data_chunks;
use crate::code::{ErasureCoder, Rs8Coder};
use crate::merkle::{build_root, prove, verify};
use crate::params::{ChunkingParams, DaAttestation, DaError};
use sha2::{Digest, Sha256};

/// Encode raw bytes into the 2N coded chunks and the attestation. The executor/
/// publisher runs this; every node that re-encodes the same bytes gets the same da_root.
pub fn build_attestation(
    bytes: &[u8],
    p: &ChunkingParams,
) -> Result<(DaAttestation, Vec<Vec<u8>>), DaError> {
    let (data, n_data, data_len) = split_data_chunks(bytes, p);
    let parity = Rs8Coder.encode_parity(&data)?;
    let coded: Vec<Vec<u8>> = data.into_iter().chain(parity).collect(); // [0..N) data, [N..2N) parity
    let da_root = build_root(&coded);
    let program_id: [u8; 32] = Sha256::digest(bytes).into();
    let att = DaAttestation {
        program_id,
        da_root,
        data_len,
        chunk_size: p.chunk_size,
        n_data,
        n_total: 2 * n_data,
        params_version: p.params_version,
    };
    Ok((att, coded))
}

/// Verify a fetched chunk belongs to the committed set.
pub fn verify_chunk(att: &DaAttestation, index: u16, chunk: &[u8], path: &[Option<[u8; 32]>]) -> bool {
    (index as usize) < att.n_total as usize && verify(&att.da_root, index as usize, chunk, path)
}

/// Build the inclusion path for a chunk (publisher side helper / test helper).
pub fn chunk_proof(coded: &[Vec<u8>], index: u16) -> Vec<Option<[u8; 32]>> {
    prove(coded, index as usize)
}
