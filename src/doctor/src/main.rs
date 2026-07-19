// commputer-doctor — pre-launch validator for operator config + genesis
//
// WHAT IT DOES:
//   Standalone CLI that runs sanity checks BEFORE node startup so operators do
//   not silently brick themselves. Validates the TOML config, the genesis JSON,
//   public-IP datacenter classification (mirrors compliance_check.rs:291-352),
//   NTP drift, P2P port reachability, and version pinning.
//
// WHERE IT SHOULD GO:
//   1. Add a new binary crate under src/doctor/ with this file as src/main.rs.
//   2. Update the workspace Cargo.toml `members` array to include "doctor".
//   3. Add a new doctor crate Cargo.toml with deps: serde, serde_json, toml,
//      clap (cli derive), chrono. Optional: trust-dns-resolver for SNTP.
//   4. (Optional) Wire `commputer-doctor` to be invoked from `commputer-node`
//      pre-flight in src/node/src/main.rs — if the doctor exits non-zero with
//      `--strict` the node refuses to start.
//
// WIRING REQUIRED:
//   - This binary MUST stay independent of node internals. Do not import the
//     node crate; it is intended to run before the node binary is even on PATH.
//   - The CIDR list lives in checks/cloud_ip.rs as a self-contained copy with a
//     citation comment back to src/validator/src/compliance_check.rs:291-352.
//
// EXIT CODES:
//   0 = all checks pass (no warnings)
//   1 = warnings only (operator should review; node MAY start)
//   2 = at least one error (operator MUST fix; node MUST refuse to launch)
//
// USAGE:
//   commputer-doctor --config ~/.commputer/config.toml --genesis /etc/commputer/genesis.json
//   commputer-doctor --check-public-ip 1.2.3.4 --json
//   commputer-doctor --strict   # any warning becomes fatal (exit 2)

mod checks;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

// ----------------------------------------------------------------------------
// Severity & CheckResult
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "[OK]",
            Severity::Warning => "[WARN]",
            Severity::Error => "[FAIL]",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub check: String,
    pub severity: Severity,
    pub message: String,
    pub suggestion: Option<String>,
}

impl CheckResult {
    pub fn ok(check: &str, msg: impl Into<String>) -> Self {
        Self { check: check.into(), severity: Severity::Info, message: msg.into(), suggestion: None }
    }
    pub fn warn(check: &str, msg: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { check: check.into(), severity: Severity::Warning, message: msg.into(), suggestion: Some(hint.into()) }
    }
    pub fn err(check: &str, msg: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { check: check.into(), severity: Severity::Error, message: msg.into(), suggestion: Some(hint.into()) }
    }

    pub fn is_fatal(&self) -> bool { self.severity == Severity::Error }

    pub fn format_line(&self) -> String {
        let base = format!("{:6} {:<28}  {}", self.severity.label(), self.check, self.message);
        match &self.suggestion {
            Some(s) => format!("{}\n         -> {}", base, s),
            None => base,
        }
    }
}

// ----------------------------------------------------------------------------
// Parsed operator-facing config (mirrors src/node/src/config.rs::NodeConfig)
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct OperatorConfig {
    pub network: String,
    pub chain_id: String,
    pub seeds: Vec<String>,
    pub port: u16,
    pub rpc_port: u16,
    pub rpc_bind: String,
    pub epoch_duration: u64,
    pub contribution_percent: u8,
    pub log_level: String,
    pub cors_origins: String,
}

// DRIFT WARNING: this crate deliberately does NOT depend on `commputer-core` (it stays
// dependency-light and independent of node/core internals so it can validate an operator's
// config before the node binary is even built — see the header comment above). That means
// the chain-id literals below are NOT derived from `commputer_core::genesis::TESTNET_CHAIN_ID`
// and must be bumped by hand whenever that const changes (last synced: "commputer-testnet-3",
// 2026-07-19 go-live batch Task E). If a `doctor` run and the real chain disagree on the
// expected chain-id, check here first.
impl Default for OperatorConfig {
    fn default() -> Self {
        Self {
            network: "testnet".into(),
            chain_id: "commputer-testnet-3".into(),
            seeds: vec!["seed.commputer.xyz:9000".into()],
            port: 9000,
            rpc_port: 9944,
            rpc_bind: "127.0.0.1".into(),
            epoch_duration: 60,
            contribution_percent: 100,
            log_level: "info".into(),
            cors_origins: "*".into(),
        }
    }
}

// ----------------------------------------------------------------------------
// Args (hand-rolled — keeps the doctor crate dependency-light)
// ----------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    genesis: Option<PathBuf>,
    check_public_ip: Option<String>,
    json: bool,
    strict: bool,
    skip_net: bool,
    binary_version: Option<String>,
    expected_chain_id: Option<String>,
    help: bool,
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let a = raw[i].as_str();
        match a {
            "-h" | "--help" => args.help = true,
            "--config" => { i += 1; args.config = raw.get(i).map(PathBuf::from); }
            "--genesis" => { i += 1; args.genesis = raw.get(i).map(PathBuf::from); }
            "--check-public-ip" => { i += 1; args.check_public_ip = raw.get(i).cloned(); }
            "--binary-version" => { i += 1; args.binary_version = raw.get(i).cloned(); }
            "--expected-chain-id" => { i += 1; args.expected_chain_id = raw.get(i).cloned(); }
            "--json" => args.json = true,
            "--strict" => args.strict = true,
            "--skip-net" => args.skip_net = true,
            _ => eprintln!("commputer-doctor: unknown arg '{}' (ignored)", a),
        }
        i += 1;
    }
    args
}

fn print_help() {
    println!("commputer-doctor — pre-launch validator");
    println!();
    println!("USAGE:");
    println!("  commputer-doctor [--config <path>] [--genesis <path>] [flags]");
    println!();
    println!("FLAGS:");
    println!("  --config <path>          path to TOML config (default: ~/.commputer/config.toml)");
    println!("  --genesis <path>         path to genesis.json (default: ./genesis.json)");
    println!("  --check-public-ip <ip>   classify a single public IP and exit");
    println!("  --binary-version <v>     binary version string for protocol-pin check");
    println!("  --expected-chain-id <s>  override expected chain_id");
    println!("  --skip-net               skip outbound NTP / port-bind probes");
    println!("  --strict                 treat warnings as errors (exit 2)");
    println!("  --json                   machine-readable output");
    println!("  -h, --help               this help");
    println!();
    println!("EXIT:");
    println!("  0 = clean    1 = warnings    2 = errors");
}

// ----------------------------------------------------------------------------
// Default paths
// ----------------------------------------------------------------------------

fn default_config_path() -> PathBuf {
    if let Some(home) = dirs_home() {
        home.join(".commputer").join("config.toml")
    } else {
        PathBuf::from("./commputer.toml")
    }
}

fn default_genesis_path() -> PathBuf {
    PathBuf::from("./genesis.json")
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ----------------------------------------------------------------------------
// Config check
// ----------------------------------------------------------------------------

fn load_and_check_config(path: &Path, results: &mut Vec<CheckResult>) -> Option<OperatorConfig> {
    if !path.exists() {
        results.push(CheckResult::warn(
            "config.exists",
            format!("config file not found at {}", path.display()),
            "node will boot with built-in defaults; create the file to lock down behavior",
        ));
        return Some(OperatorConfig::default());
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            results.push(CheckResult::err(
                "config.read",
                format!("could not read {}: {}", path.display(), e),
                "fix file permissions or correct the --config path",
            ));
            return None;
        }
    };
    match toml::from_str::<OperatorConfig>(&raw) {
        Ok(cfg) => {
            results.push(CheckResult::ok("config.parse", format!("parsed {}", path.display())));
            validate_config_values(&cfg, results);
            Some(cfg)
        }
        Err(e) => {
            results.push(CheckResult::err(
                "config.parse",
                format!("TOML parse error: {}", e),
                "compare your file against commputer.toml in the repo",
            ));
            None
        }
    }
}

fn validate_config_values(cfg: &OperatorConfig, results: &mut Vec<CheckResult>) {
    // network
    match cfg.network.as_str() {
        "mainnet" | "testnet" => results.push(CheckResult::ok(
            "config.network",
            format!("network='{}'", cfg.network),
        )),
        other => results.push(CheckResult::err(
            "config.network",
            format!("unknown network '{}'", other),
            "must be 'mainnet' or 'testnet'",
        )),
    }

    // chain_id
    if cfg.chain_id.is_empty() {
        results.push(CheckResult::err(
            "config.chain_id",
            "chain_id is empty",
            "set chain_id = \"commputer-testnet-3\" or the mainnet equivalent",
        ));
    } else if cfg.network == "mainnet" && cfg.chain_id.contains("testnet") {
        results.push(CheckResult::err(
            "config.chain_id",
            format!("chain_id '{}' looks like a testnet id but network=mainnet", cfg.chain_id),
            "fix one of network or chain_id — they MUST match",
        ));
    } else {
        results.push(CheckResult::ok("config.chain_id", format!("chain_id='{}'", cfg.chain_id)));
    }

    // ports
    if cfg.port == cfg.rpc_port {
        results.push(CheckResult::err(
            "config.ports",
            format!("port and rpc_port both set to {}", cfg.port),
            "use different values; e.g. port=9000 rpc_port=9944",
        ));
    } else {
        results.push(CheckResult::ok(
            "config.ports",
            format!("p2p={} rpc={}", cfg.port, cfg.rpc_port),
        ));
    }
    if cfg.port < 1024 {
        results.push(CheckResult::warn(
            "config.port.privileged",
            format!("p2p port {} is in the privileged range", cfg.port),
            "binding requires root; pick something >= 1024",
        ));
    }
    if cfg.rpc_port < 1024 {
        results.push(CheckResult::warn(
            "config.rpc_port.privileged",
            format!("rpc port {} is in the privileged range", cfg.rpc_port),
            "binding requires root; pick something >= 1024",
        ));
    }

    // rpc_bind
    match cfg.rpc_bind.as_str() {
        "127.0.0.1" | "localhost" => results.push(CheckResult::ok(
            "config.rpc_bind",
            "RPC bound to localhost (safe default)",
        )),
        "0.0.0.0" => results.push(CheckResult::warn(
            "config.rpc_bind",
            "RPC bound to 0.0.0.0 — exposed to the public internet",
            "front it with TLS + auth, or restrict via firewall",
        )),
        other => results.push(CheckResult::ok(
            "config.rpc_bind",
            format!("RPC bind='{}'", other),
        )),
    }

    // epoch_duration
    if cfg.epoch_duration < 10 {
        results.push(CheckResult::err(
            "config.epoch_duration",
            format!("epoch_duration={}s is too short", cfg.epoch_duration),
            "minimum 10s; testnet typically 60s, mainnet typically 3600s",
        ));
    } else if cfg.network == "mainnet" && cfg.epoch_duration < 60 {
        results.push(CheckResult::warn(
            "config.epoch_duration",
            format!("epoch_duration={}s is unusually short for mainnet", cfg.epoch_duration),
            "mainnet typically uses 3600s (1h)",
        ));
    } else {
        results.push(CheckResult::ok(
            "config.epoch_duration",
            format!("{}s", cfg.epoch_duration),
        ));
    }

    // contribution_percent
    if cfg.contribution_percent == 0 || cfg.contribution_percent > 100 {
        results.push(CheckResult::err(
            "config.contribution_percent",
            format!("contribution_percent={} is out of range", cfg.contribution_percent),
            "must be 1..=100",
        ));
    } else if cfg.contribution_percent < 25 {
        results.push(CheckResult::warn(
            "config.contribution_percent",
            format!("contribution_percent={} is low — may underperform peers", cfg.contribution_percent),
            "raise to >= 50 unless you are intentionally throttling",
        ));
    } else {
        results.push(CheckResult::ok(
            "config.contribution_percent",
            format!("{}%", cfg.contribution_percent),
        ));
    }

    // log_level
    match cfg.log_level.as_str() {
        "trace" | "debug" | "info" | "warn" | "error" => results.push(CheckResult::ok(
            "config.log_level",
            format!("'{}'", cfg.log_level),
        )),
        other => results.push(CheckResult::err(
            "config.log_level",
            format!("unknown log_level '{}'", other),
            "use one of: trace, debug, info, warn, error",
        )),
    }

    // seeds
    if cfg.seeds.is_empty() {
        results.push(CheckResult::warn(
            "config.seeds",
            "no seed nodes configured",
            "node will be isolated until peers are added; set `seeds = [...]`",
        ));
    } else {
        let mut bad: Vec<&str> = Vec::new();
        for s in &cfg.seeds {
            if !s.contains(':') {
                bad.push(s);
            }
        }
        if !bad.is_empty() {
            results.push(CheckResult::err(
                "config.seeds",
                format!("malformed seed entries: {:?}", bad),
                "expected host:port (e.g. seed.commputer.xyz:9000)",
            ));
        } else {
            results.push(CheckResult::ok(
                "config.seeds",
                format!("{} seed(s) configured", cfg.seeds.len()),
            ));
        }
    }

    // cors
    if cfg.cors_origins == "*" {
        results.push(CheckResult::warn(
            "config.cors",
            "cors_origins='*' allows any origin to call your RPC",
            "narrow to specific origins for production",
        ));
    } else {
        results.push(CheckResult::ok("config.cors", "narrow CORS configured"));
    }
}

// ----------------------------------------------------------------------------
// Render
// ----------------------------------------------------------------------------

fn render_human(results: &[CheckResult]) {
    println!("================ commputer-doctor ================");
    for r in results {
        println!("{}", r.format_line());
    }
    let (errs, warns, oks) = tally(results);
    println!("--------------------------------------------------");
    println!("summary: {} OK, {} WARN, {} FAIL", oks, warns, errs);
}

fn render_json(results: &[CheckResult]) {
    // Hand-rolled JSON to keep deps minimal; quote-escape conservatively.
    print!("[");
    for (i, r) in results.iter().enumerate() {
        if i > 0 { print!(","); }
        let sev = match r.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        let suggestion = match &r.suggestion {
            Some(s) => format!("\"{}\"", json_escape(s)),
            None => "null".into(),
        };
        print!(
            "{{\"check\":\"{}\",\"severity\":\"{}\",\"message\":\"{}\",\"suggestion\":{}}}",
            json_escape(&r.check), sev, json_escape(&r.message), suggestion,
        );
    }
    println!("]");
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn tally(results: &[CheckResult]) -> (usize, usize, usize) {
    let mut e = 0;
    let mut w = 0;
    let mut o = 0;
    for r in results {
        match r.severity {
            Severity::Error => e += 1,
            Severity::Warning => w += 1,
            Severity::Info => o += 1,
        }
    }
    (e, w, o)
}

// ----------------------------------------------------------------------------
// Entry point
// ----------------------------------------------------------------------------

fn main() -> ExitCode {
    let args = parse_args();
    if args.help {
        print_help();
        return ExitCode::from(0);
    }

    let mut results: Vec<CheckResult> = Vec::new();

    // Single-shot CIDR classifier.
    if let Some(ip) = &args.check_public_ip {
        results.push(checks::cloud_ip::classify_ip(ip));
        if args.json { render_json(&results); } else { render_human(&results); }
        return final_exit(&results, args.strict);
    }

    let config_path = args.config.unwrap_or_else(default_config_path);
    let genesis_path = args.genesis.unwrap_or_else(default_genesis_path);

    let cfg = load_and_check_config(&config_path, &mut results);

    // Genesis
    checks::genesis::check_genesis(
        &genesis_path,
        cfg.as_ref(),
        args.expected_chain_id.as_deref(),
        args.binary_version.as_deref(),
        &mut results,
    );

    // Network checks (skippable in CI)
    if !args.skip_net {
        if let Some(c) = &cfg {
            results.push(checks::port_reachability::check_p2p_port(c.port));
            results.push(checks::port_reachability::check_rpc_port(c.rpc_port));
        }
        results.push(checks::ntp::check_ntp_drift());
        results.push(checks::cloud_ip::check_local_public_ip());
    } else {
        results.push(CheckResult::ok("net.skip", "network checks skipped (--skip-net)"));
    }

    if args.json { render_json(&results); } else { render_human(&results); }
    final_exit(&results, args.strict)
}

fn final_exit(results: &[CheckResult], strict: bool) -> ExitCode {
    let (errs, warns, _) = tally(results);
    if errs > 0 { return ExitCode::from(2); }
    if warns > 0 { return ExitCode::from(if strict { 2 } else { 1 }); }
    ExitCode::from(0)
}
