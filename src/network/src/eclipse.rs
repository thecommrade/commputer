use std::collections::HashMap;

/// Maximum number of peers allowed from the same /16 subnet.
pub const MAX_PEERS_PER_SUBNET: usize = 3;

/// Tracks the diversity of peer IP addresses to mitigate eclipse attacks.
/// Groups peers by /16 subnet prefix and limits how many peers can share
/// the same subnet range.
#[derive(Debug, Clone, Default)]
pub struct DiversityTracker {
    /// Map from /16 subnet prefix (e.g. "192.168") to count of peers in that range.
    subnet_counts: HashMap<String, usize>,
}

/// Extract the /16 subnet prefix from an IP address string.
/// Returns the first two octets joined by a dot, e.g. "192.168" from "192.168.1.5".
fn extract_subnet_16(ip: &str) -> Option<String> {
    // Strip port if present (e.g., "192.168.1.5:9000")
    let ip_part = ip.split(':').next().unwrap_or(ip);
    let octets: Vec<&str> = ip_part.split('.').collect();
    if octets.len() >= 2 {
        Some(format!("{}.{}", octets[0], octets[1]))
    } else {
        None
    }
}

/// Check whether a new peer from the given IP should be accepted,
/// based on the current subnet diversity.
pub fn should_accept_peer(ip: &str, tracker: &DiversityTracker) -> bool {
    match extract_subnet_16(ip) {
        Some(subnet) => {
            let count = tracker.subnet_counts.get(&subnet).copied().unwrap_or(0);
            count < MAX_PEERS_PER_SUBNET
        }
        None => false, // Reject unparseable IPs
    }
}

/// Register a new peer's IP in the diversity tracker.
pub fn add_peer(ip: &str, tracker: &mut DiversityTracker) {
    if let Some(subnet) = extract_subnet_16(ip) {
        *tracker.subnet_counts.entry(subnet).or_insert(0) += 1;
    }
}

impl DiversityTracker {
    /// Create a new empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remove a peer from tracking (e.g., when a peer disconnects).
    pub fn remove_peer(&mut self, ip: &str) {
        if let Some(subnet) = extract_subnet_16(ip)
            && let Some(count) = self.subnet_counts.get_mut(&subnet) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.subnet_counts.remove(&subnet);
                }
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_peers_under_limit() {
        let mut tracker = DiversityTracker::new();
        assert!(should_accept_peer("192.168.1.1", &tracker));
        add_peer("192.168.1.1", &mut tracker);
        assert!(should_accept_peer("192.168.1.2", &tracker));
        add_peer("192.168.1.2", &mut tracker);
        assert!(should_accept_peer("192.168.1.3", &tracker));
        add_peer("192.168.1.3", &mut tracker);
    }

    #[test]
    fn reject_peers_over_limit() {
        let mut tracker = DiversityTracker::new();
        add_peer("10.0.1.1", &mut tracker);
        add_peer("10.0.2.2", &mut tracker);
        add_peer("10.0.3.3", &mut tracker);
        // 4th peer from 10.0.x.x should be rejected
        assert!(!should_accept_peer("10.0.4.4", &tracker));
    }

    #[test]
    fn different_subnets_are_independent() {
        let mut tracker = DiversityTracker::new();
        add_peer("10.0.1.1", &mut tracker);
        add_peer("10.0.1.2", &mut tracker);
        add_peer("10.0.1.3", &mut tracker);
        // Different /16 subnet should still be accepted
        assert!(should_accept_peer("10.1.1.1", &tracker));
        assert!(should_accept_peer("172.16.0.1", &tracker));
    }

    #[test]
    fn ip_with_port_handled() {
        let mut tracker = DiversityTracker::new();
        add_peer("192.168.1.1:9000", &mut tracker);
        add_peer("192.168.1.2:9001", &mut tracker);
        add_peer("192.168.1.3:9002", &mut tracker);
        assert!(!should_accept_peer("192.168.5.5:8080", &tracker));
    }

    #[test]
    fn reject_invalid_ip() {
        let tracker = DiversityTracker::new();
        assert!(!should_accept_peer("not-an-ip", &tracker));
    }

    #[test]
    fn remove_peer_frees_slot() {
        let mut tracker = DiversityTracker::new();
        add_peer("10.0.1.1", &mut tracker);
        add_peer("10.0.1.2", &mut tracker);
        add_peer("10.0.1.3", &mut tracker);
        assert!(!should_accept_peer("10.0.1.4", &tracker));
        tracker.remove_peer("10.0.1.1");
        assert!(should_accept_peer("10.0.1.4", &tracker));
    }
}
