use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Chain ID constants.
pub const TESTNET_CHAIN_ID: &str = "commputer-testnet-1";
pub const MAINNET_CHAIN_ID: &str = "commputer-mainnet-1";

/// Genesis configuration — defines the initial parameters for a Commputer chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisConfig {
    /// Human-readable chain identifier (e.g., "commputer-testnet-1").
    pub chain_id: String,
    /// Total token supply in raw units.
    pub total_supply: u64,
    /// Duration of one epoch in seconds (Item 8).
    pub epoch_duration_secs: u64,
    /// Base emission rate per epoch in raw units (Item 10).
    pub emission_base_rate: u64,
    /// Floor emission rate per epoch in raw units (Item 10).
    pub emission_floor_rate: u64,
    /// Per-channel floor allocation ratios (channel name -> fraction 0.0..1.0) (Item 11).
    #[serde(default)]
    pub channel_floors: HashMap<String, f64>,
    /// Item 9: Proof challenge interval in seconds (default 300 = 5 minutes).
    #[serde(default = "default_proof_interval")]
    pub proof_challenge_interval_secs: u64,
    /// Item 12: Block production interval in seconds (default 2).
    #[serde(default = "default_block_time")]
    pub block_time_secs: u64,
    /// Item 10: Emission decay rate per epoch (0.0..1.0, default 0.0001).
    #[serde(default = "default_emission_decay")]
    pub emission_decay_rate: f64,
    /// Genesis timestamp (Item 5). If 0, uses current time on first boot.
    #[serde(default)]
    pub genesis_timestamp: u64,
}

fn default_proof_interval() -> u64 { 300 }
fn default_block_time() -> u64 { 2 }
fn default_emission_decay() -> f64 { 0.0001 }

impl Default for GenesisConfig {
    fn default() -> Self {
        default_genesis()
    }
}

/// Returns a hardcoded default genesis configuration matching current testnet values.
pub fn default_genesis() -> GenesisConfig {
    let mut channel_floors = HashMap::new();
    channel_floors.insert("Processing".to_string(), 0.20);
    channel_floors.insert("Gpu".to_string(), 0.20);
    channel_floors.insert("Storage".to_string(), 0.20);
    channel_floors.insert("Ram".to_string(), 0.20);
    channel_floors.insert("Bandwidth".to_string(), 0.20);

    GenesisConfig {
        chain_id: TESTNET_CHAIN_ID.to_string(),
        total_supply: crate::token::TOTAL_SUPPLY,
        epoch_duration_secs: 3600,
        emission_base_rate: 100 * crate::token::UNITS_PER_COMME,
        emission_floor_rate: 10 * crate::token::UNITS_PER_COMME,
        channel_floors,
        proof_challenge_interval_secs: 300,
        block_time_secs: 2,
        emission_decay_rate: 0.0001,
        genesis_timestamp: 1647907200, // 2022-03-22 00:00:00 UTC
    }
}

/// Load a genesis configuration from a JSON file.
pub fn load_genesis(path: &Path) -> Result<GenesisConfig, String> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read genesis file {}: {}", path.display(), e))?;
    serde_json::from_str(&data)
        .map_err(|e| format!("failed to parse genesis JSON: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_serialize_deserialize() {
        let config = default_genesis();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: GenesisConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.chain_id, config.chain_id);
        assert_eq!(parsed.total_supply, config.total_supply);
        assert_eq!(parsed.epoch_duration_secs, config.epoch_duration_secs);
        assert_eq!(parsed.emission_base_rate, config.emission_base_rate);
        assert_eq!(parsed.emission_floor_rate, config.emission_floor_rate);
        assert_eq!(parsed.channel_floors.len(), config.channel_floors.len());
        for (k, v) in &config.channel_floors {
            assert_eq!(parsed.channel_floors.get(k), Some(v));
        }
    }

    #[test]
    fn default_genesis_has_testnet_chain_id() {
        let config = default_genesis();
        assert_eq!(config.chain_id, TESTNET_CHAIN_ID);
    }

    #[test]
    fn load_genesis_missing_file_returns_error() {
        let result = load_genesis(Path::new("/tmp/nonexistent_genesis_12345.json"));
        assert!(result.is_err());
    }
}
