//! Checked-in determinism vectors for the consensus-touching surfaces (spec §5/§8).
//! Each layer grows this file. A change to any pinned value fails CI loudly.
use commputer_da::params::{ChunkingParams, DaAttestation, SAMPLES_PER_VERIFIER, DEFAULT_CHUNK_SIZE};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn params_defaults_are_pinned() {
    assert_eq!(SAMPLES_PER_VERIFIER, 16);
    assert_eq!(DEFAULT_CHUNK_SIZE, 65_536);
    let p = ChunkingParams::default();
    assert_eq!(p.chunk_size, 65_536);
    assert_eq!(p.params_version, 1);
}

#[test]
fn chunking_and_coding_vector() {
    use commputer_da::chunk::split_data_chunks;
    use commputer_da::code::{ErasureCoder, Rs8Coder};
    let p = ChunkingParams { chunk_size: 4, params_version: 1 };
    let (data, n_data, data_len) = split_data_chunks(b"hello world!", &p); // 12 bytes -> 3 chunks
    assert_eq!(n_data, 3);
    assert_eq!(data_len, 12);
    let parity = Rs8Coder.encode_parity(&data).unwrap();
    // pin the parity bytes so an RS dep bump that changes the field/generator fails CI
    let parity_flat: Vec<u8> = parity.iter().flatten().copied().collect();
    assert_eq!(parity_flat.len(), 12);
    // GOLDEN: pins GF(2^8) RS output for reed-solomon-erasure =6.0.0
    assert_eq!(hex(&parity_flat), "75297f220c4184c70b049fc4");
}

#[test]
fn attestation_da_root_vector() {
    use commputer_da::commit::build_attestation;
    let p = ChunkingParams { chunk_size: 4, params_version: 1 };
    let (att, coded) = build_attestation(b"hello world!", &p).unwrap();
    assert_eq!(att.n_data, 3);
    assert_eq!(att.n_total, 6);
    assert_eq!(att.data_len, 12);
    // every coded chunk verifies against da_root
    for i in 0..att.n_total {
        let path = commputer_da::commit::chunk_proof(&coded, i);
        assert!(commputer_da::commit::verify_chunk(&att, i, &coded[i as usize], &path));
    }
    // GOLDEN: pinned da_root for b"hello world!" at chunk_size=4 (reed-solomon-erasure =6.0.0)
    assert_eq!(hex(&att.da_root), "630072426b03900cddba4cda861f05c13a8f63ececbf5b83d5d6a9f4d511986c");
}

#[test]
fn sampling_golden_vector() {
    use commputer_da::sampling::sample_indices;
    // Fixed inputs: da_root=[5;32], job_id=[6;32], epoch=42, verifier_id=[7;32], n_total=64
    // GOLDEN: captured from the deterministic CtrHash PRNG + partial Fisher-Yates
    // (sha256(DOMAIN_SAMPLING||da_root||job_id||epoch_le||verifier_id) seed).
    // Changing DOMAIN_SAMPLING, the hash, or the Fisher-Yates walk will break this.
    let indices = sample_indices([5u8; 32], [6u8; 32], 42u64, [7u8; 32], 64usize);
    assert_eq!(indices, vec![41u16, 37, 30, 53, 49, 20, 47, 19, 56, 28, 62, 59, 21, 55, 60, 35]);
}

#[test]
fn attestation_is_constructible_and_plain_data() {
    let att = DaAttestation {
        program_id: [1u8; 32], da_root: [2u8; 32], data_len: 100,
        chunk_size: 65_536, n_data: 1, n_total: 2, params_version: 1,
    };
    assert_eq!(att.n_total, 2 * att.n_data);
}
