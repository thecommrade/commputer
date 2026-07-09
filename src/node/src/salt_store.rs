//! Durable node-local salt store for the PoUW verifier commit/reveal loop (Track 2, Phase 1 — INERT).
//!
//! WHAT IT DOES: persists, per verified job, the secret `(result_hash, salt)` a verifier used to
//! build its `Commit` commitment `H(result_hash‖salt‖verifier)`, so the same node can later emit a
//! matching `Reveal` **across a restart**. The salt is never on-chain and is secret until reveal.
//!
//! WHY DURABILITY IS FUND-CRITICAL: a verifier that broadcasts a `Commit`, escrows its `verifier_bond`,
//! then crashes before revealing has its bond **burned** as a commit-no-reveal forfeiture
//! (`pouw-onchain/lifecycle.rs` settle, the forfeiture branch). The loop MUST persist the salt and
//! `fsync` it BEFORE broadcasting the `Commit` — otherwise a crash in that window burns an honest bond.
//! `insert` therefore fsyncs the file (atomic write + rename + dir fsync) BEFORE returning.
//!
//! WHERE IT IS WIRED IN (later, PROTECTED phase — NOT here): `main.rs` constructs a `SaltStore`
//! rooted at the node `data_dir`; the verifier loop calls `insert` before emitting `Commit`,
//! `get` when building `Reveal`, and `remove` after the job settles. This module is additive + inert:
//! it builds and unit-tests standalone and changes no running-node behavior.
//!
//! FORMAT: a single JSON file `verifier_salts.json` under the supplied dir, mode 0600 (owner-only — it
//! holds pre-reveal secrets). On-disk shape is a hex-encoded record array, written deterministically
//! (sorted by job_id) via a temp-file + atomic rename so a crash never leaves a torn file.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// File name (under the caller-supplied dir) holding the durable salt records.
const SALT_FILE_NAME: &str = "verifier_salts.json";

/// One persisted commit secret, hex-encoded for a stable human-readable on-disk form.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SaltRecord {
    job_id: String,
    result_hash: String,
    salt: String,
}

/// Durable `job_id -> (result_hash, salt)` map. In-memory index kept in sync with the on-disk file;
/// every mutation (`insert`/`remove`) re-persists + fsyncs before returning.
pub struct SaltStore {
    path: PathBuf,
    entries: HashMap<[u8; 32], ([u8; 32], [u8; 32])>,
}

impl SaltStore {
    /// Open (or create) the salt store rooted at `dir` (e.g. the node `data_dir`). Loads any existing
    /// `verifier_salts.json`; a missing file yields an empty store. `dir` is created if absent.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let path = dir.join(SALT_FILE_NAME);
        let entries = if path.exists() {
            let bytes = std::fs::read(&path)?;
            let records: Vec<SaltRecord> = serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut map = HashMap::with_capacity(records.len());
            for r in records {
                let job_id = decode32(&r.job_id)?;
                let result_hash = decode32(&r.result_hash)?;
                let salt = decode32(&r.salt)?;
                map.insert(job_id, (result_hash, salt));
            }
            map
        } else {
            HashMap::new()
        };
        Ok(Self { path, entries })
    }

    /// Persist `(result_hash, salt)` for `job_id`, fsyncing the file BEFORE returning. The verifier loop
    /// MUST await this successfully before broadcasting the `Commit` (see module header — a crash in the
    /// commit→reveal gap burns the bond). Overwrites any prior record for the same job.
    pub fn insert(
        &mut self,
        job_id: [u8; 32],
        result_hash: [u8; 32],
        salt: [u8; 32],
    ) -> io::Result<()> {
        self.entries.insert(job_id, (result_hash, salt));
        self.persist()
    }

    /// Recover the `(result_hash, salt)` a prior `insert` stored for `job_id`, if any. `None` means the
    /// loop holds no salt for this job → it MUST NOT reveal (a lost salt → abstain + accept forfeiture,
    /// never a slashable garbage reveal).
    pub fn get(&self, job_id: &[u8; 32]) -> Option<([u8; 32], [u8; 32])> {
        self.entries.get(job_id).copied()
    }

    /// Drop the record for `job_id` (call after the job settles — the secret is spent) and re-persist.
    /// Removing an absent job is a no-op that still rewrites the file durably.
    pub fn remove(&mut self, job_id: &[u8; 32]) -> io::Result<()> {
        self.entries.remove(job_id);
        self.persist()
    }

    /// Atomically rewrite the on-disk file and fsync it before returning. Temp-file + rename so a crash
    /// mid-write never leaves a torn/partial file; the temp and (via the parent dir fsync) the rename
    /// are both flushed to stable storage.
    fn persist(&self) -> io::Result<()> {
        // Deterministic on-disk order (sorted by job_id) — reproducible file, easy to diff.
        let mut records: Vec<SaltRecord> = self
            .entries
            .iter()
            .map(|(job_id, (rh, salt))| SaltRecord {
                job_id: hex::encode(job_id),
                result_hash: hex::encode(rh),
                salt: hex::encode(salt),
            })
            .collect();
        records.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        let json = serde_json::to_vec_pretty(&records)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let tmp = self.path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(&json)?;
            // fsync the file contents+metadata BEFORE the rename — the durability barrier.
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Fix a pre-existing tmp that predated the 0600 create.
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        // fsync the DIRECTORY so the rename (a new/updated dir entry) is itself durable.
        // The commit-before-reveal guarantee ultimately depends on this: on a fresh
        // insert a lost rename would drop the salt and burn the verifier bond on the
        // subsequent crash. Surface the error rather than swallow it — a caller that
        // cannot durably persist the salt MUST NOT proceed to broadcast the Commit.
        // (Opening a directory as a File and sync_all() is the portable-enough idiom;
        // filesystems that don't need dir-fsync return Ok.)
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            let dir = std::fs::File::open(parent)?;
            dir.sync_all()?;
        }
        Ok(())
    }
}

/// Decode a 32-byte hex string into `[u8; 32]`, mapping any malformed value to an I/O data error.
fn decode32(s: &str) -> io::Result<[u8; 32]> {
    let v = hex::decode(s).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    v.as_slice()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "expected 32-byte hex value"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique scratch dir per test (node crate has no `tempfile` dev-dep; avoid adding one).
    fn scratch_dir() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "commputer_salt_test_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn insert_then_get_returns_the_pair() {
        let dir = scratch_dir();
        let mut s = SaltStore::open(&dir).unwrap();
        let job = [1u8; 32];
        let rh = [2u8; 32];
        let salt = [3u8; 32];
        assert_eq!(s.get(&job), None, "empty store returns None");
        s.insert(job, rh, salt).unwrap();
        assert_eq!(s.get(&job), Some((rh, salt)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The core durability property: a salt inserted, then the store dropped and REOPENED from disk,
    /// is recovered verbatim — this is what lets the verifier reveal after a restart.
    #[test]
    fn reopen_after_insert_recovers_the_salt() {
        let dir = scratch_dir();
        let job = [9u8; 32];
        let rh = [8u8; 32];
        let salt = [7u8; 32];
        {
            let mut s = SaltStore::open(&dir).unwrap();
            s.insert(job, rh, salt).unwrap();
        } // dropped — nothing in memory
        let reopened = SaltStore::open(&dir).unwrap();
        assert_eq!(
            reopened.get(&job),
            Some((rh, salt)),
            "salt must survive a restart (fsynced to disk before insert returned)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_is_durable_and_leaves_other_records() {
        let dir = scratch_dir();
        let (j1, j2) = ([10u8; 32], [20u8; 32]);
        {
            let mut s = SaltStore::open(&dir).unwrap();
            s.insert(j1, [1u8; 32], [1u8; 32]).unwrap();
            s.insert(j2, [2u8; 32], [2u8; 32]).unwrap();
            s.remove(&j1).unwrap();
        }
        let reopened = SaltStore::open(&dir).unwrap();
        assert_eq!(reopened.get(&j1), None, "removed record must not resurface");
        assert_eq!(reopened.get(&j2), Some(([2u8; 32], [2u8; 32])), "sibling record survives");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn insert_overwrites_prior_record_for_same_job() {
        let dir = scratch_dir();
        let job = [5u8; 32];
        let mut s = SaltStore::open(&dir).unwrap();
        s.insert(job, [1u8; 32], [1u8; 32]).unwrap();
        s.insert(job, [2u8; 32], [2u8; 32]).unwrap();
        assert_eq!(s.get(&job), Some(([2u8; 32], [2u8; 32])));
        let reopened = SaltStore::open(&dir).unwrap();
        assert_eq!(reopened.get(&job), Some(([2u8; 32], [2u8; 32])));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn file_is_owner_only_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir();
        let mut s = SaltStore::open(&dir).unwrap();
        s.insert([1u8; 32], [2u8; 32], [3u8; 32]).unwrap();
        let mode = std::fs::metadata(dir.join(SALT_FILE_NAME)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "salt file holds pre-reveal secrets — must be owner-only");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
