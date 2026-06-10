// compliance_exemption.rs — STAGED, NOT WIRED IN. Founder-review artifact.
//
// WHAT THIS DOES
// -------------
// Adds a founder/seed compliance-exemption mechanism to ComplianceChecker
// that does NOT become a scale loophole. Two scoped exemptions:
//   (b) datacenter-nerf-only exemption for a GENESIS-DECLARED allowlist of
//       founder/seed addresses. Skips ONLY the is_datacenter_ip branch.
//   (a-slice) is_trusted() (720 clean epochs) sheds the INCIDENTAL
//       subnet/ASN nerf ONLY (same-/24, same-/16, same-ASN). It never
//       relaxes datacenter, exact-same-IP, duplicate-fingerprint, or
//       VPN/proxy branches.
//
// Neither exemption can be set at runtime by a validator: the datacenter
// allowlist is populated only at boot from genesis (founder-signed); trust
// is earned over 720 clean epochs and still cannot shed the co-location
// (same-IP) or Sybil (fingerprint/VPN) branches. See
// src/staging/docs/compliance_exemption_blueprint.md for the full
// anti-abuse analysis.
//
// WHERE IT WIRES IN
// -----------------
// Target file (NON-PROTECTED): src/validator/src/compliance_check.rs
//   - Add the `datacenter_exempt` field to `struct ComplianceChecker`
//     (around line 88-111, alongside the other HashMaps).
//   - Add the setter/clearer/query methods (paste into the impl block).
//   - Add `check_at_epoch` and rewrite `check` as a back-compat wrapper.
//     `check` is currently at compliance_check.rs:501. The two skip-guards
//     go (i) immediately before the `is_datacenter_ip` return at line 516,
//     and (ii) gating the same-/24 (539), same-/16 (545) and same-ASN
//     (552) branches.
//   - Add `deregister_node` cleanup: also remove from `datacenter_exempt`
//     (compliance_check.rs:126).
//
// EXISTING FILES THAT NEED CHANGES (some PROTECTED — see blueprint):
//   - src/validator/src/compliance_check.rs (this patch) — NON-PROTECTED.
//   - genesis.json — PROTECTED. Add optional
//       "founder_seed_exemptions": ["<64-hex addr>", ...]
//   - src/core/src/genesis.rs (GenesisConfig) — NON-PROTECTED. Add
//       #[serde(default)] pub founder_seed_exemptions: Vec<String>
//   - src/node/src/event_loop.rs — PROTECTED. At boot, for each parsed
//       exemption address, call compliance.set_datacenter_exempt(addr).
//       To activate the trust slice later, wire mark_clean_epoch /
//       clear_clean_streak in the epoch loop and switch the check() call
//       at event_loop.rs:437 to check_at_epoch(addr, current_epoch).
//
// PROTECTED-FILE DEPENDENCY: activating (b) requires editing genesis.json;
// activating the (a)-slice requires editing event_loop.rs. With an empty
// allowlist and no epoch wiring, this patch is byte-for-byte behaviorally
// identical to today (status quo / option c).
//
// VERIFIED AGAINST CODE (agent-overnight-20260610):
//   - ComplianceStatus::{Compliant,NerfedIncidental,NerfedAdversarial} in
//     src/core/src/compliance.rs:9.
//   - Address([u8;32]) + Address::from_hex in src/core/src/identity.rs:9,24.
//   - is_trusted/first_clean_epoch at compliance_check.rs:302,104.
//   - check() branch order at compliance_check.rs:507/516/521/534/539/545/552.
//   - validator crate deps: commputer-core, tracing, serde only — this
//     patch adds NO new dependency (HashSet is std).
//
// =====================================================================
//  PASTE-IN REFERENCE (the items below name exactly what to merge).
// =====================================================================
//
// 1) Add this `use` near the top of compliance_check.rs (it already
//    imports std::collections::HashMap on line 1 — extend it):
//
//      use std::collections::{HashMap, HashSet};
//
// 2) Add this field to `struct ComplianceChecker` (after line ~110):
//
//      /// A6: Genesis-declared founder/seed addresses exempt from the
//      /// datacenter-IP nerf ONLY. Populated at boot from genesis; never
//      /// mutated by validator-controlled input. Empty => status quo.
//      datacenter_exempt: HashSet<Address>,
//
//    (Deriving Default still works: HashSet<Address>: Default.)
//
// 3) Paste the methods + check_at_epoch below into the impl block, and
//    replace the existing `check` with the wrapper shown. Add the
//    `datacenter_exempt.remove(addr)` line into `deregister_node`.
//
// The block below is written so it can be diffed directly against the
// real impl. Types/paths match the live crate.

use std::collections::{HashMap, HashSet};
use commputer_core::identity::Address;
use commputer_core::compliance::ComplianceStatus;

// ---- This is illustrative scaffolding so the paste-in items below are
// ---- type-checkable in isolation. In the real file these fields already
// ---- exist; do NOT duplicate them — only the NEW field + methods merge.
#[allow(dead_code)]
mod paste_in_reference {
    use super::*;

    /// Sentinel epoch meaning "caller did not supply an epoch, so trust
    /// (which is epoch-relative) must NOT apply." u64::MAX guarantees
    /// `current_epoch.saturating_sub(first_clean) >= 720` can still be
    /// true if a real large epoch is passed, but the wrapper below passes
    /// 0, for which is_trusted is always false (0.saturating_sub(x)==0).
    pub const NO_EPOCH_SENTINEL: u64 = 0;

    // The following mirror the helpers that ALREADY exist in the real
    // file; reproduced here only so this reference module compiles for
    // review. They are NOT part of the merge.
    fn subnet_24(ip: &str) -> Option<String> {
        let p: Vec<&str> = ip.split('.').collect();
        if p.len() == 4 { Some(format!("{}.{}.{}", p[0], p[1], p[2])) } else { None }
    }
    fn subnet_16(ip: &str) -> Option<String> {
        let p: Vec<&str> = ip.split('.').collect();
        if p.len() == 4 { Some(format!("{}.{}", p[0], p[1])) } else { None }
    }

    pub struct ComplianceCheckerRef {
        pub node_to_ip: HashMap<Address, String>,
        pub fingerprints: HashMap<Address, [u8; 32]>,
        pub node_asn: HashMap<Address, String>,
        pub first_clean_epoch: HashMap<Address, u64>,
        // NEW FIELD (this is the one that merges):
        pub datacenter_exempt: HashSet<Address>,
    }

    impl ComplianceCheckerRef {
        // --- existing helpers reproduced for the reference build only ---
        fn ip_validator_count(&self, ip: &str) -> usize {
            self.node_to_ip.values().filter(|v| v.as_str() == ip).count()
        }
        fn is_datacenter_ip(ip: &str) -> bool {
            // real impl is the associated fn ComplianceChecker::is_datacenter_ip
            // (compliance_check.rs:319). Reproduced as a stub for the ref build.
            ip.starts_with("3.") || ip.starts_with("88.198.")
        }
        fn is_trusted(&self, addr: &Address, current_epoch: u64) -> bool {
            const TRUST_THRESHOLD_EPOCHS: u64 = 720;
            if let Some(&first_clean) = self.first_clean_epoch.get(addr) {
                current_epoch.saturating_sub(first_clean) >= TRUST_THRESHOLD_EPOCHS
            } else { false }
        }

        // ================= NEW METHODS THAT MERGE =================

        /// A6: Mark a genesis-declared founder/seed address as exempt from
        /// the datacenter-IP nerf. Call ONLY from the genesis-driven boot
        /// path (event_loop.rs). There is intentionally no transaction /
        /// RPC route to this — a validator cannot exempt itself.
        pub fn set_datacenter_exempt(&mut self, addr: Address) {
            self.datacenter_exempt.insert(addr);
        }

        /// A6: Remove a datacenter exemption (e.g. on a fresh genesis load).
        pub fn clear_datacenter_exempt(&mut self, addr: &Address) {
            self.datacenter_exempt.remove(addr);
        }

        /// A6: Whether an address holds the genesis datacenter exemption.
        pub fn is_datacenter_exempt(&self, addr: &Address) -> bool {
            self.datacenter_exempt.contains(addr)
        }

        /// A6: Number of exempt addresses (for RPC / audit display).
        pub fn datacenter_exempt_count(&self) -> usize {
            self.datacenter_exempt.len()
        }

        /// A6: Epoch-aware compliance check. This is the new primary entry
        /// point. `check()` becomes a back-compat wrapper that passes the
        /// NO_EPOCH_SENTINEL (trust never applies without a real epoch).
        ///
        /// Branch order is IDENTICAL to the current check() except for two
        /// scoped skips:
        ///   - datacenter branch is skipped iff is_datacenter_exempt(addr).
        ///   - subnet/ASN branches are skipped iff is_trusted(addr, epoch).
        /// Same-IP, duplicate-fingerprint and VPN/proxy are NEVER skipped.
        pub fn check_at_epoch(&self, addr: &Address, current_epoch: u64) -> ComplianceStatus {
            let Some(ip) = self.node_to_ip.get(addr) else {
                return ComplianceStatus::Compliant;
            };

            // Duplicate fingerprint -> adversarial. NEVER exempted.
            if let Some(hash) = self.fingerprints.get(addr) {
                for (other_addr, other_hash) in &self.fingerprints {
                    if other_addr != addr && other_hash == hash {
                        return ComplianceStatus::NerfedAdversarial;
                    }
                }
            }

            // Datacenter IP -> incidental. SKIPPED iff genesis-exempt.
            if Self::is_datacenter_ip(ip) && !self.is_datacenter_exempt(addr) {
                return ComplianceStatus::NerfedIncidental;
            }

            // VPN/proxy (>3 behind one IP) -> adversarial. NEVER exempted.
            if self.ip_validator_count(ip) > 3 {
                return ComplianceStatus::NerfedAdversarial;
            }

            // Trust slice: a 720-clean-epoch validator sheds the INCIDENTAL
            // subnet/ASN nerf only. Co-location (exact same IP) is still
            // caught below regardless of trust.
            let trusted = self.is_trusted(addr, current_epoch);

            let subnet24 = subnet_24(ip);
            let subnet16 = subnet_16(ip);

            for (other_addr, other_ip) in &self.node_to_ip {
                if other_addr == addr { continue; }

                // Exact same IP -> incidental. NEVER exempted (real
                // same-machine / same-NAT co-location).
                if other_ip == ip {
                    return ComplianceStatus::NerfedIncidental;
                }

                if !trusted {
                    // Same /24 -> incidental (trust sheds this).
                    if let (Some(s), Some(other_s)) = (&subnet24, subnet_24(other_ip)) {
                        if *s == other_s {
                            return ComplianceStatus::NerfedIncidental;
                        }
                    }
                    // Same /16 -> incidental (trust sheds this).
                    if let (Some(s), Some(other_s)) = (&subnet16, subnet_16(other_ip)) {
                        if *s == other_s {
                            return ComplianceStatus::NerfedIncidental;
                        }
                    }
                }
            }

            // Same ASN -> incidental (trust sheds this).
            if !trusted {
                if let Some(asn) = self.node_asn.get(addr) {
                    for (other_addr, other_asn) in &self.node_asn {
                        if other_addr != addr && other_asn == asn {
                            return ComplianceStatus::NerfedIncidental;
                        }
                    }
                }
            }

            ComplianceStatus::Compliant
        }

        /// Back-compat wrapper. Existing callers (incl. the PROTECTED
        /// event_loop.rs:437) keep working unchanged. Trust never applies
        /// here because the sentinel epoch (0) makes is_trusted false.
        pub fn check(&self, addr: &Address) -> ComplianceStatus {
            self.check_at_epoch(addr, NO_EPOCH_SENTINEL)
        }
    }
}

// =====================================================================
//  TESTS — real behavior against the live types. After merging the
//  field + methods into compliance_check.rs, paste these into its
//  `#[cfg(test)] mod tests` block (they call the REAL ComplianceChecker,
//  not the reference scaffolding above). They use only Address([n;32])
//  and existing public methods (register_node, register_fingerprint,
//  register_asn, mark_clean_epoch) — all confirmed to exist.
//
//  Replace `use super::*;` context: in the real test module `addr(n)`
//  helper already exists (compliance_check.rs:625). These tests assume
//  `set_datacenter_exempt`, `is_datacenter_exempt`, `check_at_epoch`,
//  and the back-compat `check` are present on ComplianceChecker.
// =====================================================================
#[cfg(test)]
mod exemption_tests {
    // NOTE FOR FOUNDER: when these move into compliance_check.rs, change
    // the next line to `use super::*;` and delete the explicit imports.
    use commputer_core::identity::Address;
    use commputer_core::compliance::ComplianceStatus;
    use commputer_validator::compliance_check::ComplianceChecker;

    fn addr(n: u8) -> Address { Address([n; 32]) }

    // ---- (b) genesis datacenter exemption ----

    #[test]
    fn datacenter_ip_is_nerfed_without_exemption() {
        // Baseline: a node on an AWS prefix is NerfedIncidental (current
        // behavior, line 516). 3.x is in the AWS table.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "3.5.10.20".into());
        assert_eq!(c.check(&addr(1)), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn datacenter_exemption_sheds_only_the_datacenter_nerf() {
        // A genesis-exempt founder/seed on a cloud IP, ALONE on that IP,
        // becomes Compliant.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "3.5.10.20".into());
        c.set_datacenter_exempt(addr(1));
        assert!(c.is_datacenter_exempt(&addr(1)));
        assert_eq!(c.check(&addr(1)), ComplianceStatus::Compliant);
    }

    #[test]
    fn exemption_does_not_shed_same_ip_collision() {
        // ANTI-ABUSE: two nodes (one exempt) on the EXACT same cloud IP
        // must STILL be NerfedIncidental — the exemption cannot be used to
        // stand up a second box on the same machine/IP.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "3.5.10.20".into());
        c.register_node(addr(2), "3.5.10.20".into());
        c.set_datacenter_exempt(addr(1));
        c.set_datacenter_exempt(addr(2));
        assert_eq!(c.check(&addr(1)), ComplianceStatus::NerfedIncidental);
        assert_eq!(c.check(&addr(2)), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn exemption_does_not_shed_duplicate_fingerprint() {
        // ANTI-ABUSE: identical hardware fingerprint across two nodes is
        // NerfedAdversarial even if both are datacenter-exempt.
        let mut c = ComplianceChecker::new();
        // Put them on DIFFERENT, non-datacenter IPs so the only thing the
        // fingerprint test isolates is the fingerprint branch.
        c.register_node(addr(1), "100.64.0.1".into());
        c.register_node(addr(2), "203.0.113.7".into());
        c.set_datacenter_exempt(addr(1));
        c.set_datacenter_exempt(addr(2));
        let h = [7u8; 32];
        c.register_fingerprint(addr(1), h);
        c.register_fingerprint(addr(2), h);
        assert_eq!(c.check(&addr(1)), ComplianceStatus::NerfedAdversarial);
        assert_eq!(c.check(&addr(2)), ComplianceStatus::NerfedAdversarial);
    }

    #[test]
    fn exemption_does_not_shed_vpn_proxy() {
        // ANTI-ABUSE: >3 validators behind one IP is NerfedAdversarial even
        // if the queried node is datacenter-exempt. Use a cloud IP and
        // exempt addr(1): without the VPN guard it would be Compliant; the
        // VPN/proxy branch must override.
        let mut c = ComplianceChecker::new();
        for i in 1..=4u8 { c.register_node(addr(i), "3.5.10.20".into()); }
        c.set_datacenter_exempt(addr(1));
        // 4 behind one IP: same-IP branch fires first for a non-exempt
        // peer, but for addr(1) the >3 VPN/proxy branch must yield
        // NerfedAdversarial (strictly worse than Incidental), proving the
        // exemption did not buy a clean status.
        assert_eq!(c.check(&addr(1)), ComplianceStatus::NerfedAdversarial);
    }

    #[test]
    fn empty_exemption_set_is_status_quo() {
        // With no exemptions and no epoch wiring, check() == today.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "88.198.1.1".into()); // Hetzner
        assert_eq!(c.check(&addr(1)), ComplianceStatus::NerfedIncidental);
        c.register_node(addr(2), "192.168.1.10".into());
        c.register_node(addr(3), "192.168.1.11".into()); // same /24
        assert_eq!(c.check(&addr(2)), ComplianceStatus::NerfedIncidental);
    }

    // ---- (a-slice) trust sheds subnet/ASN only ----

    #[test]
    fn untrusted_same_subnet_is_nerfed_via_check_at_epoch() {
        // Baseline through the epoch-aware path: two nodes same /24, not
        // trusted -> NerfedIncidental.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "100.64.1.10".into());
        c.register_node(addr(2), "100.64.1.11".into());
        assert_eq!(c.check_at_epoch(&addr(1), 50), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn trusted_validator_sheds_subnet_nerf() {
        // addr(1) clean since epoch 100; at epoch 900 (>=720 later) it is
        // trusted and sheds the same-/24 incidental nerf.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "100.64.1.10".into());
        c.register_node(addr(2), "100.64.1.11".into()); // same /24
        c.mark_clean_epoch(addr(1), 100);
        // Not yet trusted at epoch 500 -> still nerfed.
        assert_eq!(c.check_at_epoch(&addr(1), 500), ComplianceStatus::NerfedIncidental);
        // Trusted at epoch 900 -> subnet nerf shed.
        assert!(c.is_trusted(&addr(1), 900));
        assert_eq!(c.check_at_epoch(&addr(1), 900), ComplianceStatus::Compliant);
    }

    #[test]
    fn trusted_validator_sheds_asn_nerf() {
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "100.64.1.10".into());
        c.register_node(addr(2), "203.0.113.9".into()); // different subnet
        c.register_asn(addr(1), "AS64500".into());
        c.register_asn(addr(2), "AS64500".into());
        c.mark_clean_epoch(addr(1), 0);
        assert_eq!(c.check_at_epoch(&addr(1), 100), ComplianceStatus::NerfedIncidental);
        assert_eq!(c.check_at_epoch(&addr(1), 720), ComplianceStatus::Compliant);
    }

    #[test]
    fn trust_does_not_shed_same_ip_collision() {
        // ANTI-ABUSE: trust relaxes subnet/ASN but NOT exact-same-IP
        // co-location. Two trusted nodes on the SAME IP stay nerfed.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "100.64.1.10".into());
        c.register_node(addr(2), "100.64.1.10".into()); // identical IP
        c.mark_clean_epoch(addr(1), 0);
        assert!(c.is_trusted(&addr(1), 1000));
        assert_eq!(c.check_at_epoch(&addr(1), 1000), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn trust_does_not_shed_datacenter_nerf() {
        // ANTI-ABUSE: a 720-clean node on a cloud IP is STILL nerfed by the
        // datacenter branch — trust does NOT touch it. Only the genesis
        // exemption can shed the datacenter nerf.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "3.5.10.20".into()); // AWS
        c.mark_clean_epoch(addr(1), 0);
        assert!(c.is_trusted(&addr(1), 1000));
        assert_eq!(c.check_at_epoch(&addr(1), 1000), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn check_wrapper_never_applies_trust() {
        // The back-compat check() passes the sentinel epoch, so trust never
        // applies even for a long-clean validator. Guarantees existing
        // call sites keep their current (conservative) behavior.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "100.64.1.10".into());
        c.register_node(addr(2), "100.64.1.11".into()); // same /24
        c.mark_clean_epoch(addr(1), 0);
        // Even though it WOULD be trusted at a real high epoch, check()
        // uses the sentinel -> still nerfed.
        assert_eq!(c.check(&addr(1)), ComplianceStatus::NerfedIncidental);
    }

    #[test]
    fn combined_exempt_and_trusted_on_cloud_with_noisy_neighbor() {
        // The genuine founder-seed case: genesis-exempt AND long-clean, on
        // a cloud IP, with an unrelated neighbor in the same /16. Exemption
        // sheds the datacenter nerf; trust sheds the /16 nerf -> Compliant.
        let mut c = ComplianceChecker::new();
        c.register_node(addr(1), "3.5.10.20".into());   // AWS, exempt
        c.register_node(addr(2), "3.5.99.99".into());   // same /16, NOT same /24
        c.set_datacenter_exempt(addr(1));
        c.mark_clean_epoch(addr(1), 0);
        // addr(2) also on AWS would be nerfed by datacenter branch, but we
        // only assert about addr(1).
        assert_eq!(c.check_at_epoch(&addr(1), 1000), ComplianceStatus::Compliant);
    }
}
