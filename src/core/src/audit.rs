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
}
