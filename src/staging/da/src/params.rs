//! Consensus-critical params + types (spec §5/§6.1). Every value here that feeds
//! da_root or the sampling seed is versioned by `params_version`; changing it is a
//! coordinated protocol change. New crate; no existing-file changes beyond the
//! workspace member line.

/// One mega-... no: byte size of a chunk. Under libp2p Bitswap's 2 MiB block cap;
/// small programs are 1-2 chunks.
pub const DEFAULT_CHUNK_SIZE: u32 = 65_536; // 64 KiB
/// Per-verifier random samples; per-verifier false-accept <= (1/2)^16 ~= 1.5e-5 (spec §3).
pub const SAMPLES_PER_VERIFIER: usize = 16;
/// Replication / responsible-provider set size (IPFS convention).
pub const REPLICATION_FACTOR_K: usize = 20;

/// Merkle domain-separation tags (RFC-6962 style): leaf vs internal (spec §5.3).
pub const DOMAIN_LEAF: u8 = 0x00;
pub const DOMAIN_NODE: u8 = 0x01;
/// Sampling-seed domain tag (spec §5.4).
pub const DOMAIN_SAMPLING: &[u8] = b"commputer-da-sampling-v1";

/// Versioned chunking policy. `chunk_size` + `params_version` hash into the attestation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkingParams {
    pub chunk_size: u32,
    pub params_version: u16,
}
impl Default for ChunkingParams {
    fn default() -> Self {
        Self { chunk_size: DEFAULT_CHUNK_SIZE, params_version: 1 }
    }
}

/// Binds the immutable program identity (sha256 of raw bytes) to the DA commitment
/// (Merkle root over the coded chunks). `n_total == 2 * n_data` at rate 1/2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaAttestation {
    pub program_id: [u8; 32], // sha256(raw bytes) — the verification-game identity (unchanged)
    pub da_root: [u8; 32],    // Merkle root over the 2N coded chunks
    pub data_len: u64,
    pub chunk_size: u32,
    pub n_data: u16,          // N
    pub n_total: u16,         // 2N
    pub params_version: u16,
}

/// Index of a chunk within the coded set [0, n_total).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkIndex(pub u16);

/// A network peer identity (content-addressed; XOR distance defines responsibility).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(pub [u8; 32]);

/// All the ways a DA operation can fail (local, never hashed into consensus).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaError {
    TooLarge { n_data: usize },     // > 128 data chunks: exceeds GF(2^8) rate-1/2 ceiling (256 coded)
    BadAttestation(&'static str),   // structural mismatch
    Reconstruct,                    // RS could not rebuild from the present chunks
}
