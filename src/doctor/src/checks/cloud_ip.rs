// checks/cloud_ip.rs — datacenter / cloud IP detector (IPv4 + IPv6)
//
// Standalone copy of the prefix table used in
// src/validator/src/compliance_check.rs::is_datacenter_ip. We deliberately do
// NOT depend on the validator crate so the doctor can ship as a tiny
// self-contained binary that operators run BEFORE node startup.
//
// CITATION: The IPv4 octet table and the IPv6 IPV6_DC_PREFIXES are in lockstep
// with src/validator/src/compliance_check.rs (commit 44c3f50). If that file
// changes, update this file too.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use crate::{CheckResult, Severity};

/// Classify a single IP string. Returns Info / Warning depending on match.
/// Accepts both IPv4 dotted-quad and IPv6 colon-hex strings.
pub fn classify_ip(ip: &str) -> CheckResult {
    let parsed: IpAddr = match ip.parse() {
        Ok(p) => p,
        Err(_) => {
            return CheckResult::warn(
                "net.public_ip.parse",
                format!("could not parse '{}' as IPv4 or IPv6", ip),
                "pass an IPv4 dotted-quad or IPv6 colon-hex literal",
            );
        }
    };

    let provider = match parsed {
        IpAddr::V4(v4) => match_datacenter_ipv4(v4.octets()),
        IpAddr::V6(v6) => match_datacenter_ipv6(v6),
    };

    if let Some(provider) = provider {
        CheckResult {
            check: "net.public_ip".into(),
            severity: Severity::Warning,
            message: format!("public IP {} matches {} datacenter range", ip, provider),
            suggestion: Some(
                "validators on commercial cloud are flagged NerfedIncidental \
                 (see src/validator/src/compliance_check.rs::is_datacenter_ip). \
                 Run on residential / colo hardware to earn full rewards."
                    .into(),
            ),
        }
    } else {
        CheckResult::ok(
            "net.public_ip",
            format!("public IP {} does not match any flagged datacenter range", ip),
        )
    }
}

/// Best-effort: ask a public service for our outbound IP, then classify it.
/// If we cannot resolve, we emit a warning (never an error — air-gapped
/// operators are valid).
pub fn check_local_public_ip() -> CheckResult {
    match resolve_public_ip() {
        Ok(ip) => classify_ip(&ip),
        Err(e) => CheckResult::warn(
            "net.public_ip",
            format!("could not determine public IP: {}", e),
            "set --check-public-ip <addr> manually, or ignore if air-gapped",
        ),
    }
}

fn resolve_public_ip() -> Result<String, String> {
    // Hand-rolled HTTP/1.0 GET to avoid pulling in reqwest. Tries a couple of
    // services so a single outage does not break us. Both services return v4
    // by default; v6-only operators can pass --check-public-ip <v6> manually.
    const ENDPOINTS: &[(&str, &str)] = &[
        ("api.ipify.org:80", "GET / HTTP/1.0\r\nHost: api.ipify.org\r\nConnection: close\r\n\r\n"),
        ("ifconfig.me:80",  "GET /ip HTTP/1.0\r\nHost: ifconfig.me\r\nConnection: close\r\n\r\n"),
    ];
    let mut last_err = String::from("no endpoints tried");
    for (host, req) in ENDPOINTS {
        match http_text_get(host, req) {
            Ok(body) => {
                let trimmed = body.trim();
                if trimmed.parse::<IpAddr>().is_ok() {
                    return Ok(trimmed.to_string());
                }
                last_err = format!("unparseable response from {}", host);
            }
            Err(e) => last_err = format!("{}: {}", host, e),
        }
    }
    Err(last_err)
}

fn http_text_get(host_port: &str, request: &str) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};

    let mut stream = TcpStream::connect_timeout(
        &host_port
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or_else(|| "no DNS result".to_string())?,
        Duration::from_secs(3),
    )
    .map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    if let Some(idx) = buf.find("\r\n\r\n") {
        Ok(buf[idx + 4..].to_string())
    } else {
        Err("malformed HTTP response".into())
    }
}

// ---------------------------------------------------------------------------
// IPv4 prefix matching — mirrors compliance_check.rs::is_datacenter_ipv4
// ---------------------------------------------------------------------------

/// A5-cidr-tighten: CIDR matcher, mirror of compliance_check.rs::ipv4_in_prefix.
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

/// A5-cidr-tighten: CIDR-precise datacenter/cloud IPv4 prefix table.
/// LOCKSTEP COPY of compliance_check.rs::DATACENTER_V4_PREFIXES — if you change
/// one, change the other, or the operator doctor disagrees with the on-chain
/// verdict. BGP-grounded (RIPEstat AS24940 Hetzner / AS16276 OVH / AS14061 DO;
/// Google cloud.json). Replaces the legacy octet table that wrongly flagged the
/// whole 51/8 as OVH and missed modern Hetzner /16s.
const DATACENTER_V4_PREFIXES: &[(&str, u8, &str)] = &[
    // ----- AWS (AS16509 / AS14618) — coarse /8 heuristic, preserved ----
    ("3.0.0.0", 8, "AWS"),
    ("13.0.0.0", 8, "AWS"),
    ("18.0.0.0", 8, "AWS"),
    ("34.0.0.0", 8, "AWS"),  // also GCP
    ("35.0.0.0", 8, "AWS"),  // also GCP
    ("52.0.0.0", 8, "AWS"),
    ("54.0.0.0", 8, "AWS"),  // OVH 54.36/14 sits inside

    // ----- Azure (AS8075) — coarse /8 heuristic, preserved -------------
    ("20.0.0.0", 8, "Azure"),
    ("40.0.0.0", 8, "Azure"),

    // ----- GCP (AS15169 / AS396982) — confirmed cloud.json aggregates --
    ("104.196.0.0", 14, "GCP"),
    ("104.154.0.0", 15, "GCP"),
    ("104.197.0.0", 16, "GCP"),
    ("130.211.0.0", 16, "GCP"),
    ("35.184.0.0", 13, "GCP"),

    // ----- Hetzner (AS24940) — BROADENED to real announced /15-/17 -----
    ("88.198.0.0", 16, "Hetzner"),
    ("88.99.0.0", 16, "Hetzner"),
    ("49.12.0.0", 16, "Hetzner"),
    ("49.13.0.0", 16, "Hetzner"),
    ("65.21.0.0", 16, "Hetzner"),
    ("65.108.0.0", 16, "Hetzner"),
    ("65.109.0.0", 16, "Hetzner"),
    ("95.216.0.0", 16, "Hetzner"),
    ("95.217.0.0", 16, "Hetzner"),
    ("116.202.0.0", 16, "Hetzner"),
    ("116.203.0.0", 16, "Hetzner"),
    ("167.233.0.0", 16, "Hetzner"),
    ("167.235.0.0", 16, "Hetzner"),
    ("168.119.0.0", 16, "Hetzner"),
    ("78.46.0.0", 15, "Hetzner"),
    ("148.251.0.0", 16, "Hetzner"),
    ("176.9.0.0", 16, "Hetzner"),
    ("5.9.0.0", 16, "Hetzner"),
    ("46.4.0.0", 16, "Hetzner"),
    ("46.224.0.0", 15, "Hetzner"),
    ("5.75.128.0", 17, "Hetzner"),

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
    ("51.254.0.0", 15, "OVH"),
    ("54.36.0.0", 14, "OVH"),
    ("87.98.0.0", 16, "OVH"),
    ("91.121.0.0", 16, "OVH"),
    ("149.202.0.0", 16, "OVH"),
    ("145.239.0.0", 16, "OVH"),
    ("137.74.0.0", 16, "OVH"),
    ("141.94.0.0", 16, "OVH"),
    ("141.95.0.0", 16, "OVH"),
    ("178.32.0.0", 15, "OVH"),
    ("188.165.0.0", 16, "OVH"),
    ("5.135.0.0", 16, "OVH"),
    ("5.196.0.0", 16, "OVH"),
    ("92.222.0.0", 16, "OVH"),
    ("94.23.0.0", 16, "OVH"),
    ("213.32.0.0", 17, "OVH"),
    ("5.39.0.0", 17, "OVH"),

    // ----- DigitalOcean (AS14061) — fully-covered /16s -----------------
    ("64.225.0.0", 16, "DigitalOcean"),
    ("104.131.0.0", 16, "DigitalOcean"),
    ("128.199.0.0", 16, "DigitalOcean"),
    ("167.71.0.0", 16, "DigitalOcean"),
    ("167.172.0.0", 16, "DigitalOcean"),
    ("134.122.0.0", 16, "DigitalOcean"),
    ("137.184.0.0", 16, "DigitalOcean"),
    ("138.197.0.0", 16, "DigitalOcean"),
    ("138.68.0.0", 16, "DigitalOcean"),
    ("142.93.0.0", 16, "DigitalOcean"),
    ("143.198.0.0", 16, "DigitalOcean"),
    ("146.190.0.0", 16, "DigitalOcean"),
    ("157.230.0.0", 16, "DigitalOcean"),
    ("159.65.0.0", 16, "DigitalOcean"),
    ("159.89.0.0", 16, "DigitalOcean"),
    ("161.35.0.0", 16, "DigitalOcean"),
    ("165.22.0.0", 16, "DigitalOcean"),
    ("165.227.0.0", 16, "DigitalOcean"),
    ("206.189.0.0", 16, "DigitalOcean"),
    ("64.227.0.0", 16, "DigitalOcean"),
];

fn match_datacenter_ipv4(o: [u8; 4]) -> Option<&'static str> {
    for (net, len, label) in DATACENTER_V4_PREFIXES {
        if ipv4_in_prefix(o, net, *len) {
            return Some(label);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// IPv6 prefix matching — mirrors compliance_check.rs::is_datacenter_ipv6.
// Aggregates intentionally cover provider-owned IANA allocations; residential
// ISPs do not announce inside these ranges.
// ---------------------------------------------------------------------------

fn match_datacenter_ipv6(addr: Ipv6Addr) -> Option<&'static str> {
    const IPV6_DC_PREFIXES: &[(&str, u8, &str)] = &[
        ("2600:1f00::",   24, "AWS"),          // global EC2
        ("2406:da00::",   24, "AWS"),          // APAC EC2
        ("2603:1000::",   24, "Azure"),
        ("2620:1ec::",    36, "Azure"),        // edge / front-door
        ("2600:1900::",   28, "GCP"),
        ("2620:0:1c00::", 40, "GCP"),          // Google misc / corp edge
        ("2a01:4f8::",    29, "Hetzner"),
        ("2a01:4f9::",    32, "Hetzner"),
        ("2001:41d0::",   32, "OVH"),
        ("2604:a880::",   32, "DigitalOcean"), // US AS14061
        ("2a03:b0c0::",   32, "DigitalOcean"), // EU AS14061
    ];
    for (prefix, len, label) in IPV6_DC_PREFIXES {
        if ipv6_in_prefix(addr, prefix, *len) {
            return Some(label);
        }
    }
    None
}

fn ipv6_in_prefix(addr: Ipv6Addr, prefix: &str, len: u8) -> bool {
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

// Suppress dead-code warning if some helpers go unused under future feature gates.
#[allow(dead_code)]
fn _ipv4_for_compat(_a: Ipv4Addr) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_v4_prefix_flagged() {
        assert_eq!(match_datacenter_ipv4([3, 5, 6, 7]), Some("AWS"));
        assert_eq!(match_datacenter_ipv4([54, 1, 2, 3]), Some("AWS"));
    }

    #[test]
    fn azure_v4_flagged() {
        assert_eq!(match_datacenter_ipv4([20, 1, 2, 3]), Some("Azure"));
        assert_eq!(match_datacenter_ipv4([40, 9, 9, 9]), Some("Azure"));
    }

    #[test]
    fn hetzner_v4_flagged() {
        assert_eq!(match_datacenter_ipv4([88, 198, 1, 2]), Some("Hetzner"));
        assert_eq!(match_datacenter_ipv4([5, 9, 1, 2]), Some("Hetzner"));
    }

    #[test]
    fn ovh_v4_flagged() {
        // A5-cidr-tighten: 51.68/16 is a real announced OVH block. The old code
        // wrongly flagged the WHOLE 51/8 (incl. 51.1.x and Scaleway 51.15.x);
        // those are NOT OVH and must now be None.
        assert_eq!(match_datacenter_ipv4([51, 68, 1, 1]), Some("OVH"));
        assert_eq!(match_datacenter_ipv4([51, 1, 2, 3]), None);
        assert_eq!(match_datacenter_ipv4([51, 100, 1, 1]), None);
    }

    #[test]
    fn digitalocean_v4_flagged() {
        assert_eq!(match_datacenter_ipv4([167, 71, 1, 2]), Some("DigitalOcean"));
    }

    #[test]
    fn residential_v4_not_flagged() {
        assert_eq!(match_datacenter_ipv4([71, 12, 34, 56]), None);
        assert_eq!(match_datacenter_ipv4([24, 1, 1, 1]), None);
    }

    // ---- IPv6 anti-scale parity with compliance_check.rs ----

    #[test]
    fn aws_v6_flagged() {
        assert_eq!(
            match_datacenter_ipv6("2600:1f00:1000:1::1".parse().unwrap()),
            Some("AWS")
        );
        assert_eq!(
            match_datacenter_ipv6("2406:da00::1".parse().unwrap()),
            Some("AWS")
        );
    }

    #[test]
    fn azure_v6_flagged() {
        assert_eq!(
            match_datacenter_ipv6("2603:1000::1".parse().unwrap()),
            Some("Azure")
        );
    }

    #[test]
    fn gcp_v6_flagged() {
        assert_eq!(
            match_datacenter_ipv6("2600:1900::1".parse().unwrap()),
            Some("GCP")
        );
        assert_eq!(
            match_datacenter_ipv6("2620:0:1c00::1".parse().unwrap()),
            Some("GCP")
        );
    }

    #[test]
    fn hetzner_v6_flagged() {
        assert_eq!(
            match_datacenter_ipv6("2a01:4f8::1".parse().unwrap()),
            Some("Hetzner")
        );
    }

    #[test]
    fn ovh_v6_flagged() {
        assert_eq!(
            match_datacenter_ipv6("2001:41d0:1:abcd::1".parse().unwrap()),
            Some("OVH")
        );
    }

    #[test]
    fn digitalocean_v6_flagged() {
        assert_eq!(
            match_datacenter_ipv6("2604:a880::1".parse().unwrap()),
            Some("DigitalOcean")
        );
        assert_eq!(
            match_datacenter_ipv6("2a03:b0c0:3::1".parse().unwrap()),
            Some("DigitalOcean")
        );
    }

    #[test]
    fn residential_v6_not_flagged() {
        assert_eq!(match_datacenter_ipv6("2600::1".parse().unwrap()), None);
        assert_eq!(match_datacenter_ipv6("2001:db8::1".parse().unwrap()), None);
        assert_eq!(match_datacenter_ipv6("2607:f8b0::1".parse().unwrap()), None);
    }

    #[test]
    fn loopback_and_link_local_not_flagged() {
        assert_eq!(match_datacenter_ipv6("::1".parse().unwrap()), None);
        assert_eq!(match_datacenter_ipv6("fe80::1".parse().unwrap()), None);
    }

    // ---- end-to-end classify_ip ----

    #[test]
    fn classify_ip_v4_warning() {
        let r = classify_ip("3.4.5.6");
        assert_eq!(r.severity, Severity::Warning);
        assert!(r.message.contains("AWS"));
    }

    #[test]
    fn classify_ip_v4_ok() {
        let r = classify_ip("71.12.34.56");
        assert_eq!(r.severity, Severity::Info);
    }

    #[test]
    fn classify_ip_v6_warning() {
        let r = classify_ip("2600:1f00:1000:1::1");
        assert_eq!(r.severity, Severity::Warning);
        assert!(r.message.contains("AWS"));
    }

    #[test]
    fn classify_ip_v6_ok() {
        let r = classify_ip("2001:db8::1");
        assert_eq!(r.severity, Severity::Info);
    }

    #[test]
    fn classify_ip_garbage() {
        let r = classify_ip("not-an-ip");
        assert_eq!(r.severity, Severity::Warning);
    }
}
