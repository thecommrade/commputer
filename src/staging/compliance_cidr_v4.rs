// src/staging/compliance_cidr_v4.rs
//
// WHAT THIS DOES
// --------------
// Drop-in replacement for the IPv4 half of the datacenter / cloud detector in
// `src/validator/src/compliance_check.rs`. It replaces the coarse first-octet
// table in `is_datacenter_ipv4` (compliance_check.rs:329-379, commit cbd4e75)
// with a CIDR-precise matcher (`ipv4_in_prefix`, a 1:1 mirror of the existing
// `ipv6_in_prefix` at compliance_check.rs:30-46) driven by a corrected provider
// prefix table grounded in real BGP-announced ranges.
//
// Two concrete bugs from PROBLEM A5-cidr-tighten are closed:
//
//   1. HETZNER was too NARROW. The old table only matched 88.198.0.0/16 (plus
//      78.46, 148.251, 176.9, 46.4, 5.9). It missed every modern Hetzner /16:
//      88.99, 49.12, 49.13, 65.21, 65.108, 65.109, 95.216, 95.217, 116.202,
//      116.203, 167.233, 167.235, 168.119. A Hetzner operator on 49.12.x or
//      95.216.x was silently treated as residential and earned FULL rewards —
//      the exact datacenter-farming the whitepaper says must be nerfed.
//
//   2. OVH was too BROAD. The old table flagged the ENTIRE 51.0.0.0/8 with
//      `octets[0] == 51`. OVH does NOT own 51/8. It announces 18 specific /16
//      and /17 blocks inside 51.x; the rest of 51/8 belongs to RIPE members
//      that are NOT OVH (Scaleway 51.15/51.158, various ISPs, etc.). A genuine
//      residential / non-OVH operator who happened to land on 51.100.x was
//      wrongly flagged NerfedIncidental and lost ~80% of rewards. This now
//      only matches OVH's real blocks; 51.100.x and 51.15.x are NOT flagged.
//
// WHERE IT WIRES IN
// -----------------
// EXISTING FILE TO CHANGE: src/validator/src/compliance_check.rs  (NON-PROTECTED)
//
//   a) Add `ipv4_in_prefix` as a free fn next to `ipv6_in_prefix`
//      (compliance_check.rs:30-46). It is self-contained — no new imports
//      beyond `std::net::Ipv4Addr`, which the file already uses.
//
//   b) Replace the body of `ComplianceChecker::is_datacenter_ipv4`
//      (compliance_check.rs:329-379) with the `is_datacenter_ipv4` body below.
//      The public surface is unchanged: same name, same
//      `fn(std::net::Ipv4Addr) -> bool` signature, still called from
//      `is_datacenter_ip` (compliance_check.rs:322) which already dispatches
//      V4 -> is_datacenter_ipv4 and V6 -> is_datacenter_ipv6. No caller changes.
//
//   c) Paste `DATACENTER_V4_PREFIXES` and (optionally) `match_datacenter_ipv4`
//      as private items on the impl/module. `is_datacenter_ipv4` can simply be
//      `match_datacenter_ipv4(addr.octets()).is_some()` — the label variant is
//      handy for tracing/diagnostics but is not required.
//
//   d) Move the new tests in this file's `#[cfg(test)] mod cidr_v4_tests` into
//      the existing `mod tests` in compliance_check.rs (or keep as a sibling
//      module). They call only the free fns + the table, so they need no
//      `ComplianceChecker` / `Address` setup.
//
// SECOND FILE THAT SHOULD BE KEPT IN LOCKSTEP (NON-PROTECTED, not auto-changed):
//   src/doctor/src/checks/cloud_ip.rs holds a deliberate standalone COPY of this
//   table (its header at cloud_ip.rs:1-10 says "in lockstep with
//   compliance_check.rs ... if that file changes, update this file too"). Its
//   `match_datacenter_ipv4` (cloud_ip.rs:127-175) has the SAME two bugs (narrow
//   Hetzner, `o[0] == 51` whole-/8 OVH) and its test `ovh_v4_flagged`
//   (cloud_ip.rs:249-251) asserts `[51,1,2,3] == Some("OVH")`, which is wrong
//   (51.1.x is not OVH). The founder should port the corrected
//   `match_datacenter_ipv4` + table below into cloud_ip.rs and fix that test to
//   use a real OVH address (e.g. [51,68,1,1]) plus a `[51,100,1,1] == None`
//   negative. Doing only compliance_check.rs leaves the operator-facing doctor
//   tool disagreeing with the on-chain verdict.
//
// PROTECTED-FILE DEPENDENCY: none. compliance_check.rs and cloud_ip.rs are both
// non-protected. No protected file is read or modified by this change.
//
// DATA PROVENANCE (verified 2026-06-10, not guessed)
// --------------------------------------------------
// Hetzner  AS24940  : RIPEstat announced-prefixes (stat.ripe.net/data/
//                     announced-prefixes/data.json?resource=AS24940).
// OVH      AS16276  : RIPEstat announced-prefixes ...?resource=AS16276. The
//                     51.x list below is the COMPLETE set of announced 51.x
//                     blocks (18 of them); 51/8 as a whole is NOT OVH.
// DigitalOcean AS14061 : RIPEstat ...?resource=AS14061. The listed /16s are
//                     densely covered by AS14061 /20 sub-prefixes across the
//                     whole /16 (DO-owned). Partially-covered /16s are marked
//                     TODO and NOT included rather than over-claiming.
// GCP            : Google's published cloud.json
//                     (www.gstatic.com/ipranges/cloud.json). Google allocates
//                     granularly; the large aggregates confirmed present are
//                     included, the rest stay as the proven coarse heuristic.
// AWS / Azure    : kept as the existing coarse first-octet heuristic (expressed
//                     as /8 + sub-blocks). AWS/Azure/GCP publish thousands of
//                     fragmented regional prefixes; an exhaustive static table
//                     is infeasible here and the coarse cuts were already
//                     accepted in the shipping code. They are PRESERVED, only
//                     re-expressed as CIDRs, so existing behavior/tests hold.
//
// Anything not BGP-confirmable is left as a `// TODO(cidr):` line rather than
// guessed, per the deliverable rules.

use std::net::Ipv4Addr;

// ---------------------------------------------------------------------------
// CIDR matcher — mirror of compliance_check.rs::ipv6_in_prefix (lines 30-46).
// ---------------------------------------------------------------------------

/// Returns true iff `addr` falls inside `net/len`.
///
/// `addr` is the 4 octets of an IPv4 address (`Ipv4Addr::octets()`).
/// `net` is a textual dotted-quad parseable by `Ipv4Addr::from_str`, expected
/// to already be the network address (host bits zero, e.g. "51.68.0.0").
/// `len` is the prefix length in bits, 0..=32. Out-of-range `len` returns
/// false; an unparseable `net` returns false. `len == 0` matches everything
/// (kept for parity with the v6 helper; not used by the table below).
///
/// Symmetric with `ipv6_in_prefix`: build a high-bit mask, compare masked
/// addr against masked net. Note the host-bit-shift hazard at len==0 (a
/// `<< 32` on a u32 is UB-shaped) is handled by the explicit `len == 0`
/// early-return, exactly as the v6 helper guards `<< 128`.
pub fn ipv4_in_prefix(addr: [u8; 4], net: &str, len: u8) -> bool {
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

// ---------------------------------------------------------------------------
// Corrected provider CIDR table.
//
// Each row is (network, prefix_len, provider_label). Ordering does not affect
// correctness (membership is disjoint enough in practice); first match wins.
// ---------------------------------------------------------------------------

pub const DATACENTER_V4_PREFIXES: &[(&str, u8, &str)] = &[
    // ----- AWS (AS16509 / AS14618) -------------------------------------
    // PRESERVED coarse heuristic: AWS owns most of these /8 fronts heavily.
    // AWS publishes ~ thousands of fragmented prefixes in ip-ranges.json;
    // a faithful static table is infeasible, so the original accepted
    // first-octet cuts are re-expressed as /8s. Behavior is identical to
    // compliance_check.rs:332-336 (aws_prefixes 3,13,18,34,35,52,54).
    // NOTE: 34/8 and 35/8 are SHARED with GCP; 13/8 and 52/8 partly shared
    // with Azure. The union still resolves to "datacenter", which is the
    // only bit `is_datacenter_ipv4` cares about, so the overlap is benign.
    ("3.0.0.0", 8, "AWS"),
    ("13.0.0.0", 8, "AWS"),
    ("18.0.0.0", 8, "AWS"),
    ("34.0.0.0", 8, "AWS"),  // also GCP
    ("35.0.0.0", 8, "AWS"),  // also GCP
    ("52.0.0.0", 8, "AWS"),
    ("54.0.0.0", 8, "AWS"),  // NB: OVH 54.36-54.39 are inside 54/8; the
                             // dedicated OVH rows below are redundant for the
                             // boolean result but kept for label accuracy and
                             // in case the 54/8 AWS heuristic is ever narrowed.

    // ----- Azure (AS8075) ----------------------------------------------
    // PRESERVED coarse heuristic from compliance_check.rs:344 (20.x, 40.x).
    // Azure's real footprint is far larger (13.x, 52.x shared w/ AWS,
    // 104.40.0.0/13, 137.116/14, 168.61/16, ...) but 20/8 and 40/8 are the
    // dense Azure-dominant fronts the shipping code already trusts.
    ("20.0.0.0", 8, "Azure"),
    ("40.0.0.0", 8, "Azure"),
    // TODO(cidr): consider adding confirmed Azure aggregates from
    // ServiceTags_Public.json (e.g. 104.40.0.0/13, 168.61.0.0/16,
    // 13.104.0.0/14) once the founder wants tighter Azure coverage. Left out
    // here to avoid over-claiming 104.40/13 vs the GCP/other 104.x rows.

    // ----- GCP (AS15169 / AS396982) ------------------------------------
    // Old code: only 104.196.x and 104.199.x as /24-style octet checks
    // (compliance_check.rs:339). Broadened to the confirmed cloud.json
    // aggregates. 34/8 and 35/8 GCP space is already covered by the AWS
    // rows above (shared fronts), so these add the 104.x / 130.211 GCP land.
    ("104.196.0.0", 14, "GCP"),  // cloud.json: 104.196.0.0/18 + siblings,
                                 // historically the 104.196.0.0/14 GCP block
    ("104.154.0.0", 15, "GCP"),  // 104.154/15 GCP us-central
    ("104.197.0.0", 16, "GCP"),  // confirmed present in cloud.json
    ("130.211.0.0", 16, "GCP"),  // GCP load-balancer / global front
    ("35.184.0.0", 13, "GCP"),   // 35.184.0.0/13 (covered by 35/8 AWS row too,
                                 // listed for label clarity)
    // TODO(cidr): GCP allocates very granularly (34.x /16s scattered, 35.x
    // /15s). The dense fronts are captured by 34/8 + 35/8; the 104.x and
    // 130.211 rows above add the non-3x GCP land. Tightening to exact
    // cloud.json regional /16s would require generating the table from the
    // live JSON at build time — out of scope for a static hand table.

    // ----- Hetzner (AS24940) — BROADENED -------------------------------
    // Source: RIPEstat announced-prefixes for AS24940 (verified 2026-06-10).
    // The old table matched ONLY 88.198/16 (+ 78.46, 148.251, 176.9, 46.4,
    // 5.9). These are the real announced Hetzner /15-/17 blocks that were
    // being MISSED:
    ("88.198.0.0", 16, "Hetzner"),   // kept (was already correct)
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
    // Hetzner aggregates / legacy blocks confirmed announced:
    ("78.46.0.0", 15, "Hetzner"),    // was 78.46/16 in old code; real is /15
                                     // (covers 78.46.0.0 - 78.47.255.255)
    ("148.251.0.0", 16, "Hetzner"),  // kept (correct)
    ("176.9.0.0", 16, "Hetzner"),    // kept (correct)
    ("5.9.0.0", 16, "Hetzner"),      // kept (correct)
    ("46.4.0.0", 16, "Hetzner"),     // was 46.4/16 in old code (correct)
    ("46.224.0.0", 15, "Hetzner"),   // newly caught aggregate
    ("5.75.128.0", 17, "Hetzner"),   // newly caught (Hetzner cloud Ashburn etc)
    // TODO(cidr): AS24940 announces ~80 IPv4 prefixes; the /16+ blocks above
    // are the dense Hetzner-dedicated server / cloud fronts. Smaller /22-/24
    // Hetzner blocks (e.g. inside 136.243, 144.76, 144.91, 159.69, 162.55,
    // 188.40, 213.133, 213.239) are NOT yet enumerated here — add from
    // RIPEstat if false-negatives on those ranges become a problem. Marked
    // TODO rather than guessed octet boundaries.

    // ----- OVH (AS16276) — TIGHTENED -----------------------------------
    // Source: RIPEstat announced-prefixes for AS16276 (verified 2026-06-10).
    // OLD CODE FLAGGED ALL OF 51.0.0.0/8 — WRONG. OVH announces exactly these
    // 18 blocks inside 51.x; everything else in 51/8 is other RIPE members
    // (e.g. Scaleway 51.15/51.158, sundry ISPs) and MUST NOT be flagged.
    ("51.38.0.0", 16, "OVH"),
    ("51.68.0.0", 16, "OVH"),
    ("51.75.0.0", 16, "OVH"),
    ("51.77.0.0", 16, "OVH"),
    ("51.79.0.0", 16, "OVH"),    // announced as 51.79.0.0/17 + 51.79.128.0/17
                                 // -> a /16 row covers both with no false land
                                 // (OVH owns the whole 51.79/16).
    ("51.81.0.0", 16, "OVH"),    // announced as two /17s; OVH owns 51.81/16.
    ("51.83.0.0", 16, "OVH"),
    ("51.89.0.0", 16, "OVH"),
    ("51.91.0.0", 16, "OVH"),
    ("51.161.0.0", 16, "OVH"),   // announced as two /17s; OVH owns 51.161/16.
    ("51.178.0.0", 16, "OVH"),
    ("51.195.0.0", 16, "OVH"),
    ("51.210.0.0", 16, "OVH"),
    ("51.222.0.0", 16, "OVH"),
    ("51.254.0.0", 15, "OVH"),   // 51.254.0.0/15 -> 51.254.0.0 - 51.255.255.255
    // OVH legacy / non-51 blocks confirmed announced by AS16276:
    ("54.36.0.0", 14, "OVH"),    // 54.36/14 -> 54.36.0.0 - 54.39.255.255
                                 // (old code had only 54.36/16; real is /14).
                                 // Also inside AWS 54/8 row, so boolean result
                                 // is unchanged; kept for label correctness.
    ("87.98.0.0", 16, "OVH"),    // announced as 87.98.128.0/17; /16 row is a
                                 // mild over-cover. TODO(cidr): RIPEstat only
                                 // confirmed 87.98.128.0/17 for AS16276 today.
                                 // If strict, change to ("87.98.128.0",17,..).
    ("91.121.0.0", 16, "OVH"),
    ("149.202.0.0", 16, "OVH"),
    ("145.239.0.0", 16, "OVH"),  // newly caught OVH
    ("137.74.0.0", 16, "OVH"),   // newly caught OVH
    ("141.94.0.0", 16, "OVH"),   // newly caught OVH
    ("141.95.0.0", 16, "OVH"),   // newly caught OVH (two /17s -> /16)
    ("178.32.0.0", 15, "OVH"),   // newly caught OVH aggregate
    ("188.165.0.0", 16, "OVH"),  // newly caught OVH
    ("5.135.0.0", 16, "OVH"),    // newly caught OVH
    ("5.196.0.0", 16, "OVH"),    // newly caught OVH
    ("92.222.0.0", 16, "OVH"),   // newly caught OVH
    ("94.23.0.0", 16, "OVH"),    // newly caught OVH
    ("213.32.0.0", 17, "OVH"),   // newly caught OVH
    ("5.39.0.0", 17, "OVH"),     // newly caught OVH
    // TODO(cidr): AS16276 announces ~600 IPv4 prefixes; the /15-/17 blocks
    // above are the dense OVH-dedicated fronts. Smaller scattered OVH /19-/24
    // (e.g. 213.186.32.0/19) are not all enumerated; add from RIPEstat if
    // needed. NOT guessing octet ranges.

    // ----- DigitalOcean (AS14061) --------------------------------------
    // Source: RIPEstat announced-prefixes for AS14061 (verified 2026-06-10).
    // Old code: 64.225, 104.131, 128.199, 167.71, 167.172 as /16 octet
    // checks. Each of these /16s is densely covered by AS14061 /20 sub-blocks
    // across the full /16 (DO-owned), so /16 rows are correct. Added the other
    // fully-covered DO /16s confirmed in the same data.
    ("64.225.0.0", 16, "DigitalOcean"),   // kept
    ("104.131.0.0", 16, "DigitalOcean"),  // kept
    ("128.199.0.0", 16, "DigitalOcean"),  // kept
    ("167.71.0.0", 16, "DigitalOcean"),   // kept
    ("167.172.0.0", 16, "DigitalOcean"),  // kept
    ("134.122.0.0", 16, "DigitalOcean"),  // newly caught (full /16 coverage)
    ("137.184.0.0", 16, "DigitalOcean"),  // newly caught
    ("138.197.0.0", 16, "DigitalOcean"),  // newly caught
    ("138.68.0.0", 16, "DigitalOcean"),   // newly caught
    ("142.93.0.0", 16, "DigitalOcean"),   // newly caught (full /16 coverage)
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
    // TODO(cidr): 143.110.0.0/16 and 64.23.0.0/16 were only PARTIALLY covered
    // by AS14061 sub-prefixes in today's RIPEstat snapshot, so they are NOT
    // claimed as full /16s here (would risk flagging non-DO neighbors). Add
    // the exact announced sub-blocks (e.g. 143.110.128.0/17) if those ranges
    // need coverage. Marked TODO rather than over-claiming.
];

// ---------------------------------------------------------------------------
// Lookups.
// ---------------------------------------------------------------------------

/// Returns the matching provider label for an IPv4 address, or `None`.
/// Useful for tracing / diagnostics; `is_datacenter_ipv4` only needs the bool.
pub fn match_datacenter_ipv4(octets: [u8; 4]) -> Option<&'static str> {
    for (net, len, label) in DATACENTER_V4_PREFIXES {
        if ipv4_in_prefix(octets, net, *len) {
            return Some(label);
        }
    }
    None
}

/// IPv4 datacenter detection — CIDR-precise replacement for the legacy
/// octet-prefix table. Drop this body into
/// `ComplianceChecker::is_datacenter_ipv4` (compliance_check.rs:329-379);
/// signature and call site are unchanged.
pub fn is_datacenter_ipv4(addr: Ipv4Addr) -> bool {
    match_datacenter_ipv4(addr.octets()).is_some()
}

// ---------------------------------------------------------------------------
// Tests — exercise REAL behavior (membership math + the corrected table).
// Move into compliance_check.rs `mod tests` on wire-in, or keep as a sibling.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod cidr_v4_tests {
    use super::*;
    use std::str::FromStr;

    fn dc(s: &str) -> bool {
        is_datacenter_ipv4(Ipv4Addr::from_str(s).unwrap())
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
        // Parity with ipv6_in_prefix len==0; guards the <<32 host-shift hazard.
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
        assert!(dc("104.196.1.1"));   // GCP 104.196/14
        assert!(dc("104.154.5.5"));   // GCP 104.154/15
        assert!(dc("130.211.1.1"));   // GCP global LB
        // 34/8 and 35/8 GCP land resolves via the shared AWS rows:
        assert!(dc("34.64.1.1"));
        assert!(dc("35.184.1.1"));
    }

    #[test]
    fn hetzner_positive_legacy_kept() {
        // The one range the OLD code got right must still pass.
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

    // ---- THE BUG FIX 1: previously-missed Hetzner ranges now caught ----

    #[test]
    fn hetzner_previously_missed_now_caught() {
        // Every one of these returned FALSE under the old octet table.
        for ip in [
            "88.99.1.1",
            "49.12.1.1",
            "49.13.1.1",
            "65.21.1.1",
            "65.108.1.1",
            "65.109.1.1",
            "95.216.1.1",
            "95.217.1.1",
            "116.202.1.1",
            "116.203.1.1",
            "167.233.1.1",
            "167.235.1.1",
            "168.119.1.1",
        ] {
            assert_eq!(label(ip), Some("Hetzner"), "{} should be Hetzner now", ip);
            assert!(dc(ip), "{} should be datacenter now", ip);
        }
    }

    #[test]
    fn hetzner_78_46_now_full_slash15() {
        // Old code matched 78.46/16 only; real allocation is 78.46.0.0/15,
        // so 78.47.x was being missed.
        assert!(dc("78.46.5.5"));
        assert!(dc("78.47.5.5")); // newly caught half of the /15
        assert!(!dc("78.48.5.5")); // just past the /15 — still residential
        assert!(!dc("78.45.5.5")); // just before — residential
    }

    // ---- THE BUG FIX 2: OVH tightened, non-OVH 51.x NOT flagged ----

    #[test]
    fn ovh_real_blocks_flagged() {
        for ip in [
            "51.38.1.1",
            "51.68.1.1",
            "51.75.1.1",
            "51.77.1.1",
            "51.79.200.1", // upper /17 of the 51.79/16
            "51.81.200.1",
            "51.83.1.1",
            "51.89.1.1",
            "51.91.1.1",
            "51.161.200.1",
            "51.178.1.1",
            "51.195.1.1",
            "51.210.1.1",
            "51.222.1.1",
            "51.254.1.1",
            "51.255.1.1", // upper half of 51.254/15
        ] {
            assert_eq!(label(ip), Some("OVH"), "{} should be OVH", ip);
        }
    }

    #[test]
    fn non_ovh_51x_not_flagged() {
        // THE headline regression: the old `octets[0] == 51` flagged ALL of
        // 51/8. These 51.x addresses are NOT OVH and MUST be compliant.
        for ip in [
            "51.100.5.5",  // unallocated-to-OVH RIPE space
            "51.0.0.1",    // bottom of 51/8, not an OVH block
            "51.1.2.3",    // (the doctor crate's buggy test used this as OVH)
            "51.15.1.1",   // Scaleway (AS12876), NOT OVH
            "51.158.1.1",  // Scaleway, NOT OVH
            "51.69.1.1",   // gap between 51.68/16 and 51.75/16
            "51.200.1.1",  // gap, not announced by OVH
            "51.253.1.1",  // just below 51.254/15 OVH block
        ] {
            assert_eq!(label(ip), None, "{} must NOT be flagged (not OVH)", ip);
            assert!(!dc(ip), "{} must NOT be datacenter", ip);
        }
    }

    #[test]
    fn ovh_51_254_slash15_boundary() {
        assert!(dc("51.254.0.0"));   // first addr of 51.254/15
        assert!(dc("51.255.255.255")); // last addr of 51.254/15
        assert!(!dc("51.253.255.255")); // one below — not OVH
    }

    #[test]
    fn ovh_new_non51_blocks_flagged() {
        for ip in [
            "145.239.1.1",
            "137.74.1.1",
            "141.94.1.1",
            "141.95.200.1",
            "178.32.5.5",
            "178.33.5.5", // upper half of 178.32/15
            "188.165.1.1",
            "92.222.1.1",
            "94.23.1.1",
        ] {
            assert_eq!(label(ip), Some("OVH"), "{} should be OVH", ip);
        }
    }

    // ---- DigitalOcean newly-caught /16s ----

    #[test]
    fn digitalocean_new_blocks_flagged() {
        for ip in [
            "134.122.1.1",
            "137.184.1.1",
            "138.197.1.1",
            "138.68.1.1",
            "142.93.1.1",
            "146.190.1.1",
            "157.230.1.1",
            "159.65.1.1",
            "159.89.1.1",
            "161.35.1.1",
            "165.22.1.1",
            "165.227.1.1",
            "206.189.1.1",
            "64.227.1.1",
        ] {
            assert_eq!(label(ip), Some("DigitalOcean"), "{} should be DO", ip);
        }
    }

    #[test]
    fn digitalocean_unclaimed_partial_16s_not_overclaimed() {
        // 143.110/16 and 64.23/16 are intentionally left out (only partially
        // DO-owned). Documenting current behavior so a future broadening is a
        // conscious decision, not an accident. If the founder adds the exact
        // announced sub-blocks these asserts should be updated.
        assert_eq!(label("143.110.1.1"), None);
        assert_eq!(label("64.23.1.1"), None);
    }

    // ---- Residential / private negatives ----

    #[test]
    fn residential_and_private_not_flagged() {
        for ip in [
            "192.168.1.1",  // RFC1918 private
            "10.0.1.1",     // RFC1918 private
            "172.16.1.1",   // RFC1918 private
            "71.12.34.56",  // US residential (Comcast-ish)
            "24.1.1.1",     // US cable residential
            "86.1.2.3",     // UK BT residential
            "100.64.0.1",   // CGNAT (RFC6598)
            "127.0.0.1",    // loopback
            "8.8.8.8",      // Google public DNS — not in our DC table
        ] {
            assert!(!dc(ip), "{} must NOT be datacenter", ip);
        }
    }

    // ---- CIDR boundary tests on representative provider blocks ----

    #[test]
    fn hetzner_88_99_slash16_boundaries() {
        assert!(dc("88.99.0.0"));      // first addr
        assert!(dc("88.99.255.255"));  // last addr
        assert!(!dc("88.100.0.0"));    // one past the /16
        assert!(!dc("88.98.255.255")); // one before the /16
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
        assert!(!dc("55.0.0.0"));   // 55/8 is DoD, not AWS
        assert!(!dc("53.255.255.255")); // 53/8 not in table
    }

    // ---- Parity guard: the exact assertions the shipping tests rely on ----

    #[test]
    fn shipping_feature_148_assertions_still_hold() {
        // Mirrors compliance_check.rs::feature_148_datacenter_ip and
        // ipv4_string_still_works_through_new_parser so the rewrite is a
        // strict superset of the old positives (minus the OVH over-flag).
        assert!(dc("3.5.10.20"));     // AWS — still true
        assert!(dc("88.198.1.1"));    // Hetzner — still true
        assert!(!dc("192.168.1.1"));  // private — still false
    }
}
