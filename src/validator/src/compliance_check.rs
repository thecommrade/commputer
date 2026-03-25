use std::collections::HashMap;
use commputer_core::identity::Address;
use commputer_core::compliance::ComplianceStatus;

/// Extracts the /24 subnet prefix from an IPv4 string (first three octets).
/// Returns `None` if the string is not a valid IPv4 address.
fn subnet_24(ip: &str) -> Option<String> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() == 4 {
        Some(format!("{}.{}.{}", parts[0], parts[1], parts[2]))
    } else {
        None
    }
}

/// Tracks IP → validator mappings and determines IP-based compliance status.
///
/// Rules:
/// - Multiple nodes on the same exact IP → `NerfedIncidental`
/// - Multiple nodes on the same /24 subnet (but different IPs) → `NerfedIncidental`
/// - A single node on a unique IP and unique subnet → `Compliant`
#[derive(Debug, Default)]
pub struct ComplianceChecker {
    /// Maps each registered address to its reported IP string.
    node_to_ip: HashMap<Address, String>,
}

impl ComplianceChecker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a node with its reported IP address.
    /// Re-registering with a new IP updates the mapping.
    pub fn register_node(&mut self, addr: Address, ip: String) {
        self.node_to_ip.insert(addr, ip);
    }

    /// Remove a node from the registry.
    pub fn deregister_node(&mut self, addr: &Address) {
        self.node_to_ip.remove(addr);
    }

    /// Check the compliance status of a registered node.
    ///
    /// Returns `Compliant` if the node is not registered (not our concern).
    pub fn check(&self, addr: &Address) -> ComplianceStatus {
        let Some(ip) = self.node_to_ip.get(addr) else {
            return ComplianceStatus::Compliant;
        };

        let subnet = subnet_24(ip);

        // Check whether any *other* node shares the same IP or /24 subnet.
        for (other_addr, other_ip) in &self.node_to_ip {
            if other_addr == addr {
                continue;
            }

            // Same exact IP → NerfedIncidental.
            if other_ip == ip {
                return ComplianceStatus::NerfedIncidental;
            }

            // Same /24 subnet → NerfedIncidental.
            if let (Some(s), Some(other_s)) = (&subnet, subnet_24(other_ip)) {
                if *s == other_s {
                    return ComplianceStatus::NerfedIncidental;
                }
            }
        }

        ComplianceStatus::Compliant
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> Address {
        Address([n; 32])
    }

    #[test]
    fn single_node_per_ip_is_compliant() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "192.168.1.10".into());
        assert_eq!(checker.check(&addr(1)), ComplianceStatus::Compliant);
    }

    #[test]
    fn two_nodes_same_ip_flagged() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "192.168.1.10".into());
        checker.register_node(addr(2), "192.168.1.10".into());
        assert_eq!(checker.check(&addr(1)), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn same_subnet_flagged() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "192.168.1.10".into());
        checker.register_node(addr(2), "192.168.1.11".into());
        assert_eq!(checker.check(&addr(2)), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn compliance_restored_on_deregister() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "192.168.1.10".into());
        checker.register_node(addr(2), "192.168.1.10".into());
        checker.deregister_node(&addr(2));
        assert_eq!(checker.check(&addr(1)), ComplianceStatus::Compliant);
    }
}
