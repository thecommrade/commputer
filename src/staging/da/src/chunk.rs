//! Deterministic chunking (spec §5.1): the ONLY place padding / data_len logic lives.
//! Shard order is data [0..N) then parity [N..2N); little-endian everywhere.
use crate::params::ChunkingParams;

/// Split raw bytes into N fixed-size data chunks (last right-zero-padded). Returns
/// (chunks, n_data, data_len). Empty input -> one zero chunk (documented sentinel,
/// spec §5.1). Exact multiples add NO spurious padding chunk.
pub fn split_data_chunks(bytes: &[u8], p: &ChunkingParams) -> (Vec<Vec<u8>>, u16, u64) {
    let cs = p.chunk_size as usize;
    let data_len = bytes.len() as u64;
    let n_data = if bytes.is_empty() { 1 } else { bytes.len().div_ceil(cs) };
    let mut chunks = Vec::with_capacity(n_data);
    for i in 0..n_data {
        let start = i * cs;
        let end = (start + cs).min(bytes.len());
        let mut chunk = vec![0u8; cs];
        if start < bytes.len() {
            chunk[..end - start].copy_from_slice(&bytes[start..end]);
        }
        chunks.push(chunk);
    }
    (chunks, n_data as u16, data_len)
}

/// Rejoin data chunks and truncate to the exact original length.
pub fn join_data_chunks(data_chunks: &[Vec<u8>], data_len: u64) -> Vec<u8> {
    let mut out: Vec<u8> = data_chunks.iter().flatten().copied().collect();
    out.truncate(data_len as usize);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ChunkingParams;
    fn rt(bytes: &[u8]) {
        let p = ChunkingParams { chunk_size: 4, params_version: 1 };
        let (chunks, _n_data, data_len) = split_data_chunks(bytes, &p);
        assert_eq!(data_len, bytes.len() as u64);
        let back = join_data_chunks(&chunks, data_len);
        assert_eq!(back, bytes, "chunk round-trip must be exact");
    }
    #[test] fn roundtrip_empty() { rt(b""); }
    #[test] fn roundtrip_exact_multiple() { rt(b"AAAABBBB"); } // 8 = 2*4, no pad chunk
    #[test] fn roundtrip_partial() { rt(b"AAAAB"); }           // 5 -> 2 chunks, last padded
    #[test]
    fn last_chunk_zero_padded_and_count_is_ceil() {
        let p = ChunkingParams { chunk_size: 4, params_version: 1 };
        let (chunks, n_data, _) = split_data_chunks(b"AAAAB", &p);
        assert_eq!(n_data, 2);
        assert_eq!(chunks[1], vec![b'B', 0, 0, 0]); // right zero-pad
    }
    #[test]
    fn empty_input_is_single_zero_chunk() {
        let p = ChunkingParams { chunk_size: 4, params_version: 1 };
        let (chunks, n_data, data_len) = split_data_chunks(b"", &p);
        assert_eq!(n_data, 1); assert_eq!(data_len, 0); assert_eq!(chunks[0], vec![0u8; 4]);
    }
}
