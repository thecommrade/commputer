#![allow(dead_code)]
//! Node configuration — reads from ~/.commputer/config.toml with CLI overrides.

use std::path::PathBuf;
use serde::Deserialize;

/// Default testnet seed nodes.
/// Update these to your seed node's address before running.
pub const DEFAULT_TESTNET_SEEDS: &[&str] = &["seed.commputer.xyz:9000"];

/// Default chain ID for testnet.
pub const DEFAULT_TESTNET_CHAIN_ID: &str = "commputer-testnet-3";

/// Commputer node configuration file format.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
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
    /// Track-2 PoUW executor auto-claim loop (OFF by default).
    pub executor: ExecutorConfig,
    /// Track-2 PoUW verifier commit/reveal loop (OFF by default).
    pub verifier: VerifierConfig,
    /// Track-2 DA backend — serve/publish coded chunks over /commputer/da/1 (OFF by default).
    pub da: DaConfig,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            network: "testnet".to_string(),
            chain_id: DEFAULT_TESTNET_CHAIN_ID.to_string(),
            seeds: DEFAULT_TESTNET_SEEDS.iter().map(|s| s.to_string()).collect(),
            port: 9000,
            rpc_port: 9944,
            rpc_bind: "127.0.0.1".to_string(),
            epoch_duration: 60,
            contribution_percent: 100,
            log_level: "info".to_string(),
            cors_origins: "*".to_string(),
            executor: ExecutorConfig::default(),
            verifier: VerifierConfig::default(),
            da: DaConfig::default(),
        }
    }
}

/// Track-2 PoUW executor auto-claim loop config (Phase B). OFF by default — a node
/// with `enabled=false` and `auto_enable_when_bonded=false` never runs the loop and
/// is byte-identical on-chain to today.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ExecutorConfig {
    pub enabled: bool,
    pub auto_enable_when_bonded: bool,
    pub max_concurrent_claims: usize,
    pub min_balance_reserve: u64,
}
impl Default for ExecutorConfig {
    fn default() -> Self {
        Self { enabled: false, auto_enable_when_bonded: true, max_concurrent_claims: 4, min_balance_reserve: 0 }
    }
}

/// Track-2 PoUW verifier commit/reveal loop config (Phase B). OFF by default.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VerifierConfig {
    pub enabled: bool,
    pub auto_enable_when_bonded: bool,
    pub min_balance_reserve: u64,
}
impl Default for VerifierConfig {
    fn default() -> Self {
        Self { enabled: false, auto_enable_when_bonded: true, min_balance_reserve: 0 }
    }
}

/// Track-2 DA backend config (Phase B). OFF by default. `fetch_timeout_ms` bounds
/// each BridgeTransport DA call (P11) so a loop never blocks unboundedly.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaConfig {
    pub enabled: bool,
    pub fetch_timeout_ms: u64,
}
impl Default for DaConfig {
    fn default() -> Self {
        Self { enabled: false, fetch_timeout_ms: 5_000 }
    }
}

impl NodeConfig {
    /// Load config from ~/.commputer/config.toml if it exists, otherwise use defaults.
    pub fn load() -> Self {
        let config_path = config_path();
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(contents) => match toml::from_str::<NodeConfig>(&contents) {
                    Ok(config) => {
                        tracing::info!("Loaded config from {}", config_path.display());
                        return config;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse config file: {}. Using defaults.", e);
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read config file: {}. Using defaults.", e);
                }
            }
        }
        Self::default()
    }
}

/// Path to the config file: ~/.commputer/config.toml
pub fn config_path() -> PathBuf {
    commputer_dir().join("config.toml")
}

/// Base directory: ~/.commputer/
pub fn commputer_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".commputer")
}

/// Wallet directory: ~/.commputer/wallet/
pub fn wallet_dir() -> PathBuf {
    commputer_dir().join("wallet")
}

/// Chain data directory: ~/.commputer/testnet/ or ~/.commputer/mainnet/
pub fn data_dir(testnet: bool) -> PathBuf {
    if testnet {
        commputer_dir().join("testnet")
    } else {
        commputer_dir().join("mainnet")
    }
}

/// Peer key path: ~/.commputer/peer_id
pub fn peer_key_path() -> PathBuf {
    commputer_dir().join("peer_id")
}

/// Ensure the ~/.commputer/ directory structure exists.
pub fn ensure_dirs(testnet: bool) {
    let base = commputer_dir();
    let _ = std::fs::create_dir_all(&base);
    let _ = std::fs::create_dir_all(wallet_dir());
    let _ = std::fs::create_dir_all(data_dir(testnet));
}
