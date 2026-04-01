// eclipse_attack_detector.rs — Detect eclipse attacks
//
// WHAT IT DOES:
//   Detects potential eclipse attacks based on peer subnet distribution:
//   - Alert if >50% of peers share a /16 subnet (same first two octets)
//   - Alert if all peers are in the same /8 network (same first octet)
//   - Suggest: disconnect some peers, seek diverse connections
//
// WHERE IT SHOULD GO: src/network/src/eclipse_attack_detector.rs
//
// WIRING REQUIRED:
//   1. Add `pub mod eclipse_attack_detector;` to src/network/src/lib.rs
//   2. Call detector.check(peer_subnets) periodically (e.g., every 60 seconds)
//   3. Log alerts and consider disconnecting concentrated peers
//   4. When connecting to new peers, prefer different /16 subnets

use std::collections::HashMap;
use tracing::warn;

/// Alert from the eclipse detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    /// More than 50% of peers share a /16 subnet.
    ConcentratedSubnet16 {
        subnet: String,    // e.g., "192.168"
        count: usize,
        total: usize,
        fraction: u32, // percent (e.g., 75 = 75%)
    },
    /// All peers are in the same /8 network.
    AllSameNetwork8 {
        network: String, // e.g., "192"
        count: usize,
    },
    /// Only one peer connected — trivially eclipse-able.
    TooFewPeers { count: usize },
}

impl Alert {
    pub fn description(&self) -> String {
        match self {
            Self::ConcentratedSubnet16 { subnet, count, total, fraction } =>
                format!(
                    "Eclipse risk: {}/{} peers ({fraction}%) are in /16 subnet {}. \
                    Consider disconnecting some and seeking peers in different subnets.",
                    count, total, subnet
                ),
            Self::AllSameNetwork8 { network, count } =>
                format!(
                    "Eclipse risk: all {} peers are in /8 network {}.*. \
                    This may indicate you are being eclipsed by a single network operator.",
                    count, network
                ),
            Self::TooFewPeers { count } =>
                format!(
                    "Only {} peer(s) connected. More peers = better eclipse resistance.",
                    count
                ),
        }
    }
}

/// A peer's IP subnet representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerSubnet {
    /// Full IP string (e.g., "192.168.1.5").
    pub ip: String,
}

impl PeerSubnet {
    pub fn new(ip: &str) -> Self {
        Self { ip: ip.to_string() }
    }

    /// Extract /16 subnet (first two octets).
    pub fn slash16(&self) -> String {
        let parts: Vec<&str> = self.ip.split('.').collect();
        if parts.len() >= 2 {
            format!("{}.{}", parts[0], parts[1])
        } else {
            self.ip.clone()
        }
    }

    /// Extract /8 network (first octet).
    pub fn slash8(&self) -> String {
        let parts: Vec<&str> = self.ip.split('.').collect();
        if !parts.is_empty() {
            parts[0].to_string()
        } else {
            self.ip.clone()
        }
    }
}

/// Eclipse attack detector.
pub struct EclipseDetector {
    /// Minimum peers before triggering concentration alerts.
    min_peers_for_alert: usize,
    /// Fraction threshold for /16 concentration (default: 0.5 = 50%).
    concentration_threshold: f64,
}

impl EclipseDetector {
    pub fn new() -> Self {
        Self {
            min_peers_for_alert: 2,
            concentration_threshold: 0.5,
        }
    }

    pub fn with_threshold(threshold: f64) -> Self {
        Self {
            min_peers_for_alert: 2,
            concentration_threshold: threshold,
        }
    }

    /// Check peer subnet distribution and return any alerts.
    pub fn check(&self, peer_subnets: &[PeerSubnet]) -> Vec<Alert> {
        let mut alerts = Vec::new();
        let total = peer_subnets.len();

        if total == 0 {
            return alerts;
        }

        // Too few peers
        if total < 3 {
            alerts.push(Alert::TooFewPeers { count: total });
        }

        if total < self.min_peers_for_alert {
            return alerts;
        }

        // Count /16 subnets
        let mut subnet16_counts: HashMap<String, usize> = HashMap::new();
        for peer in peer_subnets {
            *subnet16_counts.entry(peer.slash16()).or_insert(0) += 1;
        }

        for (subnet, count) in &subnet16_counts {
            let fraction = *count as f64 / total as f64;
            if fraction > self.concentration_threshold {
                let pct = (fraction * 100.0) as u32;
                warn!(
                    subnet = %subnet,
                    count = count,
                    total = total,
                    pct = pct,
                    "eclipse_detector: /16 subnet concentration detected"
                );
                alerts.push(Alert::ConcentratedSubnet16 {
                    subnet: subnet.clone(),
                    count: *count,
                    total,
                    fraction: pct,
                });
            }
        }

        // Check if all peers are in the same /8
        let mut network8_counts: HashMap<String, usize> = HashMap::new();
        for peer in peer_subnets {
            *network8_counts.entry(peer.slash8()).or_insert(0) += 1;
        }

        if network8_counts.len() == 1 {
            let (net, count) = network8_counts.iter().next().unwrap();
            if *count == total && total >= 3 {
                warn!(
                    network = %net,
                    count = count,
                    "eclipse_detector: all peers in same /8 network"
                );
                alerts.push(Alert::AllSameNetwork8 {
                    network: net.clone(),
                    count: *count,
                });
            }
        }

        alerts
    }

    /// Returns a diversity score: 0.0 (all same subnet) to 1.0 (all unique /16s).
    pub fn diversity_score(&self, peer_subnets: &[PeerSubnet]) -> f64 {
        if peer_subnets.len() <= 1 { return 0.0; }
        let unique_16s: std::collections::HashSet<String> =
            peer_subnets.iter().map(|p| p.slash16()).collect();
        unique_16s.len() as f64 / peer_subnets.len() as f64
    }
}

impl Default for EclipseDetector {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(ip: &str) -> PeerSubnet { PeerSubnet::new(ip) }

    #[test]
    fn no_alerts_for_diverse_peers() {
        let detector = EclipseDetector::new();
        let peers = vec![
            peer("1.2.3.4"),
            peer("5.6.7.8"),
            peer("9.10.11.12"),
            peer("13.14.15.16"),
        ];
        let alerts = detector.check(&peers);
        let concentration_alerts: Vec<&Alert> = alerts.iter()
            .filter(|a| matches!(a, Alert::ConcentratedSubnet16 { .. }))
            .collect();
        assert!(concentration_alerts.is_empty(), "diverse peers should not trigger concentration alert");
    }

    #[test]
    fn concentrated_subnet_triggers_alert() {
        let detector = EclipseDetector::new();
        // 3 out of 4 peers in same /16
        let peers = vec![
            peer("192.168.1.1"),
            peer("192.168.2.2"),
            peer("192.168.3.3"),
            peer("10.0.0.1"),
        ];
        let alerts = detector.check(&peers);
        let has_concentration = alerts.iter().any(|a| matches!(a, Alert::ConcentratedSubnet16 { .. }));
        assert!(has_concentration, "75% in same /16 should trigger alert");
    }

    #[test]
    fn all_same_8_triggers_alert() {
        let detector = EclipseDetector::new();
        let peers = vec![
            peer("10.1.2.3"),
            peer("10.2.3.4"),
            peer("10.3.4.5"),
            peer("10.4.5.6"),
        ];
        let alerts = detector.check(&peers);
        let has_8 = alerts.iter().any(|a| matches!(a, Alert::AllSameNetwork8 { .. }));
        assert!(has_8, "all /8 same should trigger alert");
    }

    #[test]
    fn too_few_peers_alert() {
        let detector = EclipseDetector::new();
        let peers = vec![peer("1.2.3.4"), peer("5.6.7.8")];
        let alerts = detector.check(&peers);
        let has_few = alerts.iter().any(|a| matches!(a, Alert::TooFewPeers { .. }));
        assert!(has_few, "2 peers should trigger TooFewPeers alert");
    }

    #[test]
    fn empty_peers_no_alerts() {
        let detector = EclipseDetector::new();
        let alerts = detector.check(&[]);
        assert!(alerts.is_empty());
    }

    #[test]
    fn slash16_extraction() {
        let p = PeerSubnet::new("192.168.1.50");
        assert_eq!(p.slash16(), "192.168");
    }

    #[test]
    fn slash8_extraction() {
        let p = PeerSubnet::new("10.20.30.40");
        assert_eq!(p.slash8(), "10");
    }

    #[test]
    fn diversity_score_all_different() {
        let detector = EclipseDetector::new();
        let peers = vec![peer("1.1.1.1"), peer("2.2.2.2"), peer("3.3.3.3"), peer("4.4.4.4")];
        let score = detector.diversity_score(&peers);
        assert!((score - 1.0).abs() < 0.01, "all different = 1.0 diversity");
    }

    #[test]
    fn diversity_score_all_same() {
        let detector = EclipseDetector::new();
        let peers = vec![peer("10.1.1.1"), peer("10.1.1.2"), peer("10.1.1.3")];
        let score = detector.diversity_score(&peers);
        assert!(score < 0.5, "all same /16 = low diversity: {}", score);
    }

    #[test]
    fn alert_description_nonempty() {
        let alert = Alert::ConcentratedSubnet16 {
            subnet: "192.168".to_string(),
            count: 3,
            total: 4,
            fraction: 75,
        };
        assert!(!alert.description().is_empty());
        assert!(alert.description().contains("192.168"));
    }
}
