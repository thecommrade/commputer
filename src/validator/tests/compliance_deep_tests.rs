//! Tier B (B-5) — compliance checker deep tests.
//!
//! Complements the in-module unit tests (which cover the IPv4 CIDR table and
//! the loopback exemption) with coverage of the IPv6 datacenter path, the
//! colocation / VPN / adversarial verdicts, the loopback exemption against the
//! VPN path, and suspicion-score bounds.
//!
//! New file, zero runtime behavior change. (Roadmap: src/staging/docs/wirein_roadmap.md B-5.)

use commputer_core::compliance::ComplianceStatus;
use commputer_core::identity::Address;
use commputer_validator::ComplianceChecker;
use proptest::prelude::*;

fn a(n: u8) -> Address {
    Address([n; 32])
}

// ── IPv6 datacenter detection (the IPv4 CIDR work did not cover this path) ──

#[test]
fn ipv6_datacenter_prefixes_flagged() {
    for ip in [
        "2600:1f00:1234::1",   // AWS global EC2 /24
        "2406:da00:1::1",      // AWS APAC /24
        "2603:1000:abcd::1",   // Azure /24
        "2620:1ec::abcd",      // Azure edge/front-door /36
        "2600:1900:0:1::1",    // GCP /28
        "2620:0:1c00::1",      // Google misc /40
        "2a01:4f8:dead::1",    // Hetzner /29
        "2a01:4f9:beef::1",    // Hetzner /32
        "2001:41d0:1:abcd::1", // OVH /32
        "2604:a880:2::1",      // DigitalOcean US /32
        "2a03:b0c0:3::1",      // DigitalOcean EU /32
    ] {
        assert!(
            ComplianceChecker::is_datacenter_ip(ip),
            "{ip} should be detected as datacenter (IPv6)"
        );
    }
}

#[test]
fn ipv6_residential_and_local_not_flagged() {
    for ip in [
        "2001:db8::1",  // documentation prefix — not a datacenter range
        "2607:fb90::1", // mobile carrier space — not in table
        "::1",          // loopback
        "fe80::1",      // link-local
        "fd00::1",      // ULA
    ] {
        assert!(
            !ComplianceChecker::is_datacenter_ip(ip),
            "{ip} should NOT be datacenter (IPv6)"
        );
    }
}

proptest! {
    /// Any host inside OVH's 2001:41d0::/32 is flagged, regardless of the low bits.
    #[test]
    fn ovh_v6_slash32_any_host_flagged(g3 in any::<u16>(), g4 in any::<u16>(), g8 in any::<u16>()) {
        let ip = format!("2001:41d0:{g3:x}:{g4:x}:0:0:0:{g8:x}");
        prop_assert!(
            ComplianceChecker::is_datacenter_ip(&ip),
            "{} inside OVH /32 must be datacenter", ip
        );
    }

    /// The documentation prefix 2001:db8::/32 is never a datacenter range.
    #[test]
    fn doc_prefix_v6_never_flagged(g3 in any::<u16>(), g4 in any::<u16>()) {
        let ip = format!("2001:db8:{g3:x}:{g4:x}:0:0:0:1");
        prop_assert!(
            !ComplianceChecker::is_datacenter_ip(&ip),
            "{} must NOT be datacenter", ip
        );
    }
}

// ── Colocation / VPN / adversarial verdicts (public, non-datacenter IPs) ──

#[test]
fn same_public_ip_colocation_is_incidental() {
    // 3 validators on one public IP — same exact IP → NerfedIncidental
    // (3 is not > 3, so the VPN/proxy path does not fire).
    let mut c = ComplianceChecker::new();
    for i in 1..=3u8 {
        c.register_node(a(i), "203.0.113.5".into()); // TEST-NET-3, not datacenter
    }
    for i in 1..=3u8 {
        assert_eq!(c.check(&a(i)), ComplianceStatus::NerfedIncidental, "node {i}");
    }
}

#[test]
fn many_validators_one_ip_is_adversarial() {
    // >3 validators behind one IP → VPN/proxy → NerfedAdversarial.
    let mut c = ComplianceChecker::new();
    for i in 1..=5u8 {
        c.register_node(a(i), "198.51.100.9".into()); // TEST-NET-2, not datacenter
    }
    assert_eq!(c.check(&a(1)), ComplianceStatus::NerfedAdversarial);
}

#[test]
fn distinct_public_ips_are_compliant() {
    let mut c = ComplianceChecker::new();
    c.register_node(a(1), "203.0.113.5".into());
    c.register_node(a(2), "198.51.100.9".into());
    assert_eq!(c.check(&a(1)), ComplianceStatus::Compliant);
    assert_eq!(c.check(&a(2)), ComplianceStatus::Compliant);
}

// ── Loopback exemption short-circuits even the VPN/adversarial path ──

#[test]
fn loopback_exempt_even_when_many_colocated() {
    let mut c = ComplianceChecker::new();
    for i in 1..=5u8 {
        c.register_node(a(i), "127.0.0.1".into()); // 5 > 3 would normally be Adversarial
    }
    for i in 1..=5u8 {
        assert_eq!(c.check(&a(i)), ComplianceStatus::Compliant, "loopback node {i} must be exempt");
    }

    // IPv6 loopback likewise.
    let mut c6 = ComplianceChecker::new();
    c6.register_node(a(1), "::1".into());
    c6.register_node(a(2), "::1".into());
    assert_eq!(c6.check(&a(1)), ComplianceStatus::Compliant);
}

// ── Suspicion score bounds ──

proptest! {
    /// The suspicion score is always within 0..=100 for any registration mix.
    #[test]
    fn suspicion_score_is_bounded(n in 1usize..8, same_ip in any::<bool>()) {
        let mut c = ComplianceChecker::new();
        for i in 0..n {
            let ip = if same_ip {
                "203.0.113.1".to_string()
            } else {
                format!("203.0.113.{}", i + 1)
            };
            c.register_node(a(i as u8), ip);
        }
        for i in 0..n {
            let s = c.suspicion_score(&a(i as u8));
            prop_assert!(s <= 100, "suspicion score {} must be within 0..=100", s);
        }
    }
}
