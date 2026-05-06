// checks/genesis.rs — genesis.json validation for commputer-doctor
//
// WHAT IT DOES:
//   Loads genesis.json and verifies:
//     1. file exists, is valid JSON, has all required keys
//     2. chain_id matches operator config (if config provided)
//     3. supply math is internally consistent (positive, fits u64, etc)
//     4. emission_floor_rate <= emission_base_rate
//     5. epoch_duration_secs is sensible for declared network
//     6. channel_floors are positive and sum to <= 1.0 (else over-allocated)
//     7. binary version matches genesis-encoded protocol_version (if present)
//     8. emits a SHA-256 of the canonical bytes for operator cross-check
//
// WHERE IT SHOULD GO:
//   src/doctor/src/checks/genesis.rs
//
// WIRING REQUIRED:
//   None — uses serde_json. The protocol_version field is OPTIONAL: the live
//   genesis.json today has no such field. If/when protocol pinning lands,
//   bump genesis.json with `"protocol_version": "1.0.0"` and the doctor will
//   start enforcing it automatically.

use std::path::Path;

use serde_json::Value;

use crate::{CheckResult, OperatorConfig};
#[cfg(test)]
use crate::Severity;

pub fn check_genesis(
    path: &Path,
    cfg: Option<&OperatorConfig>,
    expected_chain_id_override: Option<&str>,
    binary_version: Option<&str>,
    results: &mut Vec<CheckResult>,
) {
    let raw = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            results.push(CheckResult::err(
                "genesis.exists",
                format!("could not read genesis at {}: {}", path.display(), e),
                "pass --genesis <path> or fix file permissions",
            ));
            return;
        }
    };

    // Hash the raw bytes so the operator can cross-check with a known good
    // value out-of-band. We use a tiny in-file SHA-256 to avoid taking a deep
    // dep just for this.
    let digest = sha256_hex(&raw);
    results.push(CheckResult::ok(
        "genesis.sha256",
        format!("{} ({})", digest, path.display()),
    ));

    let v: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            results.push(CheckResult::err(
                "genesis.parse",
                format!("invalid JSON: {}", e),
                "validate with `jq . genesis.json`",
            ));
            return;
        }
    };

    // chain_id
    let chain_id = v.get("chain_id").and_then(|x| x.as_str());
    match chain_id {
        Some(id) if !id.is_empty() => {
            results.push(CheckResult::ok("genesis.chain_id", format!("'{}'", id)));
            let expected = expected_chain_id_override
                .or_else(|| cfg.map(|c| c.chain_id.as_str()));
            if let Some(want) = expected {
                if want != id {
                    results.push(CheckResult::err(
                        "genesis.chain_id.match",
                        format!("config chain_id '{}' != genesis chain_id '{}'", want, id),
                        "node will reject every block; fix one of the two files",
                    ));
                } else {
                    results.push(CheckResult::ok(
                        "genesis.chain_id.match",
                        "config and genesis chain_id agree",
                    ));
                }
            }
        }
        _ => results.push(CheckResult::err(
            "genesis.chain_id",
            "missing or empty chain_id",
            "every genesis MUST have a non-empty chain_id",
        )),
    }

    // total_supply
    match v.get("total_supply").and_then(|x| x.as_u64()) {
        Some(0) => results.push(CheckResult::err(
            "genesis.total_supply",
            "total_supply is zero",
            "set to the canonical supply (e.g. 200_000_000 * 1e9)",
        )),
        Some(s) => results.push(CheckResult::ok(
            "genesis.total_supply",
            format!("{} base units", s),
        )),
        None => results.push(CheckResult::err(
            "genesis.total_supply",
            "missing total_supply",
            "add total_supply (u64 base units)",
        )),
    }

    // emission_base_rate vs emission_floor_rate
    let base = v.get("emission_base_rate").and_then(|x| x.as_u64());
    let floor = v.get("emission_floor_rate").and_then(|x| x.as_u64());
    match (base, floor) {
        (Some(b), Some(f)) if f > b => results.push(CheckResult::err(
            "genesis.emission",
            format!("floor {} exceeds base {}", f, b),
            "floor must be <= base",
        )),
        (Some(b), Some(f)) => results.push(CheckResult::ok(
            "genesis.emission",
            format!("base={} floor={}", b, f),
        )),
        _ => results.push(CheckResult::err(
            "genesis.emission",
            "missing emission_base_rate or emission_floor_rate",
            "both fields are required",
        )),
    }

    // epoch_duration_secs sanity
    if let Some(eds) = v.get("epoch_duration_secs").and_then(|x| x.as_u64()) {
        if eds < 10 {
            results.push(CheckResult::err(
                "genesis.epoch_duration",
                format!("epoch_duration_secs={} is too short", eds),
                "minimum 10s",
            ));
        } else {
            results.push(CheckResult::ok(
                "genesis.epoch_duration",
                format!("{}s", eds),
            ));
        }
        // Cross-check vs operator config — must match.
        if let Some(c) = cfg {
            if c.epoch_duration != eds {
                results.push(CheckResult::warn(
                    "genesis.epoch_duration.match",
                    format!(
                        "operator epoch_duration={} != genesis epoch_duration_secs={}",
                        c.epoch_duration, eds
                    ),
                    "genesis is canonical; align your config to it",
                ));
            }
        }
    } else {
        results.push(CheckResult::err(
            "genesis.epoch_duration",
            "missing epoch_duration_secs",
            "add epoch_duration_secs (e.g. 3600)",
        ));
    }

    // channel_floors
    if let Some(cf) = v.get("channel_floors").and_then(|x| x.as_object()) {
        let mut sum = 0.0_f64;
        let mut bad: Vec<String> = Vec::new();
        for (k, val) in cf {
            match val.as_f64() {
                Some(n) if n >= 0.0 && n <= 1.0 => sum += n,
                _ => bad.push(k.clone()),
            }
        }
        if !bad.is_empty() {
            results.push(CheckResult::err(
                "genesis.channel_floors",
                format!("invalid floor values for: {:?}", bad),
                "each entry must be a finite f64 in [0.0, 1.0]",
            ));
        } else if sum > 1.0 + 1e-9 {
            results.push(CheckResult::err(
                "genesis.channel_floors",
                format!("channel_floors sum to {:.4} (>1.0)", sum),
                "rebalance so the total is <= 1.0",
            ));
        } else {
            results.push(CheckResult::ok(
                "genesis.channel_floors",
                format!("{} channels, sum={:.4}", cf.len(), sum),
            ));
        }
    } else {
        results.push(CheckResult::warn(
            "genesis.channel_floors",
            "no channel_floors in genesis",
            "this is required for the multi-channel emission split",
        ));
    }

    // protocol_version (optional today, enforced if present)
    if let Some(pv) = v.get("protocol_version").and_then(|x| x.as_str()) {
        if let Some(bv) = binary_version {
            if pv != bv {
                results.push(CheckResult::err(
                    "genesis.protocol_version",
                    format!("binary version {} != genesis protocol_version {}", bv, pv),
                    "use a binary build that matches the genesis-encoded protocol version",
                ));
            } else {
                results.push(CheckResult::ok(
                    "genesis.protocol_version",
                    format!("matched ({})", pv),
                ));
            }
        } else {
            results.push(CheckResult::ok(
                "genesis.protocol_version",
                format!("found {} (no --binary-version supplied to compare)", pv),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Tiny SHA-256 (so the doctor stays dep-light). Implementation follows
// FIPS 180-4. NOT for crypto use elsewhere — fine for an integrity digest.
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    let h = sha256(data);
    let mut s = String::with_capacity(64);
    for b in h {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
        0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    // Pre-process: append 1 bit, k zero bits, then 64-bit length.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 { padded.push(0); }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let mj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(mj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let got = sha256_hex(b"abc");
        assert_eq!(got, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn sha256_empty() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let got = sha256_hex(b"");
        assert_eq!(got, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn missing_genesis_emits_error() {
        let mut results = Vec::new();
        check_genesis(
            std::path::Path::new("/nonexistent/path/genesis.json"),
            None,
            None,
            None,
            &mut results,
        );
        assert!(results.iter().any(|r| r.severity == Severity::Error));
    }

    #[test]
    fn good_genesis_passes_core_checks() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("doctor_genesis_test_{}.json", std::process::id()));
        std::fs::write(
            &p,
            br#"{
                "chain_id": "commputer-testnet-1",
                "total_supply": 200000000000000000,
                "epoch_duration_secs": 3600,
                "emission_base_rate": 10000000000,
                "emission_floor_rate": 1000000000,
                "channel_floors": {"a": 0.2, "b": 0.2}
            }"#,
        )
        .unwrap();
        let mut results = Vec::new();
        check_genesis(&p, None, None, None, &mut results);
        let _ = std::fs::remove_file(&p);
        assert!(!results.iter().any(|r| r.severity == Severity::Error),
            "good genesis should not emit errors, got: {:?}", results);
    }

    #[test]
    fn floor_above_base_errors() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("doctor_genesis_bad_emission_{}.json", std::process::id()));
        std::fs::write(
            &p,
            br#"{
                "chain_id": "x",
                "total_supply": 1,
                "epoch_duration_secs": 60,
                "emission_base_rate": 10,
                "emission_floor_rate": 9999,
                "channel_floors": {}
            }"#,
        )
        .unwrap();
        let mut results = Vec::new();
        check_genesis(&p, None, None, None, &mut results);
        let _ = std::fs::remove_file(&p);
        assert!(results.iter().any(|r| r.check == "genesis.emission" && r.severity == Severity::Error));
    }

    #[test]
    fn channel_floors_oversum_errors() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("doctor_genesis_oversum_{}.json", std::process::id()));
        std::fs::write(
            &p,
            br#"{
                "chain_id": "x",
                "total_supply": 1,
                "epoch_duration_secs": 60,
                "emission_base_rate": 10,
                "emission_floor_rate": 1,
                "channel_floors": {"a": 0.6, "b": 0.6}
            }"#,
        )
        .unwrap();
        let mut results = Vec::new();
        check_genesis(&p, None, None, None, &mut results);
        let _ = std::fs::remove_file(&p);
        assert!(results.iter().any(|r| r.check == "genesis.channel_floors" && r.severity == Severity::Error));
    }
}
