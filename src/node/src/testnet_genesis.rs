#![allow(dead_code)]
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
        proof_challenge_interval_secs: 60, // Fast proofs for testnet
        block_time_secs: 2,
        emission_decay_rate: 0.0001,
        genesis_timestamp: 1647907200, // 2022-03-22 00:00:00 UTC
        consensus_params: Default::default(), // B8: serde-default == today's compiled params.
        // A-batch item 7: the CORE `GenesisConfig.accounts` (height-0 credits) — empty
        // here; the alpha-reset faucet funding is added to genesis.json at the reset,
        // not in this generator. Empty + `skip_serializing_if` keeps the generated
        // genesis byte-identical to today (no `"accounts":[]` is emitted). NB this is
        // distinct from `TestnetGenesis.accounts` above — faucet funding goes in THIS
        // core field, consumed by `apply_genesis_accounts`.
        accounts: Vec::new(),
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

// ---------------------------------------------------------------------------
// Slice-4 / E1: alpha-reset faucet allocation (INERT until the founder sets the
// address at the genesis reset).
// ---------------------------------------------------------------------------

/// Hex-encoded address of the alpha-testnet faucet, funded at genesis.
///
/// `None` here (the shipped default) means NO faucet account is credited, so the
/// generated genesis stays byte-identical to today. At the alpha reset the founder
/// generates the faucet wallet OFFLINE (E11) and pastes its 64-hex address here;
/// `alpha_genesis_accounts()` then emits the single funded entry. The P8 boot
/// check in `rpc::provision_faucet_from_env` refuses to bind unless this equals
/// the address derived from `COMMPUTER_FAUCET_SEED`, so the funded/exempted
/// identity and the live signing wallet can never diverge.
pub const ALPHA_FAUCET_ADDRESS_HEX: Option<&str> =
    Some("6d6cc3f5e53e672b9f565b2e538dedcabf74791a4ee9eef12bda087082771eff");

/// Genesis balance credited to the faucet address, in raw base units.
/// 100,000 COMME = 100_000 * UNITS_PER_COMME = 1e13 raw units.
pub const ALPHA_FAUCET_ALLOCATION: u64 = 100_000 * UNITS_PER_COMME;

/// Alpha CONSENSUS validator allowlist. While the public installer is live,
/// registration is free and automatic (`auto_register_validator` fires at every
/// boot with a zero-fee tx), and leader rotation + the receive-side leader
/// check hang off `is_validator` — so an unpinned set lets any stranger's
/// laptop enter consensus math. The pin restricts the CONSENSUS set to these
/// addresses; registration stays open and `is_validator` still drives
/// telemetry/display. An EMPTY list disables the pin (pre-pin behavior).
/// Founder-operated nodes, alpha reset of 2026-07-24:
///
/// The `formation-test` cargo feature empties this list so the local formation
/// harness (whose nodes generate random wallets) can exercise multi-producer
/// consensus. It is a COMPILE-TIME switch that no runtime input can flip, and
/// release builds never enable it.
#[cfg(not(feature = "formation-test"))]
pub const ALPHA_PINNED_VALIDATORS: &[&str] = &[
    // solarplexus
    "0d9b5d0af6fe4f84f47cc23dcd39e0c5d86e425224276328d271b9702ead9c9a",
    // optiplex
    "1d6983ec9740143dfc839833c977ebed5304742981f61899f731bb690c7b33a5",
    // public seed
    "7322fd8c301299a430369d5609a46f29975b185ee30e991cffad6495e9e4e5d3",
];

/// Formation-harness builds: no pin, so randomly-generated test wallets can
/// take part in leader rotation.
#[cfg(feature = "formation-test")]
pub const ALPHA_PINNED_VALIDATORS: &[&str] = &[];

/// Whether `addr` may participate in CONSENSUS (leader rotation, validator-set
/// quorum math). True for every address when the pin list is empty.
pub fn is_pinned_validator(addr: &commputer_core::identity::Address) -> bool {
    is_pinned_in(ALPHA_PINNED_VALIDATORS, addr)
}

/// Testable core of `is_pinned_validator`.
fn is_pinned_in(list: &[&str], addr: &commputer_core::identity::Address) -> bool {
    if list.is_empty() {
        return true;
    }
    let hex = hex::encode(addr.0);
    list.iter().any(|p| *p == hex)
}

/// The height-0 credits for the alpha reset: exactly the faucet entry when
/// `ALPHA_FAUCET_ADDRESS_HEX` is set, or an EMPTY vec otherwise (⇒ a no-op in
/// `ChainState::apply_genesis_accounts`, keeping today's genesis byte-identical).
///
/// Returns `(hex_address, raw_balance)` pairs so it feeds directly into
/// `state.apply_genesis_accounts(&alpha_genesis_accounts())` — the PROTECTED
/// `main.rs` call site added at the reset (E1: credited BEFORE `apply_block`).
/// INERT: nothing calls this until that protected commit lands.
pub fn alpha_genesis_accounts() -> Vec<(String, u64)> {
    match ALPHA_FAUCET_ADDRESS_HEX {
        Some(addr) => vec![(addr.to_string(), ALPHA_FAUCET_ALLOCATION)],
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The harness build must ship an EMPTY pin (random test wallets have to
    /// be able to take part in leader rotation) — and, just as importantly,
    /// a production build must NOT be empty. Each side is asserted under its
    /// own cfg so neither can regress silently.
    #[cfg(feature = "formation-test")]
    #[test]
    fn formation_test_build_has_no_pin() {
        assert!(ALPHA_PINNED_VALIDATORS.is_empty());
    }

    #[cfg(not(feature = "formation-test"))]
    #[test]
    fn pinned_validator_list_is_wellformed_and_matches() {
        use commputer_core::identity::Address;
        assert_eq!(ALPHA_PINNED_VALIDATORS.len(), 3, "the three founder nodes");
        for p in ALPHA_PINNED_VALIDATORS {
            let bytes = hex::decode(p).expect("pinned entry is valid hex");
            assert_eq!(bytes.len(), 32, "pinned entry is a 32-byte address");
            assert_eq!(*p, p.to_lowercase(), "pinned entry is lowercase hex");
            let mut a = [0u8; 32];
            a.copy_from_slice(&bytes);
            assert!(is_pinned_validator(&Address(a)), "pinned address matches itself");
        }
        // An unlisted address is excluded while the pin is active.
        assert!(!is_pinned_validator(&Address([0x42u8; 32])));
    }

    #[test]
    fn empty_pin_list_admits_everyone() {
        use commputer_core::identity::Address;
        assert!(is_pinned_in(&[], &Address([0x42u8; 32])));
    }

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

    #[test]
    fn alpha_faucet_allocation_is_100k_comme() {
        // 100,000 COMME in raw base units.
        assert_eq!(ALPHA_FAUCET_ALLOCATION, 100_000 * UNITS_PER_COMME);
        assert_eq!(ALPHA_FAUCET_ALLOCATION, 10_000_000_000_000); // 1e13 raw units
    }

    #[test]
    fn alpha_genesis_accounts_fund_the_faucet() {
        // Alpha-reset state (set 2026-07-19): the founder's faucet address is
        // compiled in ⇒ genesis credits exactly one account with the full
        // faucet allocation.
        let addr = ALPHA_FAUCET_ADDRESS_HEX.expect("faucet address is set for the alpha reset");
        assert_eq!(addr.len(), 64);
        assert!(addr.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        let accounts = alpha_genesis_accounts();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, addr);
        assert_eq!(accounts[0].1, ALPHA_FAUCET_ALLOCATION);
    }
}
