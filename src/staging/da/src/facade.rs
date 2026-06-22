//! DataAvailability::verify_available — the retry-window->abstain driver (spec §6.7/§7).
//! Returns Available(reconstructed bytes) iff all sampled chunks fetch + Merkle-verify
//! within the window AND RS reconstruction from >= N succeeds AND sha256(recon)==program_id.
//! Otherwise Abstain. The abstain decision lives HERE so callers never branch on it.
//! New file; wired by adding `pub mod facade;` to lib.rs.
use crate::chunk::join_data_chunks;
use crate::code::{ErasureCoder, Rs8Coder};
use crate::commit::verify_chunk;
use crate::params::DaAttestation;
use crate::providers::xor_distance;
use crate::sampling::sample_indices;
use crate::transport::{Clock, DaTransport};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AvailabilityOutcome { Available(Vec<u8>), Abstain }

/// chunk_hash for chunk at `index`: sha256(da_root || index_le) — a stable network key
/// that binds a fetched chunk to its attestation+position (transport addressing only).
pub fn chunk_hash(att: &DaAttestation, index: u16) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(att.da_root);
    h.update(index.to_le_bytes());
    h.finalize().into()
}

pub struct DataAvailability<'a, T: DaTransport, C: Clock> {
    pub transport: &'a T,
    pub clock: &'a C,
    pub retry_window_ticks: u64,
    pub max_attempts_per_chunk: u32,
}

impl<'a, T: DaTransport, C: Clock> DataAvailability<'a, T, C> {
    pub fn verify_available(
        &self, att: &DaAttestation, job_id: [u8; 32], epoch: u64, verifier_id: [u8; 32],
    ) -> AvailabilityOutcome {
        let deadline = self.clock.now_tick() + self.retry_window_ticks;
        let indices = sample_indices(att.da_root, job_id, epoch, verifier_id, att.n_total as usize);

        // collect verified chunks (by coded index) within the window
        let mut have: Vec<Option<Vec<u8>>> = vec![None; att.n_total as usize];
        for &idx in &indices {
            if self.clock.now_tick() > deadline { return AvailabilityOutcome::Abstain; }
            if let Some(bytes) = self.fetch_verified(att, idx, deadline) {
                have[idx as usize] = Some(bytes);
            } else {
                return AvailabilityOutcome::Abstain; // sampled chunk unobtainable -> abstain
            }
        }

        // We sampled s of 2N; to reconstruct we need any N. The sampler proves
        // AVAILABILITY (all s present); reconstruction may need more chunks — fetch the
        // rest opportunistically until we hold >= N (still within the window).
        let n = att.n_data as usize;
        for idx in 0..att.n_total {
            if have.iter().filter(|x| x.is_some()).count() >= n { break; }
            if have[idx as usize].is_none() && self.clock.now_tick() <= deadline {
                if let Some(bytes) = self.fetch_verified(att, idx, deadline) {
                    have[idx as usize] = Some(bytes);
                }
            }
        }
        if have.iter().filter(|x| x.is_some()).count() < n {
            return AvailabilityOutcome::Abstain;
        }

        // reconstruct + re-bind
        let coded = match Rs8Coder.reconstruct(&have) { Ok(c) => c, Err(_) => return AvailabilityOutcome::Abstain };
        let data_chunks: Vec<Vec<u8>> = coded.into_iter().take(n).collect();
        let bytes = join_data_chunks(&data_chunks, att.data_len);
        let recon_id: [u8; 32] = Sha256::digest(&bytes).into();
        if recon_id == att.program_id {
            AvailabilityOutcome::Available(bytes)
        } else {
            AvailabilityOutcome::Abstain // wrong/non-codeword bytes -> abstain (re-bind closes the attack)
        }
    }

    /// Fetch one chunk and Merkle-verify it against da_root; deterministic provider
    /// order (XOR-closest). Returns the chunk bytes on success.
    fn fetch_verified(&self, att: &DaAttestation, idx: u16, deadline: u64) -> Option<Vec<u8>> {
        let ch = chunk_hash(att, idx);
        let mut provs = self.transport.find_providers(ch);
        provs.sort_by_key(|p| xor_distance(&ch, &p.0)); // deterministic attempt order
        let mut attempts = 0;
        for p in provs {
            if self.clock.now_tick() > deadline || attempts >= self.max_attempts_per_chunk { break; }
            attempts += 1;
            if let Some((bytes, path)) = self.transport.fetch_chunk(ch, p) {
                if verify_chunk(att, idx, &bytes, &path) { return Some(bytes); }
            }
        }
        None
    }
}
