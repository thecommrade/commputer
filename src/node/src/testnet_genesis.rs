use std::collections::HashMap;
use commputer_core::genesis::GenesisConfig;
use commputer_core::wallet::Wallet;
use commputer_core::token::UNITS_PER_COMME;
use serde::{Deserialize, Serialize};

/// A pre-funded account in the testnet genesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisAccount {
    /// Hex-encoded address.
    pub address: String,
    /// Balance in raw units.
    pub balance: u64,
}

/// Full testnet genesis output, including chain config and pre-funded accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestnetGenesis {
    pub config: GenesisConfig,
    pub accounts: Vec<GenesisAccount>,
}

/// Generate a testnet genesis configuration with `num_accounts` pre-funded accounts
/// (each with 1000 COMME) and write it to the given output path.
///
/// Uses fast epoch duration (60s) and a randomized chain_id.
pub fn generate_testnet_genesis(num_accounts: usize, output_path: &str) -> Result<(), String> {
    let random_suffix: u64 = rand::random();
    let chain_id = format!("commputer-testnet-{:x}", random_suffix);

    let mut channel_floors = HashMap::new();
    channel_floors.insert("Processing".to_string(), 0.20);
    channel_floors.insert("Gpu".to_string(), 0.20);
    channel_floors.insert("Storage".to_string(), 0.20);
    channel_floors.insert("Ram".to_string(), 0.20);
    channel_floors.insert("Bandwidth".to_string(), 0.20);

    let config = GenesisConfig {
        chain_id,
        total_supply: commputer_core::token::TOTAL_SUPPLY,
        epoch_duration_secs: 60, // Fast epochs for testnet
        emission_base_rate: 100 * UNITS_PER_COMME,
        emission_floor_rate: 10 * UNITS_PER_COMME,
        channel_floors,
    };

    let mut accounts = Vec::with_capacity(num_accounts);
    for _ in 0..num_accounts {
        let wallet = Wallet::generate();
        accounts.push(GenesisAccount {
            address: hex::encode(wallet.address().0),
            balance: 1000 * UNITS_PER_COMME, // 1000 COMME each
        });
    }

    let genesis = TestnetGenesis { config, accounts };
    let json = serde_json::to_string_pretty(&genesis)
        .map_err(|e| format!("failed to serialize genesis: {e}"))?;
    std::fs::write(output_path, json)
        .map_err(|e| format!("failed to write genesis file: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_testnet_genesis_creates_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("genesis_test_{}.json", std::process::id()));
        let path_str = path.to_str().unwrap();

        generate_testnet_genesis(5, path_str).unwrap();

        let data = std::fs::read_to_string(path_str).unwrap();
        let genesis: TestnetGenesis = serde_json::from_str(&data).unwrap();

        assert_eq!(genesis.accounts.len(), 5);
        assert!(genesis.config.chain_id.starts_with("commputer-testnet-"));
        assert_eq!(genesis.config.epoch_duration_secs, 60);
        for account in &genesis.accounts {
            assert_eq!(account.balance, 1000 * UNITS_PER_COMME);
            assert_eq!(account.address.len(), 64); // 32 bytes hex-encoded
        }

        std::fs::remove_file(path_str).ok();
    }

    #[test]
    fn generate_zero_accounts() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("genesis_test_zero_{}.json", std::process::id()));
        let path_str = path.to_str().unwrap();

        generate_testnet_genesis(0, path_str).unwrap();

        let data = std::fs::read_to_string(path_str).unwrap();
        let genesis: TestnetGenesis = serde_json::from_str(&data).unwrap();
        assert!(genesis.accounts.is_empty());

        std::fs::remove_file(path_str).ok();
    }
}
