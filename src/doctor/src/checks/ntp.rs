// checks/ntp.rs — NTP / SNTP drift check for commputer-doctor
//
// WHAT IT DOES:
//   Best-effort SNTP query against a small pool of public time servers. Warns
//   if the operator clock is more than 5 seconds off. Validators with skewed
//   clocks produce mis-timed votes and risk being seen as faulty.
//
// WHERE IT SHOULD GO:
//   src/doctor/src/checks/ntp.rs
//
// WIRING REQUIRED:
//   None. Self-contained. Uses raw UDP (RFC 4330 / SNTP minimal client) so we
//   do not pull in a heavy NTP crate.

use std::net::{ToSocketAddrs, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::CheckResult;
#[cfg(test)]
use crate::Severity;

/// Maximum drift before we warn the operator (seconds).
pub const MAX_DRIFT_SECS: f64 = 5.0;

/// Check NTP drift. Never returns Severity::Error — being unable to reach the
/// outside world is a Warning; the operator may be air-gapped.
pub fn check_ntp_drift() -> CheckResult {
    const POOL: &[&str] = &[
        "pool.ntp.org:123",
        "time.cloudflare.com:123",
        "time.google.com:123",
    ];
    for server in POOL {
        match query_sntp(server) {
            Ok(drift) => {
                let abs = drift.abs();
                if abs > MAX_DRIFT_SECS {
                    return CheckResult::warn(
                        "net.ntp",
                        format!("clock drift vs {}: {:+.2}s", server, drift),
                        "run `sudo timedatectl set-ntp true` or `sudo ntpdate -u pool.ntp.org`",
                    );
                }
                return CheckResult::ok(
                    "net.ntp",
                    format!("clock drift vs {}: {:+.3}s (OK)", server, drift),
                );
            }
            Err(_) => continue,
        }
    }
    CheckResult::warn(
        "net.ntp",
        "could not reach any NTP server",
        "verify outbound UDP/123, or skip with --skip-net if air-gapped",
    )
}

/// Returns drift = (local_clock - server_clock) in seconds. Positive means
/// the local clock is ahead.
fn query_sntp(server: &str) -> Result<f64, String> {
    let addr = server
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "no DNS result".to_string())?;

    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    sock.set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;

    // Build minimal SNTP request: 48-byte packet, LI=0 VN=4 Mode=3 client.
    let mut req = [0u8; 48];
    req[0] = 0b00_100_011;

    sock.send_to(&req, addr).map_err(|e| e.to_string())?;

    let mut buf = [0u8; 48];
    let (n, _) = sock.recv_from(&mut buf).map_err(|e| e.to_string())?;
    if n < 48 {
        return Err("short SNTP reply".into());
    }

    // Transmit timestamp lives at offset 40..48: u32 seconds + u32 fraction,
    // both big-endian, in the NTP epoch (1900-01-01).
    let secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) as u64;
    let frac = u32::from_be_bytes([buf[44], buf[45], buf[46], buf[47]]) as u64;
    if secs == 0 {
        return Err("server returned zero timestamp".into());
    }
    // Convert NTP epoch (1900) to UNIX epoch (1970). Diff = 2_208_988_800s.
    const NTP_UNIX_OFFSET: u64 = 2_208_988_800;
    let unix_secs = secs.checked_sub(NTP_UNIX_OFFSET).ok_or("pre-1970 timestamp")?;
    // Fraction-of-second to nanoseconds.
    let frac_ns = (frac as f64 / u32::MAX as f64) * 1.0e9;
    let server_ts = unix_secs as f64 + frac_ns / 1.0e9;

    let local = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    let local_ts = local.as_secs() as f64 + local.subsec_nanos() as f64 / 1.0e9;

    Ok(local_ts - server_ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test only — we cannot rely on outbound UDP in CI, so just ensure
    /// the function returns SOMETHING and never panics.
    #[test]
    fn check_ntp_drift_returns_a_result() {
        let r = check_ntp_drift();
        assert_ne!(r.severity, Severity::Error, "NTP should never be fatal");
        assert!(!r.message.is_empty());
    }
}
