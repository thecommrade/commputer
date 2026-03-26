use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

/// Categories of auditable events in the Commputer network.
#[derive(Debug, Clone)]
pub enum AuditEvent {
    /// A peer was banned from the network.
    PeerBanned { peer_id: String, reason: String },
    /// Compliance status changed for a validator.
    ComplianceChanged { validator: String, new_status: String },
    /// A slashing penalty was applied.
    SlashingApplied { validator: String, amount: u64 },
    /// A validation check failed.
    ValidationFailed { details: String },
    /// A configuration parameter was changed.
    ConfigChanged { key: String, old_value: String, new_value: String },
}

impl fmt::Display for AuditEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuditEvent::PeerBanned { peer_id, reason } => {
                write!(f, "PEER_BANNED peer={} reason={}", peer_id, reason)
            }
            AuditEvent::ComplianceChanged { validator, new_status } => {
                write!(f, "COMPLIANCE_CHANGED validator={} status={}", validator, new_status)
            }
            AuditEvent::SlashingApplied { validator, amount } => {
                write!(f, "SLASHING_APPLIED validator={} amount={}", validator, amount)
            }
            AuditEvent::ValidationFailed { details } => {
                write!(f, "VALIDATION_FAILED details={}", details)
            }
            AuditEvent::ConfigChanged { key, old_value, new_value } => {
                write!(f, "CONFIG_CHANGED key={} old={} new={}", key, old_value, new_value)
            }
        }
    }
}

/// Thread-safe audit logger that writes events with timestamps to a file.
pub struct AuditLogger {
    file: Mutex<File>,
}

impl AuditLogger {
    /// Create a new AuditLogger that writes to the given file path.
    /// Creates the file if it doesn't exist, appends if it does.
    pub fn new(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self { file: Mutex::new(file) })
    }

    /// Log an audit event with a timestamp.
    pub fn log_event(&self, event: AuditEvent) -> std::io::Result<()> {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let line = format!("[{}] {}\n", timestamp, event);
        let mut file = self.file.lock().unwrap();
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }
}

// === Items 44-55: Security hardening utilities ===

/// Item 44: Checked arithmetic for supply calculations.
pub fn checked_supply_add(a: u64, b: u64) -> Option<u64> {
    a.checked_add(b)
}

pub fn checked_supply_sub(a: u64, b: u64) -> Option<u64> {
    a.checked_sub(b)
}

pub fn checked_supply_mul(a: u64, b: u64) -> Option<u64> {
    a.checked_mul(b)
}

/// Item 45: Constant-time comparison for cryptographic values.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Item 46: Zeroize a byte slice (for secret key cleanup).
pub fn zeroize_bytes(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        unsafe {
            std::ptr::write_volatile(b, 0u8);
        }
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

/// Item 48: DoS protection size limits.
pub const MAX_TX_SIZE_BYTES: usize = 65_536;
pub const MAX_PROOF_SIZE_BYTES: usize = 131_072;
pub const MAX_RPC_REQUEST_BYTES: usize = 1_048_576;
pub const MAX_GOSSIP_MESSAGE_BYTES: usize = 2_097_152;
pub const MAX_ORPHAN_POOL_SIZE: usize = 1_000;
pub const MAX_BANNED_PEERS: usize = 10_000;

/// Item 54: Sanitize error messages to prevent internal state leakage.
pub fn sanitize_error(msg: &str) -> String {
    let mut sanitized = msg.to_string();
    // Strip anything that looks like a file path.
    while let Some(start) = sanitized.find('/') {
        if let Some(end) = sanitized[start..].find(|c: char| c.is_whitespace()) {
            sanitized.replace_range(start..start + end, "[path]");
        } else {
            sanitized.replace_range(start.., "[path]");
            break;
        }
    }
    sanitized
}

/// Item 55: Check if a string contains sensitive data that should not be logged.
pub fn contains_sensitive(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("private key")
        || lower.contains("secret key")
        || lower.contains("seed phrase")
        || lower.contains("mnemonic")
        || lower.contains("password")
}

/// Item 55: Redact sensitive data from a log message.
pub fn redact_sensitive(msg: &str) -> String {
    if contains_sensitive(msg) {
        "[REDACTED]".to_string()
    } else {
        msg.to_string()
    }
}

/// Item 50: File permission constants.
pub struct FilePermissions;
impl FilePermissions {
    pub const SECRET: u32 = 0o600;
    pub const CONFIG: u32 = 0o644;
}

/// Item 50: Set file permissions (Unix only).
#[cfg(unix)]
pub fn set_file_permissions(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
pub fn set_file_permissions(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn audit_logger_writes_events() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("audit_test_{}.log", std::process::id()));
        let path_str = path.to_str().unwrap();

        let logger = AuditLogger::new(path_str).unwrap();
        logger.log_event(AuditEvent::PeerBanned {
            peer_id: "peer123".into(),
            reason: "misbehavior".into(),
        }).unwrap();
        logger.log_event(AuditEvent::SlashingApplied {
            validator: "val456".into(),
            amount: 1000,
        }).unwrap();

        let mut contents = String::new();
        File::open(path_str).unwrap().read_to_string(&mut contents).unwrap();
        assert!(contents.contains("PEER_BANNED"));
        assert!(contents.contains("peer123"));
        assert!(contents.contains("SLASHING_APPLIED"));
        assert!(contents.contains("val456"));
        assert!(contents.contains("1000"));

        // Cleanup
        std::fs::remove_file(path_str).ok();
    }

    #[test]
    fn audit_event_display() {
        let event = AuditEvent::ConfigChanged {
            key: "max_peers".into(),
            old_value: "50".into(),
            new_value: "100".into(),
        };
        let display = format!("{}", event);
        assert!(display.contains("CONFIG_CHANGED"));
        assert!(display.contains("max_peers"));
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hi", b"hello"));
    }

    #[test]
    fn zeroize_works() {
        let mut data = vec![0xAA; 32];
        zeroize_bytes(&mut data);
        assert!(data.iter().all(|&b| b == 0));
    }

    #[test]
    fn checked_supply_overflow() {
        assert!(checked_supply_add(u64::MAX, 1).is_none());
        assert!(checked_supply_sub(0, 1).is_none());
        assert!(checked_supply_mul(u64::MAX, 2).is_none());
    }

    #[test]
    fn detects_sensitive_data() {
        assert!(contains_sensitive("my private key is abc"));
        assert!(contains_sensitive("seed phrase: word1 word2"));
        assert!(!contains_sensitive("block height is 42"));
    }

    #[test]
    fn redacts_sensitive() {
        assert_eq!(redact_sensitive("private key: deadbeef"), "[REDACTED]");
        assert_eq!(redact_sensitive("block height 42"), "block height 42");
    }
}
