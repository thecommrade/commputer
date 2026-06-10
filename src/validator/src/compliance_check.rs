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

/// W5.3 / whitepaper anti-scale: returns true iff `addr` falls inside `prefix/len`.
/// `prefix` is a textual IPv6 string parseable by `Ipv6Addr::from_str`.
/// `len` is in bits, 0..=128. Out-of-range len returns false.
fn ipv6_in_prefix(addr: std::net::Ipv6Addr, prefix: &str, len: u8) -> bool {
    use std::net::Ipv6Addr;
    if len > 128 {
        return false;
    }
    let net: Ipv6Addr = match prefix.parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    let addr_bits = u128::from_be_bytes(addr.octets());
    let net_bits = u128::from_be_bytes(net.octets());
    if len == 0 {
        return true;
    }
    let mask: u128 = (!0u128) << (128 - len);
    (addr_bits & mask) == (net_bits & mask)
}

/// A5-cidr-tighten: IPv4 analogue of `ipv6_in_prefix`. Returns true iff `addr`
/// (the 4 octets of an IPv4 address) falls inside `net/len`. `net` is a dotted
/// quad parseable by `Ipv4Addr::from_str` (network address, host bits zero).
/// Out-of-range `len` or an unparseable `net` returns false; `len == 0` matches
/// everything (guards the `<< 32` host-shift hazard, exactly as the v6 helper
/// guards `<< 128`).
fn ipv4_in_prefix(addr: [u8; 4], net: &str, len: u8) -> bool {
    use std::net::Ipv4Addr;
    if len > 32 {
        return false;
    }
    let net_addr: Ipv4Addr = match net.parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    let addr_bits = u32::from_be_bytes(addr);
    let net_bits = u32::from_be_bytes(net_addr.octets());
    if len == 0 {
        return true;
    }
    let mask: u32 = (!0u32) << (32 - len);
    (addr_bits & mask) == (net_bits & mask)
}

/// A5-cidr-tighten: CIDR-precise datacenter/cloud IPv4 prefix table grounded in
/// real BGP-announced ranges (RIPEstat AS24940 Hetzner / AS16276 OVH / AS14061
/// DigitalOcean, verified 2026-06-10; Google cloud.json for GCP). Replaces the
/// legacy coarse first-octet table. Each row is (network, prefix_len, label);
/// first match wins. AWS/Azure/GCP keep the accepted coarse heuristic re-
/// expressed as CIDRs; Hetzner is broadened to its real modern /16s and OVH is
/// tightened from the wrong whole-51/8 to its 18 announced blocks.
const DATACENTER_V4_PREFIXES: &[(&str, u8, &str)] = &[
    // ----- AWS (AS16509 / AS14618) — coarse /8 heuristic, preserved ----
    ("3.0.0.0", 8, "AWS"),
    ("13.0.0.0", 8, "AWS"),
    ("18.0.0.0", 8, "AWS"),
    ("34.0.0.0", 8, "AWS"),  // also GCP
    ("35.0.0.0", 8, "AWS"),  // also GCP
    ("52.0.0.0", 8, "AWS"),
    ("54.0.0.0", 8, "AWS"),  // OVH 54.36/14 sits inside; OVH rows below are
                             // redundant for the bool but kept for labels.

    // ----- Azure (AS8075) — coarse /8 heuristic, preserved -------------
    ("20.0.0.0", 8, "Azure"),
    ("40.0.0.0", 8, "Azure"),

    // ----- GCP (AS15169 / AS396982) — confirmed cloud.json aggregates --
    ("104.196.0.0", 14, "GCP"),
    ("104.154.0.0", 15, "GCP"),
    ("104.197.0.0", 16, "GCP"),
    ("130.211.0.0", 16, "GCP"),
    ("35.184.0.0", 13, "GCP"),   // also covered by 35/8 AWS row; label clarity

    // ----- Hetzner (AS24940) — BROADENED to real announced /15-/17 -----
    ("88.198.0.0", 16, "Hetzner"),   // kept (already correct)
    ("88.99.0.0", 16, "Hetzner"),    // newly caught
    ("49.12.0.0", 16, "Hetzner"),    // newly caught
    ("49.13.0.0", 16, "Hetzner"),    // newly caught
    ("65.21.0.0", 16, "Hetzner"),    // newly caught
    ("65.108.0.0", 16, "Hetzner"),   // newly caught
    ("65.109.0.0", 16, "Hetzner"),   // newly caught
    ("95.216.0.0", 16, "Hetzner"),   // newly caught
    ("95.217.0.0", 16, "Hetzner"),   // newly caught
    ("116.202.0.0", 16, "Hetzner"),  // newly caught
    ("116.203.0.0", 16, "Hetzner"),  // newly caught
    ("167.233.0.0", 16, "Hetzner"),  // newly caught
    ("167.235.0.0", 16, "Hetzner"),  // newly caught
    ("168.119.0.0", 16, "Hetzner"),  // newly caught
    ("78.46.0.0", 15, "Hetzner"),    // was 78.46/16 in old code; real is /15
    ("148.251.0.0", 16, "Hetzner"),  // kept (correct)
    ("176.9.0.0", 16, "Hetzner"),    // kept (correct)
    ("5.9.0.0", 16, "Hetzner"),      // kept (correct)
    ("46.4.0.0", 16, "Hetzner"),     // was 46.4/16 in old code (correct)
    ("46.224.0.0", 15, "Hetzner"),   // newly caught aggregate
    ("5.75.128.0", 17, "Hetzner"),   // newly caught (Hetzner cloud)

    // ----- OVH (AS16276) — TIGHTENED from whole-51/8 to 18 real blocks -
    ("51.38.0.0", 16, "OVH"),
    ("51.68.0.0", 16, "OVH"),
    ("51.75.0.0", 16, "OVH"),
    ("51.77.0.0", 16, "OVH"),
    ("51.79.0.0", 16, "OVH"),
    ("51.81.0.0", 16, "OVH"),
    ("51.83.0.0", 16, "OVH"),
    ("51.89.0.0", 16, "OVH"),
    ("51.91.0.0", 16, "OVH"),
    ("51.161.0.0", 16, "OVH"),
    ("51.178.0.0", 16, "OVH"),
    ("51.195.0.0", 16, "OVH"),
    ("51.210.0.0", 16, "OVH"),
    ("51.222.0.0", 16, "OVH"),
    ("51.254.0.0", 15, "OVH"),   // 51.254.0.0 - 51.255.255.255
    ("54.36.0.0", 14, "OVH"),    // 54.36.0.0 - 54.39.255.255 (inside AWS 54/8)
    ("87.98.0.0", 16, "OVH"),    // mild over-cover; RIPEstat confirmed
                                 // 87.98.128.0/17 — TODO(cidr) tighten if strict
    ("91.121.0.0", 16, "OVH"),
    ("149.202.0.0", 16, "OVH"),
    ("145.239.0.0", 16, "OVH"),  // newly caught
    ("137.74.0.0", 16, "OVH"),   // newly caught
    ("141.94.0.0", 16, "OVH"),   // newly caught
    ("141.95.0.0", 16, "OVH"),   // newly caught
    ("178.32.0.0", 15, "OVH"),   // newly caught aggregate
    ("188.165.0.0", 16, "OVH"),  // newly caught
    ("5.135.0.0", 16, "OVH"),    // newly caught
    ("5.196.0.0", 16, "OVH"),    // newly caught
    ("92.222.0.0", 16, "OVH"),   // newly caught
    ("94.23.0.0", 16, "OVH"),    // newly caught
    ("213.32.0.0", 17, "OVH"),   // newly caught
    ("5.39.0.0", 17, "OVH"),     // newly caught

    // ----- DigitalOcean (AS14061) — fully-covered /16s -----------------
    ("64.225.0.0", 16, "DigitalOcean"),   // kept
    ("104.131.0.0", 16, "DigitalOcean"),  // kept
    ("128.199.0.0", 16, "DigitalOcean"),  // kept
    ("167.71.0.0", 16, "DigitalOcean"),   // kept
    ("167.172.0.0", 16, "DigitalOcean"),  // kept
    ("134.122.0.0", 16, "DigitalOcean"),  // newly caught
    ("137.184.0.0", 16, "DigitalOcean"),  // newly caught
    ("138.197.0.0", 16, "DigitalOcean"),  // newly caught
    ("138.68.0.0", 16, "DigitalOcean"),   // newly caught
    ("142.93.0.0", 16, "DigitalOcean"),   // newly caught
    ("143.198.0.0", 16, "DigitalOcean"),  // newly caught
    ("146.190.0.0", 16, "DigitalOcean"),  // newly caught
    ("157.230.0.0", 16, "DigitalOcean"),  // newly caught
    ("159.65.0.0", 16, "DigitalOcean"),   // newly caught
    ("159.89.0.0", 16, "DigitalOcean"),   // newly caught
    ("161.35.0.0", 16, "DigitalOcean"),   // newly caught
    ("165.22.0.0", 16, "DigitalOcean"),   // newly caught
    ("165.227.0.0", 16, "DigitalOcean"),  // newly caught
    ("206.189.0.0", 16, "DigitalOcean"),  // newly caught
    ("64.227.0.0", 16, "DigitalOcean"),   // newly caught
];

/// A5-cidr-tighten: returns the matching provider label for an IPv4 address, or
/// `None`. `is_datacenter_ipv4` only needs the bool; the label aids diagnostics.
fn match_datacenter_ipv4(octets: [u8; 4]) -> Option<&'static str> {
    for (net, len, label) in DATACENTER_V4_PREFIXES {
        if ipv4_in_prefix(octets, net, *len) {
            return Some(label);
        }
    }
    None
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
    /// Create an empty compliance checker.
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
            if let (Some(s), Some(other_s)) = (&subnet16, subnet_16(other_ip))
                && *s == other_s {
                    flags.push(ComplianceFlag::SameSubnet16 {
                        peer_address: *other_addr,
                    });
                    flags.push(ComplianceFlag::GeographicProximity {
                        peer_address: *other_addr,
                    });
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
    ///
    /// Accepts both IPv4 dotted-quad and IPv6 colon-hex strings. IPv4 hits
    /// the legacy octet-prefix table; IPv6 hits the `IPV6_DC_PREFIXES`
    /// aggregate table in `is_datacenter_ipv6`. Anything that doesn't parse
    /// as either returns false (preserves existing malformed-input behaviour).
    /// Closes the IPv6 anti-scale bypass that previously let every cloud
    /// provider's v6 ranges silently evade detection.
    pub fn is_datacenter_ip(ip: &str) -> bool {
        use std::net::IpAddr;
        match ip.parse::<IpAddr>() {
            Ok(IpAddr::V4(v4)) => Self::is_datacenter_ipv4(v4),
            Ok(IpAddr::V6(v6)) => Self::is_datacenter_ipv6(v6),
            Err(_) => false,
        }
    }

    /// IPv4 datacenter detection — CIDR-precise prefix table (A5-cidr-tighten).
    /// Delegates to the module-level `match_datacenter_ipv4` / `DATACENTER_V4_PREFIXES`
    /// (BGP-grounded). Replaces the legacy coarse octet table that flagged the
    /// whole 51/8 as OVH and missed modern Hetzner /16s. Signature and call site
    /// at `is_datacenter_ip` are unchanged.
    fn is_datacenter_ipv4(addr: std::net::Ipv4Addr) -> bool {
        match_datacenter_ipv4(addr.octets()).is_some()
    }

    /// IPv6 datacenter detection — aggregate prefix table.
    /// Citations: AWS ip-ranges.json, Microsoft ServiceTags_Public.json,
    /// Google goog.json, RIPE/ARIN BGP route objects (AS24940, AS16276,
    /// AS14061). Aggregates intentionally cover provider-owned IANA
    /// allocations; residential ISPs do not announce inside these ranges.
    fn is_datacenter_ipv6(addr: std::net::Ipv6Addr) -> bool {
        const IPV6_DC_PREFIXES: &[(&str, u8)] = &[
            ("2600:1f00::", 24),   // AWS global EC2
            ("2406:da00::", 24),   // AWS APAC EC2
            ("2603:1000::", 24),   // Azure
            ("2620:1ec::",  36),   // Azure edge / front-door
            ("2600:1900::", 28),   // GCP primary
            ("2620:0:1c00::", 40), // Google misc / corp edge
            ("2a01:4f8::",  29),   // Hetzner AS24940
            ("2a01:4f9::",  32),   // Hetzner secondary
            ("2001:41d0::", 32),   // OVH AS16276
            ("2604:a880::", 32),   // DigitalOcean AS14061 (US)
            ("2a03:b0c0::", 32),   // DigitalOcean AS14061 (EU)
        ];
        for (prefix, len) in IPV6_DC_PREFIXES {
            if ipv6_in_prefix(addr, prefix, *len) {
                return true;
            }
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
                if let (Some(s), Some(other_s)) = (&subnet, subnet_24(other_ip))
                    && *s == other_s {
                        score += 25;
                        break;
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
        if let Some(profile) = self.behavior_profiles.get(addr)
            && (profile.is_datacenter_pattern() || profile.is_flat_resource()) {
                score += 25;
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
                if let (Some(s), Some(other_s)) = (&s16, subnet_16(other_ip))
                    && *s == other_s {
                        geo_flag = true;
                        break;
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
            if let (Some(s), Some(other_s)) = (&subnet24, subnet_24(other_ip))
                && *s == other_s {
                    return ComplianceStatus::NerfedIncidental;
                }

            // Feature 138: Same /16 subnet → NerfedIncidental.
            if let (Some(s), Some(other_s)) = (&subnet16, subnet_16(other_ip))
                && *s == other_s {
                    return ComplianceStatus::NerfedIncidental;
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

    // -------------------------------------------------------------------
    // IPv6 anti-scale — closes the cloud-IP bypass. The whitepaper
    // promises "anti-scale enforcement from block one" and "datacenter
    // mining economically suicidal" — the v6 path now matches the v4 path.
    // -------------------------------------------------------------------

    #[test]
    fn ipv6_aws_is_datacenter() {
        // 2600:1f00::/24 — AWS global EC2.
        assert!(ComplianceChecker::is_datacenter_ip("2600:1f00:1000:1::1"));
        // 2406:da00::/24 — AWS APAC.
        assert!(ComplianceChecker::is_datacenter_ip("2406:da00::1"));
    }

    #[test]
    fn ipv6_azure_is_datacenter() {
        assert!(ComplianceChecker::is_datacenter_ip("2603:1000::1"));
        assert!(ComplianceChecker::is_datacenter_ip("2603:10ff:ffff::1"));
    }

    #[test]
    fn ipv6_gcp_is_datacenter() {
        assert!(ComplianceChecker::is_datacenter_ip("2600:1900::1"));
        assert!(ComplianceChecker::is_datacenter_ip("2620:0:1c00::1"));
    }

    #[test]
    fn ipv6_hetzner_is_datacenter() {
        assert!(ComplianceChecker::is_datacenter_ip("2a01:4f8::1"));
        assert!(ComplianceChecker::is_datacenter_ip("2a01:4f9:c010::1"));
    }

    #[test]
    fn ipv6_ovh_is_datacenter() {
        assert!(ComplianceChecker::is_datacenter_ip("2001:41d0:1:abcd::1"));
    }

    #[test]
    fn ipv6_digitalocean_is_datacenter() {
        assert!(ComplianceChecker::is_datacenter_ip("2604:a880::1"));
        assert!(ComplianceChecker::is_datacenter_ip("2a03:b0c0:3::1"));
    }

    #[test]
    fn ipv6_residential_not_datacenter() {
        // ISP allocations outside any cloud aggregate.
        assert!(!ComplianceChecker::is_datacenter_ip("2600::1"));      // sparse
        assert!(!ComplianceChecker::is_datacenter_ip("2001:db8::1"));  // RFC 3849 doc
        assert!(!ComplianceChecker::is_datacenter_ip("2607:f8b0::1")); // legacy edge
    }

    #[test]
    fn ipv6_link_local_and_loopback_not_datacenter() {
        assert!(!ComplianceChecker::is_datacenter_ip("::1"));     // loopback
        assert!(!ComplianceChecker::is_datacenter_ip("fe80::1")); // link-local
        assert!(!ComplianceChecker::is_datacenter_ip("ff02::1")); // multicast
    }

    #[test]
    fn ipv6_unparseable_returns_false() {
        // Same contract as v4 for malformed input.
        assert!(!ComplianceChecker::is_datacenter_ip("not.an.ip"));
        assert!(!ComplianceChecker::is_datacenter_ip(""));
        assert!(!ComplianceChecker::is_datacenter_ip("zzzz::1"));
        assert!(!ComplianceChecker::is_datacenter_ip("2600:1f00::g"));
    }

    #[test]
    fn ipv6_boundary_first_address_of_aws_prefix() {
        // First address of 2600:1f00::/24 is 2600:1f00:: itself.
        assert!(ComplianceChecker::is_datacenter_ip("2600:1f00::"));
    }

    #[test]
    fn ipv6_boundary_last_address_of_aws_prefix() {
        // Last address of 2600:1f00::/24 — host bits all 1.
        assert!(ComplianceChecker::is_datacenter_ip(
            "2600:1fff:ffff:ffff:ffff:ffff:ffff:ffff"
        ));
        // One address past the prefix (2600:2000::) must NOT be flagged.
        assert!(!ComplianceChecker::is_datacenter_ip("2600:2000::"));
    }

    #[test]
    fn ipv4_string_still_works_through_new_parser() {
        // Regression: dotted-quad asserts must still pass after the
        // IpAddr-based rewrite.
        assert!(ComplianceChecker::is_datacenter_ip("3.5.10.20"));
        assert!(ComplianceChecker::is_datacenter_ip("88.198.1.1"));
        assert!(!ComplianceChecker::is_datacenter_ip("192.168.1.1"));
    }

    // ── A5-cidr-tighten: CIDR-precise IPv4 datacenter detector ──
    // `dc` routes through the real public dispatcher (is_datacenter_ip ->
    // is_datacenter_ipv4 -> match_datacenter_ipv4); `label` calls the table
    // helper directly for provider-attribution assertions.
    mod cidr_v4_tests {
        use super::super::*;
        use std::net::Ipv4Addr;
        use std::str::FromStr;

        fn dc(s: &str) -> bool {
            ComplianceChecker::is_datacenter_ip(s)
        }
        fn label(s: &str) -> Option<&'static str> {
            match_datacenter_ipv4(Ipv4Addr::from_str(s).unwrap().octets())
        }

        // ---- ipv4_in_prefix correctness (the matcher itself) ----

        #[test]
        fn prefix_len_out_of_range_is_false() {
            assert!(!ipv4_in_prefix([10, 0, 0, 1], "10.0.0.0", 33));
            assert!(!ipv4_in_prefix([10, 0, 0, 1], "10.0.0.0", 255));
        }

        #[test]
        fn prefix_unparseable_net_is_false() {
            assert!(!ipv4_in_prefix([10, 0, 0, 1], "not.an.ip", 16));
            assert!(!ipv4_in_prefix([10, 0, 0, 1], "", 16));
        }

        #[test]
        fn prefix_len_zero_matches_everything() {
            assert!(ipv4_in_prefix([1, 2, 3, 4], "0.0.0.0", 0));
            assert!(ipv4_in_prefix([255, 255, 255, 255], "0.0.0.0", 0));
        }

        #[test]
        fn prefix_32_is_exact_host_match() {
            assert!(ipv4_in_prefix([51, 68, 1, 1], "51.68.1.1", 32));
            assert!(!ipv4_in_prefix([51, 68, 1, 2], "51.68.1.1", 32));
        }

        #[test]
        fn prefix_16_membership_math() {
            assert!(ipv4_in_prefix([88, 99, 0, 0], "88.99.0.0", 16));
            assert!(ipv4_in_prefix([88, 99, 255, 255], "88.99.0.0", 16));
            assert!(!ipv4_in_prefix([88, 100, 0, 0], "88.99.0.0", 16));
            assert!(!ipv4_in_prefix([88, 98, 255, 255], "88.99.0.0", 16));
        }

        #[test]
        fn prefix_15_spans_two_octet3_values() {
            // 78.46.0.0/15 -> 78.46.0.0 .. 78.47.255.255
            assert!(ipv4_in_prefix([78, 46, 0, 0], "78.46.0.0", 15));
            assert!(ipv4_in_prefix([78, 47, 255, 255], "78.46.0.0", 15));
            assert!(!ipv4_in_prefix([78, 48, 0, 0], "78.46.0.0", 15));
            assert!(!ipv4_in_prefix([78, 45, 255, 255], "78.46.0.0", 15));
        }

        #[test]
        fn prefix_14_spans_four_octet3_values() {
            // 54.36.0.0/14 -> 54.36.0.0 .. 54.39.255.255 (OVH)
            assert!(ipv4_in_prefix([54, 36, 0, 0], "54.36.0.0", 14));
            assert!(ipv4_in_prefix([54, 39, 255, 255], "54.36.0.0", 14));
            assert!(!ipv4_in_prefix([54, 40, 0, 0], "54.36.0.0", 14));
            assert!(!ipv4_in_prefix([54, 35, 255, 255], "54.36.0.0", 14));
        }

        // ---- Each provider: positive hits ----

        #[test]
        fn aws_positive() {
            assert_eq!(label("3.5.10.20"), Some("AWS"));
            assert_eq!(label("18.200.1.1"), Some("AWS"));
            assert_eq!(label("52.95.1.1"), Some("AWS"));
            assert!(dc("54.240.1.1"));
        }

        #[test]
        fn azure_positive() {
            assert_eq!(label("20.1.2.3"), Some("Azure"));
            assert_eq!(label("40.9.9.9"), Some("Azure"));
        }

        #[test]
        fn gcp_positive() {
            assert!(dc("104.196.1.1"));
            assert!(dc("104.154.5.5"));
            assert!(dc("130.211.1.1"));
            assert!(dc("34.64.1.1"));
            assert!(dc("35.184.1.1"));
        }

        #[test]
        fn hetzner_positive_legacy_kept() {
            assert_eq!(label("88.198.1.1"), Some("Hetzner"));
            assert_eq!(label("5.9.1.1"), Some("Hetzner"));
            assert_eq!(label("176.9.1.1"), Some("Hetzner"));
            assert_eq!(label("148.251.1.1"), Some("Hetzner"));
            assert_eq!(label("46.4.1.1"), Some("Hetzner"));
        }

        #[test]
        fn ovh_positive_legacy_kept() {
            assert_eq!(label("91.121.1.1"), Some("OVH"));
            assert_eq!(label("149.202.1.1"), Some("OVH"));
        }

        #[test]
        fn digitalocean_positive_legacy_kept() {
            assert_eq!(label("64.225.1.1"), Some("DigitalOcean"));
            assert_eq!(label("104.131.1.1"), Some("DigitalOcean"));
            assert_eq!(label("128.199.1.1"), Some("DigitalOcean"));
            assert_eq!(label("167.71.1.1"), Some("DigitalOcean"));
            assert_eq!(label("167.172.1.1"), Some("DigitalOcean"));
        }

        // ---- BUG FIX 1: previously-missed Hetzner ranges now caught ----

        #[test]
        fn hetzner_previously_missed_now_caught() {
            for ip in [
                "88.99.1.1", "49.12.1.1", "49.13.1.1", "65.21.1.1", "65.108.1.1",
                "65.109.1.1", "95.216.1.1", "95.217.1.1", "116.202.1.1",
                "116.203.1.1", "167.233.1.1", "167.235.1.1", "168.119.1.1",
            ] {
                assert_eq!(label(ip), Some("Hetzner"), "{} should be Hetzner now", ip);
                assert!(dc(ip), "{} should be datacenter now", ip);
            }
        }

        #[test]
        fn hetzner_78_46_now_full_slash15() {
            assert!(dc("78.46.5.5"));
            assert!(dc("78.47.5.5"));   // newly caught half of the /15
            assert!(!dc("78.48.5.5"));  // just past the /15 — residential
            assert!(!dc("78.45.5.5"));  // just before — residential
        }

        // ---- BUG FIX 2: OVH tightened, non-OVH 51.x NOT flagged ----

        #[test]
        fn ovh_real_blocks_flagged() {
            for ip in [
                "51.38.1.1", "51.68.1.1", "51.75.1.1", "51.77.1.1", "51.79.200.1",
                "51.81.200.1", "51.83.1.1", "51.89.1.1", "51.91.1.1", "51.161.200.1",
                "51.178.1.1", "51.195.1.1", "51.210.1.1", "51.222.1.1", "51.254.1.1",
                "51.255.1.1",
            ] {
                assert_eq!(label(ip), Some("OVH"), "{} should be OVH", ip);
            }
        }

        #[test]
        fn non_ovh_51x_not_flagged() {
            // THE headline regression: old `octets[0] == 51` flagged ALL of 51/8.
            for ip in [
                "51.100.5.5", "51.0.0.1", "51.1.2.3", "51.15.1.1", "51.158.1.1",
                "51.69.1.1", "51.200.1.1", "51.253.1.1",
            ] {
                assert_eq!(label(ip), None, "{} must NOT be flagged (not OVH)", ip);
                assert!(!dc(ip), "{} must NOT be datacenter", ip);
            }
        }

        #[test]
        fn ovh_51_254_slash15_boundary() {
            assert!(dc("51.254.0.0"));
            assert!(dc("51.255.255.255"));
            assert!(!dc("51.253.255.255"));
        }

        #[test]
        fn ovh_new_non51_blocks_flagged() {
            for ip in [
                "145.239.1.1", "137.74.1.1", "141.94.1.1", "141.95.200.1",
                "178.32.5.5", "178.33.5.5", "188.165.1.1", "92.222.1.1", "94.23.1.1",
            ] {
                assert_eq!(label(ip), Some("OVH"), "{} should be OVH", ip);
            }
        }

        // ---- DigitalOcean newly-caught /16s ----

        #[test]
        fn digitalocean_new_blocks_flagged() {
            for ip in [
                "134.122.1.1", "137.184.1.1", "138.197.1.1", "138.68.1.1",
                "142.93.1.1", "146.190.1.1", "157.230.1.1", "159.65.1.1",
                "159.89.1.1", "161.35.1.1", "165.22.1.1", "165.227.1.1",
                "206.189.1.1", "64.227.1.1",
            ] {
                assert_eq!(label(ip), Some("DigitalOcean"), "{} should be DO", ip);
            }
        }

        #[test]
        fn digitalocean_unclaimed_partial_16s_not_overclaimed() {
            // 143.110/16 and 64.23/16 are intentionally left out (only partially
            // DO-owned). Documents current behavior so a future broadening is a
            // conscious decision, not an accident.
            assert_eq!(label("143.110.1.1"), None);
            assert_eq!(label("64.23.1.1"), None);
        }

        // ---- Residential / private negatives ----

        #[test]
        fn residential_and_private_not_flagged() {
            for ip in [
                "192.168.1.1", "10.0.1.1", "172.16.1.1", "71.12.34.56", "24.1.1.1",
                "86.1.2.3", "100.64.0.1", "127.0.0.1", "8.8.8.8",
            ] {
                assert!(!dc(ip), "{} must NOT be datacenter", ip);
            }
        }

        // ---- CIDR boundary tests on representative provider blocks ----

        #[test]
        fn hetzner_88_99_slash16_boundaries() {
            assert!(dc("88.99.0.0"));
            assert!(dc("88.99.255.255"));
            assert!(!dc("88.100.0.0"));
            assert!(!dc("88.98.255.255"));
        }

        #[test]
        fn ovh_51_68_slash16_boundaries() {
            assert!(dc("51.68.0.0"));
            assert!(dc("51.68.255.255"));
            assert!(!dc("51.67.255.255"));
            assert!(!dc("51.69.0.0"));
        }

        #[test]
        fn aws_54_8_boundary_does_not_leak_to_55() {
            assert!(dc("54.0.0.0"));
            assert!(dc("54.255.255.255"));
            assert!(!dc("55.0.0.0"));        // 55/8 is DoD, not AWS
            assert!(!dc("53.255.255.255"));  // 53/8 not in table
        }

        // ---- Parity guard: the exact assertions the shipping tests rely on ----

        #[test]
        fn shipping_feature_148_assertions_still_hold() {
            assert!(dc("3.5.10.20"));      // AWS — still true
            assert!(dc("88.198.1.1"));     // Hetzner — still true
            assert!(!dc("192.168.1.1"));   // private — still false
        }
    }
}
