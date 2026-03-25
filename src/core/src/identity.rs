use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Sha256, Digest};

/// A 32-byte address derived from the validator's public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Address(pub [u8; 32]);

impl Address {
    /// Derive an address from an ed25519 public key via SHA-256.
    pub fn from_public_key(key: &VerifyingKey) -> Self {
        let hash = Sha256::digest(key.as_bytes());
        let mut addr = [0u8; 32];
        addr.copy_from_slice(&hash);
        Self(addr)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "comme:{}", hex::encode(&self.0[..8]))
    }
}

/// Hardware fingerprint reported by a validator node.
/// Used for Sybil detection — identical fingerprints across nodes is a flag.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct HardwareFingerprint {
    pub cpu_model: String,
    pub cpu_cores: u16,
    pub ram_total_mb: u64,
    pub gpu_model: Option<String>,
    pub gpu_vram_mb: Option<u64>,
    pub storage_total_mb: u64,
    pub os_family: String,
    /// Network interface speed in Mbps (0 if unknown).
    pub network_speed_mbps: u64,
}

impl HardwareFingerprint {
    /// Detect hardware on the current machine. Populates all fields from
    /// live system info. GPU detection is best-effort (checks common paths).
    pub fn detect() -> Self {
        use sysinfo::System;

        let mut sys = System::new_all();
        sys.refresh_all();

        let cpu_model = sys.cpus().first()
            .map(|c| c.brand().to_string())
            .unwrap_or_else(|| "unknown".into());
        let cpu_cores = sys.cpus().len() as u16;
        let ram_total_mb = sys.total_memory() / (1024 * 1024);

        // Disk space: sum of all disks
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let storage_total_mb: u64 = disks.iter()
            .map(|d| d.total_space() / (1024 * 1024))
            .sum();

        let os_family = std::env::consts::OS.to_string();

        // GPU detection: best-effort via common paths
        let (gpu_model, gpu_vram_mb) = detect_gpu();

        Self {
            cpu_model,
            cpu_cores,
            ram_total_mb,
            gpu_model,
            gpu_vram_mb,
            storage_total_mb,
            os_family,
            network_speed_mbps: 0, // Requires runtime measurement
        }
    }

    /// Compute a SHA-256 hash of this fingerprint for comparison.
    pub fn hash(&self) -> [u8; 32] {
        let data = format!(
            "{}:{}:{}:{}:{}:{}:{}",
            self.cpu_model, self.cpu_cores, self.ram_total_mb,
            self.gpu_model.as_deref().unwrap_or("none"),
            self.gpu_vram_mb.unwrap_or(0),
            self.storage_total_mb, self.os_family,
        );
        let hash = Sha256::digest(data.as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        out
    }
}

/// Best-effort GPU detection. Checks /proc (Linux) and command output.
fn detect_gpu() -> (Option<String>, Option<u64>) {
    // Try reading /proc/driver/nvidia/gpus on Linux
    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = std::fs::read_dir("/proc/driver/nvidia/gpus") {
            for entry in entries.flatten() {
                if let Ok(info) = std::fs::read_to_string(entry.path().join("information")) {
                    let model = info.lines()
                        .find(|l| l.starts_with("Model:"))
                        .map(|l| l.trim_start_matches("Model:").trim().to_string());
                    if model.is_some() {
                        return (model, None);
                    }
                }
            }
        }
        // Try lspci output
        if let Ok(output) = std::process::Command::new("lspci").output() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if line.contains("VGA") || line.contains("3D controller") {
                    let model = line.split(':').last()
                        .map(|s| s.trim().to_string());
                    return (model, None);
                }
            }
        }
    }
    (None, None)
}

/// Reported resource capacity of a validator node.
/// Measured relative to the reference node defined by the gold standard.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ResourceCapacity {
    /// CPU score relative to reference node (100 = reference baseline).
    pub cpu_score: u32,
    /// GPU score relative to reference node (100 = reference baseline, 0 = no GPU).
    pub gpu_score: u32,
    /// Available RAM in MB.
    pub ram_available_mb: u64,
    /// Available storage in MB.
    pub storage_available_mb: u64,
    /// Measured bandwidth in kbps.
    pub bandwidth_kbps: u64,
    /// Percentage of resources contributed (1-100).
    pub contribution_percent: u8,
}

/// Full validator identity on the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorIdentity {
    pub address: Address,
    pub public_key: VerifyingKey,
    pub hardware: HardwareFingerprint,
    pub capacity: ResourceCapacity,
    /// When this validator first registered on-chain.
    pub registered_epoch: u64,
    /// Cumulative uptime in seconds since registration.
    pub cumulative_uptime_secs: u64,
}

impl ValidatorIdentity {
    /// Contribution time as a Duration for grace period calculations.
    pub fn contribution_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.cumulative_uptime_secs)
    }

    /// Grace period balance in seconds. Equal to cumulative uptime,
    /// capped at 10 years.
    pub fn grace_period_secs(&self) -> u64 {
        const TEN_YEARS_SECS: u64 = 10 * 365 * 24 * 3600;
        self.cumulative_uptime_secs.min(TEN_YEARS_SECS)
    }
}
