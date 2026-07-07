//! Feature 173: Message compression using flate2 (deflate).
//!
//! Compress gossipsub messages before broadcast, decompress on receipt.
//! Uses a 1-byte prefix: 0x00 = uncompressed, 0x01 = deflate-compressed.

use flate2::read::{DeflateDecoder, DeflateEncoder};
use flate2::Compression;
use std::io::Read;

const PREFIX_RAW: u8 = 0x00;
const PREFIX_DEFLATE: u8 = 0x01;

/// Hard cap on the number of bytes `decompress` will ever produce, aligned with
/// the network's largest accepted message (`validation::MAX_BLOCK_MESSAGE_SIZE`,
/// ~2 MiB). deflate amplifies ~1000:1, so an uncapped decoder run on every
/// inbound gossip message is a remote-OOM vector: `take` bounds the allocation
/// regardless of how small the compressed input is. Anything larger than the
/// biggest legitimate message is a bomb and is dropped.
const MAX_DECOMPRESSED_BYTES: usize = crate::validation::MAX_BLOCK_MESSAGE_SIZE;

/// Compress data using deflate. Adds a 1-byte prefix to indicate compression.
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(data, Compression::fast());
    let mut compressed = Vec::new();
    if encoder.read_to_end(&mut compressed).is_ok() && compressed.len() < data.len() {
        let mut result = Vec::with_capacity(1 + compressed.len());
        result.push(PREFIX_DEFLATE);
        result.extend_from_slice(&compressed);
        result
    } else {
        let mut result = Vec::with_capacity(1 + data.len());
        result.push(PREFIX_RAW);
        result.extend_from_slice(data);
        result
    }
}

/// Decompress data. Checks the 1-byte prefix to determine format.
/// If no prefix is recognized (legacy data), attempts raw deserialization.
pub fn decompress(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return data.to_vec();
    }
    match data[0] {
        PREFIX_RAW => data[1..].to_vec(),
        PREFIX_DEFLATE => {
            // Bound the decoder to MAX+1 bytes: `take` guarantees at most that
            // many bytes are ever read (and thus allocated), so a decompression
            // bomb cannot exhaust memory. If the output would exceed the cap we
            // drop the message rather than forward a truncated/oversized payload.
            let decoder = DeflateDecoder::new(&data[1..]);
            let mut decompressed = Vec::new();
            match decoder
                .take(MAX_DECOMPRESSED_BYTES as u64 + 1)
                .read_to_end(&mut decompressed)
            {
                Ok(_) if decompressed.len() <= MAX_DECOMPRESSED_BYTES => decompressed,
                Ok(_) => {
                    // Output exceeded the cap — treat as a decompression bomb and drop.
                    tracing::warn!(
                        compressed_len = data.len(),
                        cap = MAX_DECOMPRESSED_BYTES,
                        "decompress: output exceeded cap, dropping possible decompression bomb"
                    );
                    Vec::new()
                }
                Err(_) => {
                    // Corrupt deflate stream — fall back to the prefix-stripped bytes.
                    data[1..].to_vec()
                }
            }
        }
        _ => {
            // Legacy unframed data — return as-is.
            data.to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_compression() {
        let original = b"hello world hello world hello world hello world";
        let compressed = compress(original);
        let decompressed = decompress(&compressed);
        assert_eq!(&decompressed, original);
    }

    #[test]
    fn small_data_passthrough() {
        // Very small data may not compress well — still roundtrips correctly.
        let original = b"hi";
        let compressed = compress(original);
        let decompressed = decompress(&compressed);
        assert_eq!(&decompressed, original);
    }

    #[test]
    fn empty_data() {
        let original = b"";
        let compressed = compress(original);
        let decompressed = decompress(&compressed);
        assert_eq!(&decompressed, original);
    }

    #[test]
    fn legacy_data_passthrough() {
        // Data without our prefix byte (e.g., raw JSON starting with '{')
        let legacy = b"{\"key\":\"value\"}";
        let result = decompress(legacy);
        assert_eq!(&result, legacy);
    }

    #[test]
    fn decompression_bomb_rejected() {
        // A highly-repetitive payload compresses tiny but expands past the cap.
        // The decoder must stop at the bound and the message must be dropped.
        let bomb_plain = vec![0u8; MAX_DECOMPRESSED_BYTES + 4096];
        let compressed = compress(&bomb_plain);
        assert_eq!(compressed[0], PREFIX_DEFLATE, "bomb must take the deflate path");
        // The compressed form is a small fraction of the payload — that's the
        // amplification that makes an uncapped decoder a remote-OOM vector.
        assert!(
            compressed.len() < bomb_plain.len() / 10,
            "a zero-filled bomb should compress far smaller than the cap, got {} bytes",
            compressed.len()
        );

        let out = decompress(&compressed);
        assert!(out.is_empty(), "bomb must be dropped, got {} bytes", out.len());
    }

    #[test]
    fn repetitive_payload_under_cap_roundtrips() {
        // Compresses well (repetitive) but stays well under the cap.
        let msg = b"commputer-gossip-".repeat(1000);
        let compressed = compress(&msg);
        assert_eq!(compressed[0], PREFIX_DEFLATE);
        let out = decompress(&compressed);
        assert_eq!(out, msg);
    }

    #[test]
    fn payload_exactly_at_cap_roundtrips() {
        // A legitimate message decompressing to exactly the cap is accepted.
        let msg = vec![7u8; MAX_DECOMPRESSED_BYTES];
        let compressed = compress(&msg);
        let out = decompress(&compressed);
        assert_eq!(out.len(), MAX_DECOMPRESSED_BYTES);
        assert_eq!(out, msg);
    }
}
