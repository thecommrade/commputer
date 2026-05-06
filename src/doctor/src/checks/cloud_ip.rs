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

fn match_datacenter_ipv4(o: [u8; 4]) -> Option<&'static str> {
    // AWS EC2: 3.x, 13.x, 18.x, 34.x, 35.x, 52.x, 54.x.
    const AWS_PREFIXES: &[u8] = &[3, 13, 18, 34, 35, 52, 54];
    if AWS_PREFIXES.contains(&o[0]) {
        return Some("AWS");
    }

    // GCP additional: 104.196.x, 104.199.x.
    if o[0] == 104 && (o[1] == 196 || o[1] == 199) {
        return Some("GCP");
    }

    // Azure: 20.x, 40.x.
    if o[0] == 20 || o[0] == 40 {
        return Some("Azure");
    }

    // Hetzner.
    if (o[0] == 88 && o[1] == 198)
        || (o[0] == 78 && o[1] == 46)
        || (o[0] == 148 && o[1] == 251)
        || (o[0] == 176 && o[1] == 9)
        || (o[0] == 46 && o[1] == 4)
        || (o[0] == 5 && o[1] == 9)
    {
        return Some("Hetzner");
    }

    // OVH.
    if o[0] == 51
        || (o[0] == 54 && o[1] == 36)
        || (o[0] == 87 && o[1] == 98)
        || (o[0] == 91 && o[1] == 121)
        || (o[0] == 149 && o[1] == 202)
    {
        return Some("OVH");
    }

    // DigitalOcean.
    if (o[0] == 64 && o[1] == 225)
        || (o[0] == 104 && o[1] == 131)
        || (o[0] == 128 && o[1] == 199)
        || (o[0] == 167 && (o[1] == 71 || o[1] == 172))
    {
        return Some("DigitalOcean");
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
        assert_eq!(match_datacenter_ipv4([51, 1, 2, 3]), Some("OVH"));
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
