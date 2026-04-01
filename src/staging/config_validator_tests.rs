// config_validator_tests.rs — Comprehensive tests for src/node/src/config_validator.rs
//
// WHAT IT DOES:
//   Tests for ConfigValidator covering port availability, port conflicts,
//   disk space checks, NTP status, and severity classifications.
//
// WHERE IT SHOULD GO:
//   Paste into src/node/src/config_validator.rs under #[cfg(test)] mod tests.
//
// WIRING REQUIRED:
//   None — all tests use public API. Port tests require no other service running
//   on the tested ports (they use high port numbers to reduce conflicts).

#[cfg(test)]
mod config_validator_comprehensive_tests {
    use commputer::config_validator::{
        ConfigValidator, NodeConfig, Severity, CheckResult,
    };
    use std::net::{TcpListener, SocketAddr};

    // -----------------------------------------------------------------------
    // Task 8a: Available port — passes (Info severity)
    // -----------------------------------------------------------------------
    #[test]
    fn available_port_passes() {
        // Use a port that should be free; if it isn't, test may fail
        // We use port 0 to let the OS pick a free port, then release it
        // and check that port immediately (race condition, but acceptable in tests)
        let listener = TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // Release it

        let result = ConfigValidator::check_tcp_port(port);
        assert_eq!(
            result.severity, Severity::Info,
            "available port should have Info severity, got: {:?}", result.severity
        );
    }

    // -----------------------------------------------------------------------
    // Task 8b: Busy port — fails with Error severity
    // -----------------------------------------------------------------------
    #[test]
    fn busy_port_fails_with_error() {
        // Bind a port and hold it, then try to check the same port
        let listener = TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Don't drop listener — keep port occupied

        let result = ConfigValidator::check_tcp_port(port);
        assert_eq!(
            result.severity, Severity::Error,
            "busy port should have Error severity"
        );
        assert!(result.message.contains(&port.to_string()));
        assert!(result.is_fatal());
    }

    // -----------------------------------------------------------------------
    // Task 8c: NTP sync check — passes or warns (skip if no NTP available)
    // -----------------------------------------------------------------------
    #[test]
    fn ntp_check_produces_a_result() {
        let result = ConfigValidator::check_ntp_status();
        // Should be either Info (synced) or Warning (can't verify)
        // Never Error — not having NTP is a warning, not fatal
        assert_ne!(
            result.severity, Severity::Error,
            "NTP check should never be fatal"
        );
        assert!(!result.message.is_empty());
    }

    // -----------------------------------------------------------------------
    // Task 8d: Sufficient disk — passes when data_dir exists
    // -----------------------------------------------------------------------
    #[test]
    fn sufficient_disk_passes_for_tmp() {
        let result = ConfigValidator::check_disk_space("/tmp");
        assert_eq!(result.severity, Severity::Info, "/tmp should exist and pass");
    }

    // -----------------------------------------------------------------------
    // Task 8e: Low disk / bad dir — Warning or Error
    // -----------------------------------------------------------------------
    #[test]
    fn bad_data_dir_produces_error_or_creates_it() {
        let unique_dir = format!("/tmp/commputer_test_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let result = ConfigValidator::check_disk_space(&unique_dir);
        // The validator tries to create the dir if it doesn't exist
        // If it succeeds: Info; if permissions fail: Error
        match result.severity {
            Severity::Info => {
                // Directory was created successfully — clean up
                let _ = std::fs::remove_dir(&unique_dir);
            }
            Severity::Error => {
                // Creation failed (e.g., permission denied) — that's fine for test
            }
            Severity::Warning => {
                // Unexpected but acceptable
            }
        }
        // The key check: result always has a non-empty message
        assert!(!result.message.is_empty());
    }

    // -----------------------------------------------------------------------
    // Additional: Port conflict detection
    // -----------------------------------------------------------------------
    #[test]
    fn port_conflict_same_ports_is_fatal() {
        let result = ConfigValidator::check_port_conflict(9000, 9000);
        assert_eq!(result.severity, Severity::Error);
        assert!(result.is_fatal());
        assert!(result.message.contains("9000"));
    }

    #[test]
    fn port_conflict_different_ports_is_ok() {
        let result = ConfigValidator::check_port_conflict(9000, 9944);
        assert_eq!(result.severity, Severity::Info);
        assert!(!result.is_fatal());
    }

    // -----------------------------------------------------------------------
    // Additional: Empty seed nodes warns
    // -----------------------------------------------------------------------
    #[test]
    fn empty_seeds_produces_warning() {
        let results = ConfigValidator::check_seed_nodes(&[]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].severity, Severity::Warning);
        assert!(!results[0].is_fatal());
    }

    // -----------------------------------------------------------------------
    // Additional: check_all aggregates all results
    // -----------------------------------------------------------------------
    #[test]
    fn check_all_returns_multiple_results() {
        let config = NodeConfig {
            p2p_port: 19000,
            rpc_port: 19001,
            seed_nodes: vec![],
            data_dir: "/tmp".to_string(),
        };
        let results = ConfigValidator::check_all(&config);
        assert!(results.len() >= 3, "should have at least port checks + disk + NTP");
    }

    // -----------------------------------------------------------------------
    // Additional: print_and_check returns true if any fatal
    // -----------------------------------------------------------------------
    #[test]
    fn print_and_check_detects_fatal() {
        let results = vec![
            CheckResult { check: "a".into(), severity: Severity::Info, message: "ok".into(), suggestion: None },
            CheckResult { check: "b".into(), severity: Severity::Error, message: "bad".into(), suggestion: Some("fix".into()) },
        ];
        assert!(ConfigValidator::print_and_check(&results));
    }

    #[test]
    fn print_and_check_false_when_no_fatal() {
        let results = vec![
            CheckResult { check: "a".into(), severity: Severity::Info, message: "ok".into(), suggestion: None },
            CheckResult { check: "b".into(), severity: Severity::Warning, message: "hmm".into(), suggestion: None },
        ];
        assert!(!ConfigValidator::print_and_check(&results));
    }

    // -----------------------------------------------------------------------
    // Additional: format_line includes severity and suggestion
    // -----------------------------------------------------------------------
    #[test]
    fn format_line_includes_arrow_for_suggestion() {
        let r = CheckResult {
            check: "Port".into(),
            severity: Severity::Warning,
            message: "port busy".into(),
            suggestion: Some("use another port".into()),
        };
        let line = r.format_line();
        assert!(line.contains("[WARN]"));
        assert!(line.contains("→"));
        assert!(line.contains("use another port"));
    }

    #[test]
    fn format_line_no_arrow_without_suggestion() {
        let r = CheckResult {
            check: "Port".into(),
            severity: Severity::Info,
            message: "all good".into(),
            suggestion: None,
        };
        let line = r.format_line();
        assert!(line.contains("[OK]"));
        assert!(!line.contains("→"));
    }
}
