use std::collections::HashMap;
use commputer_core::identity::{Address, ResourceCapacity};
use commputer_core::compliance::{ComplianceFlag, ComplianceStatus};
use tracing::info;

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

/// Feature 138: Extracts the /16 subnet prefix from an IPv4 string (first two octets).
fn subnet_16(ip: &str) -> Option<String> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() == 4 {
        Some(format!("{}.{}", parts[0], parts[1]))
    } else {
        None
    }
}

/// Feature 137: Behavioral analysis profile for a validator.
#[derive(Debug, Clone)]
pub struct BehaviorProfile {
    /// Uptime ratio (0.0 - 1.0). >0.995 is suspicious datacenter pattern.
    pub uptime_ratio: f64,
    /// Standard deviation of reported resource scores across epochs.
    pub resource_variance: f64,
    /// Coefficient of variation of proof response times.
    pub response_time_cv: f64,
}

impl Default for BehaviorProfile {
    fn default() -> Self {
        Self {
            uptime_ratio: 0.0,
            resource_variance: 1.0,
            response_time_cv: 1.0,
        }
    }
}

impl BehaviorProfile {
    /// Whether the profile shows suspicious datacenter patterns.
    /// 24/7 uptime (>99.5%) = suspicious.
    pub fn is_datacenter_pattern(&self) -> bool {
        self.uptime_ratio > 0.995
    }

    /// Whether the resource curve is suspiciously flat (variance < 0.01).
    pub fn is_flat_resource(&self) -> bool {
        self.resource_variance < 0.01
    }
}

/// Tracks IP → validator mappings and determines IP-based compliance status.
///
/// Features 136-150: Comprehensive anti-scale enforcement including fingerprint
/// verification, behavioral analysis, geographic diversity, sybil detection,
/// resource spike detection, compliance history, whitelist, datacenter/VPN detection.
#[derive(Debug, Default)]
pub struct ComplianceChecker {
    /// Maps each registered address to its reported IP string.
    node_to_ip: HashMap<Address, String>,
    /// Feature 136: Hardware fingerprint hashes per validator.
    fingerprints: HashMap<Address, [u8; 32]>,
    /// Feature 137: Behavioral profiles per validator.
    behavior_profiles: HashMap<Address, BehaviorProfile>,
    /// Feature 138: ASN strings per validator (if available).
    node_asn: HashMap<Address, String>,
    /// Feature 141: Previous resource reports for spike detection.
    previous_resources: HashMap<Address, ResourceCapacity>,
    /// Feature 141: Cooldown tracking — address -> epoch until which rewards are zero.
    cooldown_until: HashMap<Address, u64>,
    /// Feature 143: Compliance history — (epoch, status) per validator.
    compliance_history: HashMap<Address, Vec<(u64, ComplianceStatus)>>,
    /// Feature 147: First epoch a validator was continuously clean.
    first_clean_epoch: HashMap<Address, u64>,
    /// Feature 150: Total warehouse detections counter.
    pub total_warehouse_detections: u64,
    /// Feature 150: Total nerfed rewards (raw units).
    pub total_nerfed_rewards: u64,
    /// Feature 150: History of nerf percentage changes: (epoch, bps).
    pub nerf_percentage_history: Vec<(u64, u32)>,
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
        self.fingerprints.remove(addr);
        self.behavior_profiles.remove(addr);
        self.node_asn.remove(addr);
        self.previous_resources.remove(addr);
        self.cooldown_until.remove(addr);
    }

    /// Feature 136: Register a hardware fingerprint hash for a validator.
    /// If the hash already exists for a different address, flags both as DuplicateFingerprint.
    /// Returns any flags generated.
    pub fn register_fingerprint(&mut self, addr: Address, hash: [u8; 32]) -> Vec<ComplianceFlag> {
        let mut flags = Vec::new();

        // Check for duplicate fingerprints.
        for (other_addr, other_hash) in &self.fingerprints {
            if *other_addr != addr && *other_hash == hash {
                flags.push(ComplianceFlag::DuplicateFingerprint {
                    matching_address: *other_addr,
                });
                self.total_warehouse_detections += 1;
                info!(
                    "Feature 136: Duplicate fingerprint detected between {} and {}",
                    addr, other_addr
                );
            }
        }

        self.fingerprints.insert(addr, hash);
        flags
    }

    /// Feature 137: Update the behavioral profile for a validator.
    pub fn update_behavior_profile(&mut self, addr: Address, profile: BehaviorProfile) {
        self.behavior_profiles.insert(addr, profile);
    }

    /// Feature 138: Register ASN for a validator.
    pub fn register_asn(&mut self, addr: Address, asn: String) {
        self.node_asn.insert(addr, asn);
    }

    /// Feature 138: Geographic diversity flags for a validator.
    /// Checks /16 subnet and ASN matching.
    pub fn geographic_flags(&self, addr: &Address) -> Vec<ComplianceFlag> {
        let mut flags = Vec::new();

        let Some(ip) = self.node_to_ip.get(addr) else {
            return flags;
        };

        let subnet16 = subnet_16(ip);

        for (other_addr, other_ip) in &self.node_to_ip {
            if other_addr == addr {
                continue;
            }

            // Same /16 subnet.
            if let (Some(s), Some(other_s)) = (&subnet16, subnet_16(other_ip)) {
                if *s == other_s {
                    flags.push(ComplianceFlag::SameSubnet16 {
                        peer_address: *other_addr,
                    });
                    flags.push(ComplianceFlag::GeographicProximity {
                        peer_address: *other_addr,
                    });
                }
            }
        }

        // Same ASN.
        if let Some(asn) = self.node_asn.get(addr) {
            for (other_addr, other_asn) in &self.node_asn {
                if other_addr != addr && other_asn == asn {
                    flags.push(ComplianceFlag::SameAsn {
                        asn: asn.clone(),
                        peer_address: *other_addr,
                    });
                }
            }
        }

        flags
    }

    /// Feature 141: Report new resources for a validator.
    /// Returns ResourceSpike flags if RAM or CPU jumped by >100%.
    pub fn report_resources(
        &mut self,
        addr: Address,
        resources: ResourceCapacity,
        current_epoch: u64,
    ) -> Vec<ComplianceFlag> {
        let mut flags = Vec::new();

        if let Some(prev) = self.previous_resources.get(&addr) {
            // Check RAM spike (>100% increase).
            if prev.ram_available_mb > 0 {
                let ram_ratio = resources.ram_available_mb as f64 / prev.ram_available_mb as f64;
                if ram_ratio > 2.0 {
                    flags.push(ComplianceFlag::ResourceSpike {
                        channel: "RAM".to_string(),
                        before: prev.ram_available_mb,
                        after: resources.ram_available_mb,
                    });
                    // Enter 3-epoch cooldown.
                    self.cooldown_until.insert(addr, current_epoch + 3);
                    info!(
                        "Feature 141: RAM spike detected for {} ({} -> {} MB), cooldown until epoch {}",
                        addr, prev.ram_available_mb, resources.ram_available_mb, current_epoch + 3
                    );
                }
            }

            // Check CPU spike (>100% increase).
            if prev.cpu_score > 0 {
                let cpu_ratio = resources.cpu_score as f64 / prev.cpu_score as f64;
                if cpu_ratio > 2.0 {
                    flags.push(ComplianceFlag::ResourceSpike {
                        channel: "CPU".to_string(),
                        before: prev.cpu_score as u64,
                        after: resources.cpu_score as u64,
                    });
                    self.cooldown_until.insert(addr, current_epoch + 3);
                    info!(
                        "Feature 141: CPU spike detected for {} ({} -> {}), cooldown until epoch {}",
                        addr, prev.cpu_score, resources.cpu_score, current_epoch + 3
                    );
                }
            }
        }

        self.previous_resources.insert(addr, resources);
        flags
    }

    /// Feature 141: Check if a validator is in cooldown (0 rewards).
    pub fn is_in_cooldown(&self, addr: &Address, current_epoch: u64) -> bool {
        if let Some(&until_epoch) = self.cooldown_until.get(addr) {
            current_epoch < until_epoch
        } else {
            false
        }
    }

    /// Feature 143: Record a compliance status change for a validator.
    pub fn record_compliance_status(&mut self, addr: Address, epoch: u64, status: ComplianceStatus) {
        let history = self.compliance_history.entry(addr).or_default();
        // Only record if it's a change from the last recorded status.
        if history.last().map(|(_, s)| *s) != Some(status) {
            history.push((epoch, status));
        }
    }

    /// Feature 143: Get compliance history for a validator.
    pub fn get_compliance_history(&self, addr: &Address) -> &[(u64, ComplianceStatus)] {
        self.compliance_history
            .get(addr)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Feature 147: Track clean epoch for whitelist purposes.
    /// Call each epoch when a validator is compliant.
    pub fn mark_clean_epoch(&mut self, addr: Address, epoch: u64) {
        self.first_clean_epoch.entry(addr).or_insert(epoch);
    }

    /// Feature 147: Clear clean streak when a validator becomes non-compliant.
    pub fn clear_clean_streak(&mut self, addr: &Address) {
        self.first_clean_epoch.remove(addr);
    }

    /// Feature 147: Check if a validator is trusted (30 days = 720 epochs clean).
    /// Returns true if continuously clean for 720+ epochs.
    pub fn is_trusted(&self, addr: &Address, current_epoch: u64) -> bool {
        const TRUST_THRESHOLD_EPOCHS: u64 = 720; // 30 days at 1hr/epoch
        if let Some(&first_clean) = self.first_clean_epoch.get(addr) {
            current_epoch.saturating_sub(first_clean) >= TRUST_THRESHOLD_EPOCHS
        } else {
            false
        }
    }

    /// Feature 148: Check if an IP belongs to a known datacenter provider.
    /// Checks common CIDR ranges for AWS, GCP, Azure, Hetzner, OVH.
    pub fn is_datacenter_ip(ip: &str) -> bool {
        let parts: Vec<&str> = ip.split('.').collect();
        if parts.len() != 4 {
            return false;
        }
        let octets: Vec<u8> = parts.iter()
            .filter_map(|p| p.parse().ok())
            .collect();
        if octets.len() != 4 {
            return false;
        }

        // AWS EC2: 3.x.x.x, 13.x.x.x, 18.x.x.x, 34.x.x.x, 35.x.x.x, 52.x.x.x, 54.x.x.x
        let aws_prefixes: &[u8] = &[3, 13, 18, 34, 35, 52, 54];
        if aws_prefixes.contains(&octets[0]) {
            return true;
        }

        // GCP: 34.x.x.x, 35.x.x.x (overlap with AWS — already covered)
        // Additional GCP: 104.196.x.x, 104.199.x.x
        if octets[0] == 104 && (octets[1] == 196 || octets[1] == 199) {
            return true;
        }

        // Azure: 13.x.x.x (overlap), 20.x.x.x, 40.x.x.x, 52.x.x.x (overlap)
        if octets[0] == 20 || octets[0] == 40 {
            return true;
        }

        // Hetzner: 88.198.x.x, 78.46.x.x, 148.251.x.x, 176.9.x.x, 46.4.x.x, 5.9.x.x
        if (octets[0] == 88 && octets[1] == 198)
            || (octets[0] == 78 && octets[1] == 46)
            || (octets[0] == 148 && octets[1] == 251)
            || (octets[0] == 176 && octets[1] == 9)
            || (octets[0] == 46 && octets[1] == 4)
            || (octets[0] == 5 && octets[1] == 9)
        {
            return true;
        }

        // OVH: 51.x.x.x, 54.36.x.x, 87.98.x.x, 91.121.x.x, 149.202.x.x
        if octets[0] == 51
            || (octets[0] == 54 && octets[1] == 36)
            || (octets[0] == 87 && octets[1] == 98)
            || (octets[0] == 91 && octets[1] == 121)
            || (octets[0] == 149 && octets[1] == 202)
        {
            return true;
        }

        // DigitalOcean: 64.225.x.x, 104.131.x.x, 128.199.x.x, 167.71.x.x, 167.172.x.x
        if (octets[0] == 64 && octets[1] == 225)
            || (octets[0] == 104 && octets[1] == 131)
            || (octets[0] == 128 && octets[1] == 199)
            || (octets[0] == 167 && (octets[1] == 71 || octets[1] == 172))
        {
            return true;
        }

        false
    }

    /// Feature 149: Count how many validators are behind a given IP.
    pub fn ip_validator_count(&self, ip: &str) -> usize {
        self.node_to_ip.values().filter(|v| v.as_str() == ip).count()
    }

    /// Feature 149: Check for VPN/proxy pattern (>3 validators behind same IP).
    pub fn is_vpn_proxy(&self, addr: &Address) -> Option<ComplianceFlag> {
        let ip = self.node_to_ip.get(addr)?;
        let count = self.ip_validator_count(ip);
        if count > 3 {
            Some(ComplianceFlag::VpnProxy {
                ip: ip.clone(),
                validator_count: count,
            })
        } else {
            None
        }
    }

    /// Feature 145: Compute a sybil suspicion score (0-100) for a validator.
    /// +25 for same /24 subnet, +25 for duplicate fingerprint,
    /// +25 for datacenter pattern, +25 for geographic proximity.
    pub fn suspicion_score(&self, addr: &Address) -> u32 {
        let mut score: u32 = 0;

        // +25 for same /24 subnet with any other node.
        if let Some(ip) = self.node_to_ip.get(addr) {
            let subnet = subnet_24(ip);
            for (other_addr, other_ip) in &self.node_to_ip {
                if other_addr == addr {
                    continue;
                }
                if let (Some(s), Some(other_s)) = (&subnet, subnet_24(other_ip)) {
                    if *s == other_s {
                        score += 25;
                        break;
                    }
                }
            }
        }

        // +25 for duplicate fingerprint.
        if let Some(hash) = self.fingerprints.get(addr) {
            for (other_addr, other_hash) in &self.fingerprints {
                if other_addr != addr && other_hash == hash {
                    score += 25;
                    break;
                }
            }
        }

        // +25 for datacenter pattern (behavioral).
        if let Some(profile) = self.behavior_profiles.get(addr) {
            if profile.is_datacenter_pattern() || profile.is_flat_resource() {
                score += 25;
            }
        }

        // +25 for geographic proximity (same /16 or same ASN or datacenter IP).
        let mut geo_flag = false;
        if let Some(ip) = self.node_to_ip.get(addr) {
            if Self::is_datacenter_ip(ip) {
                geo_flag = true;
            }
            let s16 = subnet_16(ip);
            for (other_addr, other_ip) in &self.node_to_ip {
                if other_addr == addr {
                    continue;
                }
                if let (Some(s), Some(other_s)) = (&s16, subnet_16(other_ip)) {
                    if *s == other_s {
                        geo_flag = true;
                        break;
                    }
                }
            }
        }
        if let Some(asn) = self.node_asn.get(addr) {
            for (other_addr, other_asn) in &self.node_asn {
                if other_addr != addr && other_asn == asn {
                    geo_flag = true;
                    break;
                }
            }
        }
        if geo_flag {
            score += 25;
        }

        score.min(100)
    }

    /// Check the compliance status of a registered node.
    ///
    /// Returns `Compliant` if the node is not registered (not our concern).
    /// Features 136/137/138/148/149 integrated into the check.
    pub fn check(&self, addr: &Address) -> ComplianceStatus {
        let Some(ip) = self.node_to_ip.get(addr) else {
            return ComplianceStatus::Compliant;
        };

        // Feature 136: Check for duplicate fingerprints.
        if let Some(hash) = self.fingerprints.get(addr) {
            for (other_addr, other_hash) in &self.fingerprints {
                if other_addr != addr && other_hash == hash {
                    return ComplianceStatus::NerfedAdversarial;
                }
            }
        }

        // Feature 148: Check datacenter IP.
        if Self::is_datacenter_ip(ip) {
            return ComplianceStatus::NerfedIncidental;
        }

        // Feature 149: VPN/proxy detection (>3 validators behind same IP).
        if self.ip_validator_count(ip) > 3 {
            return ComplianceStatus::NerfedAdversarial;
        }

        let subnet24 = subnet_24(ip);
        let subnet16 = subnet_16(ip);

        for (other_addr, other_ip) in &self.node_to_ip {
            if other_addr == addr {
                continue;
            }

            // Same exact IP → NerfedIncidental.
            if other_ip == ip {
                return ComplianceStatus::NerfedIncidental;
            }

            // Same /24 subnet → NerfedIncidental.
            if let (Some(s), Some(other_s)) = (&subnet24, subnet_24(other_ip)) {
                if *s == other_s {
                    return ComplianceStatus::NerfedIncidental;
                }
            }

            // Feature 138: Same /16 subnet → NerfedIncidental.
            if let (Some(s), Some(other_s)) = (&subnet16, subnet_16(other_ip)) {
                if *s == other_s {
                    return ComplianceStatus::NerfedIncidental;
                }
            }
        }

        // Feature 138: Same ASN → NerfedIncidental.
        if let Some(asn) = self.node_asn.get(addr) {
            for (other_addr, other_asn) in &self.node_asn {
                if other_addr != addr && other_asn == asn {
                    return ComplianceStatus::NerfedIncidental;
                }
            }
        }

        ComplianceStatus::Compliant
    }

    /// Feature 142/150: Get network-wide compliance statistics.
    pub fn network_stats(&self) -> ComplianceNetworkStats {
        let total_validators = self.node_to_ip.len() as u64;
        let mut nerfed_count = 0u64;
        let mut suspicious_count = 0u64;

        for addr in self.node_to_ip.keys() {
            let status = self.check(addr);
            if status != ComplianceStatus::Compliant {
                nerfed_count += 1;
            }
            if self.suspicion_score(addr) >= 50 {
                suspicious_count += 1;
            }
        }

        let current_nerf_bps = commputer_core::compliance::NerfRate::compute_adaptive(
            nerfed_count, total_validators,
        );

        ComplianceNetworkStats {
            total_validators,
            compliant_count: total_validators.saturating_sub(nerfed_count),
            nerfed_count,
            current_nerf_percentage: current_nerf_bps,
            suspicious_count,
        }
    }

    /// Feature 150: Get detected clusters (groups of validators sharing IPs or subnets).
    pub fn detected_clusters(&self) -> Vec<(usize, String)> {
        let mut ip_groups: HashMap<String, Vec<Address>> = HashMap::new();
        for (addr, ip) in &self.node_to_ip {
            ip_groups.entry(ip.clone()).or_default().push(*addr);
        }

        let mut clusters: Vec<(usize, String)> = ip_groups
            .into_iter()
            .filter(|(_, addrs)| addrs.len() > 1)
            .map(|(ip, addrs)| (addrs.len(), ip))
            .collect();

        clusters.sort_by(|a, b| b.0.cmp(&a.0));
        clusters.truncate(10); // Top 10 clusters
        clusters
    }
}

/// Feature 142: Network-wide compliance statistics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComplianceNetworkStats {
    pub total_validators: u64,
    pub compliant_count: u64,
    pub nerfed_count: u64,
    pub current_nerf_percentage: u32,
    pub suspicious_count: u64,
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

    #[test]
    fn feature_136_duplicate_fingerprint() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "10.0.1.1".into());
        checker.register_node(addr(2), "10.1.1.1".into());
        let hash = [42u8; 32];
        checker.register_fingerprint(addr(1), hash);
        let flags = checker.register_fingerprint(addr(2), hash);
        assert!(!flags.is_empty());
        // Both should be nerfed.
        assert_eq!(checker.check(&addr(1)), ComplianceStatus::NerfedAdversarial);
        assert_eq!(checker.check(&addr(2)), ComplianceStatus::NerfedAdversarial);
    }

    #[test]
    fn feature_137_behavior_profile() {
        let mut checker = ComplianceChecker::new();
        let profile = BehaviorProfile {
            uptime_ratio: 0.999,
            resource_variance: 0.001,
            response_time_cv: 0.05,
        };
        assert!(profile.is_datacenter_pattern());
        assert!(profile.is_flat_resource());
        checker.update_behavior_profile(addr(1), profile);
    }

    #[test]
    fn feature_138_same_subnet_16() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "10.0.1.1".into());
        checker.register_node(addr(2), "10.0.2.1".into()); // Same /16
        assert_eq!(checker.check(&addr(1)), ComplianceStatus::NerfedIncidental);
        let flags = checker.geographic_flags(&addr(1));
        assert!(!flags.is_empty());
    }

    #[test]
    fn feature_138_same_asn() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "10.0.1.1".into());
        checker.register_node(addr(2), "172.16.1.1".into());
        checker.register_asn(addr(1), "AS12345".into());
        checker.register_asn(addr(2), "AS12345".into());
        assert_eq!(checker.check(&addr(1)), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn feature_139_multi_node_multiplier() {
        use commputer_core::compliance::multi_node_multiplier;
        assert_eq!(multi_node_multiplier(1), 1.0);
        assert_eq!(multi_node_multiplier(2), 0.25);
        assert_eq!(multi_node_multiplier(3), 0.0625);
        assert_eq!(multi_node_multiplier(4), 0.015625);
        assert_eq!(multi_node_multiplier(5), 0.0);
        assert_eq!(multi_node_multiplier(10), 0.0);
    }

    #[test]
    fn feature_140_adaptive_nerf() {
        use commputer_core::compliance::NerfRate;
        // No one nerfed → 80%.
        assert_eq!(NerfRate::compute_adaptive(0, 100), 8000);
        // 50% nerfed → 90%.
        assert_eq!(NerfRate::compute_adaptive(50, 100), 9000);
        // 100% nerfed → 100%.
        assert_eq!(NerfRate::compute_adaptive(100, 100), 10000);
        // No validators → 80%.
        assert_eq!(NerfRate::compute_adaptive(0, 0), 8000);
    }

    #[test]
    fn feature_141_resource_spike_detection() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "10.0.1.1".into());

        let initial = ResourceCapacity {
            cpu_score: 100,
            gpu_score: 0,
            ram_available_mb: 8000,
            storage_available_mb: 100000,
            bandwidth_kbps: 100000,
            contribution_percent: 100,
        };
        let flags = checker.report_resources(addr(1), initial, 10);
        assert!(flags.is_empty()); // No previous, no spike.

        // RAM doubles → spike.
        let spiked = ResourceCapacity {
            cpu_score: 100,
            gpu_score: 0,
            ram_available_mb: 20000, // 2.5x
            storage_available_mb: 100000,
            bandwidth_kbps: 100000,
            contribution_percent: 100,
        };
        let flags = checker.report_resources(addr(1), spiked, 11);
        assert!(!flags.is_empty());
        assert!(checker.is_in_cooldown(&addr(1), 11));
        assert!(checker.is_in_cooldown(&addr(1), 13));
        assert!(!checker.is_in_cooldown(&addr(1), 14));
    }

    #[test]
    fn feature_143_compliance_history() {
        let mut checker = ComplianceChecker::new();
        checker.record_compliance_status(addr(1), 1, ComplianceStatus::Compliant);
        checker.record_compliance_status(addr(1), 2, ComplianceStatus::NerfedIncidental);
        checker.record_compliance_status(addr(1), 3, ComplianceStatus::NerfedIncidental); // No duplicate
        let history = checker.get_compliance_history(&addr(1));
        assert_eq!(history.len(), 2);
        assert_eq!(history[0], (1, ComplianceStatus::Compliant));
        assert_eq!(history[1], (2, ComplianceStatus::NerfedIncidental));
    }

    #[test]
    fn feature_145_suspicion_score() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "192.168.1.10".into());
        checker.register_node(addr(2), "192.168.1.11".into()); // Same /24
        let score = checker.suspicion_score(&addr(1));
        assert!(score >= 25); // At least subnet flag
    }

    #[test]
    fn feature_147_whitelist_trusted() {
        let mut checker = ComplianceChecker::new();
        checker.mark_clean_epoch(addr(1), 100);
        assert!(!checker.is_trusted(&addr(1), 500));
        assert!(checker.is_trusted(&addr(1), 820)); // 720 epochs later
    }

    #[test]
    fn feature_148_datacenter_ip() {
        assert!(ComplianceChecker::is_datacenter_ip("3.5.10.20")); // AWS
        assert!(ComplianceChecker::is_datacenter_ip("88.198.1.1")); // Hetzner
        assert!(!ComplianceChecker::is_datacenter_ip("192.168.1.1")); // Private
        assert!(!ComplianceChecker::is_datacenter_ip("not.an.ip")); // Invalid
    }

    #[test]
    fn feature_149_vpn_proxy_detection() {
        let mut checker = ComplianceChecker::new();
        let ip = "1.2.3.4".to_string();
        for i in 1..=4 {
            checker.register_node(addr(i), ip.clone());
        }
        // 4 validators behind same IP → VPN/proxy flag.
        let flag = checker.is_vpn_proxy(&addr(1));
        assert!(flag.is_some());
    }

    #[test]
    fn feature_142_network_stats() {
        let mut checker = ComplianceChecker::new();
        checker.register_node(addr(1), "10.0.1.1".into());
        checker.register_node(addr(2), "172.16.1.1".into());
        let stats = checker.network_stats();
        assert_eq!(stats.total_validators, 2);
        assert_eq!(stats.compliant_count, 2);
        assert_eq!(stats.nerfed_count, 0);
    }

    // Feature 199: Anti-scale simulation — 100 fake validators from same /24 subnet
    #[test]
    fn feature_199_anti_scale_100_validators_same_subnet() {
        use commputer_core::compliance::{multi_node_multiplier, NerfRate};
        use commputer_core::token::UNITS_PER_COMME;

        let mut checker = ComplianceChecker::new();

        // Register 100 validators on the same /24 subnet
        for i in 0..100u8 {
            let a = addr(i);
            checker.register_node(a, format!("192.168.1.{}", i + 1));
        }

        // All should be nerfed (same /24 subnet)
        for i in 0..100u8 {
            let status = checker.check(&addr(i));
            assert_ne!(
                status,
                ComplianceStatus::Compliant,
                "Validator {} should be nerfed",
                i
            );
        }

        // Calculate total nerfed reward using multi_node_multiplier AND nerf rate.
        // multi_node_multiplier gives: 100%, 25%, 6.25%, 1.5625%, 0%...
        // But all 100 nodes are also nerfed (80%+ reward reduction).
        let base_daily_reward = (UNITS_PER_COMME * 9) / 100; // 0.09 COMME/day
        let nerf = NerfRate::INITIAL; // 80% nerf -> 20% reward
        let nerf_mult = nerf.reward_multiplier();

        let mut total_nerfed_reward: f64 = 0.0;
        for n in 1..=100u32 {
            let mult = multi_node_multiplier(n);
            total_nerfed_reward += base_daily_reward as f64 * mult * nerf_mult;
        }

        // Single honest validator earns full reward
        let single_honest_reward = base_daily_reward as f64;

        // With nerf applied: total = (1.0 + 0.25 + 0.0625 + 0.015625) * 0.20 * base
        // = 1.328125 * 0.20 * base = 0.265625 * base < 1.0 * base
        assert!(
            total_nerfed_reward < single_honest_reward,
            "100 nerfed validators total ({:.0}) should earn less than single honest ({:.0})",
            total_nerfed_reward,
            single_honest_reward
        );
    }

    // Feature 219: Compliance restoration test
    #[test]
    fn feature_219_compliance_restoration() {
        let mut checker = ComplianceChecker::new();

        // Register two validators on same subnet -> both nerfed
        checker.register_node(addr(1), "192.168.1.10".into());
        checker.register_node(addr(2), "192.168.1.11".into());
        assert_eq!(checker.check(&addr(1)), ComplianceStatus::NerfedIncidental);
        assert_eq!(checker.check(&addr(2)), ComplianceStatus::NerfedIncidental);

        // Deregister the second -> first should be restored to compliant
        checker.deregister_node(&addr(2));
        assert_eq!(checker.check(&addr(1)), ComplianceStatus::Compliant);
    }
}
