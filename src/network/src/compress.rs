//! Feature 173: Message compression using flate2 (deflate).
//!
//! Compress gossipsub messages before broadcast, decompress on receipt.
//! Uses a 1-byte prefix: 0x00 = uncompressed, 0x01 = deflate-compressed.

use flate2::read::{DeflateDecoder, DeflateEncoder};
use flate2::Compression;
use std::io::Read;

const PREFIX_RAW: u8 = 0x00;
const PREFIX_DEFLATE: u8 = 0x01;

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
            let mut decoder = DeflateDecoder::new(&data[1..]);
            let mut decompressed = Vec::new();
            if decoder.read_to_end(&mut decompressed).is_ok() {
                decompressed
            } else {
                // Fallback: return without prefix.
                data[1..].to_vec()
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
}
