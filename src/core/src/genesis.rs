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

// ── B8: genesis-anchored PoUW consensus params (plain-serde mirror) ────────────────────────────
//
// STAGING NOTE (1.2a): this is a STANDALONE `core::genesis` type, NOT yet a field of `GenesisConfig`.
// Embedding it as `GenesisConfig.consensus_params` (the design's end-state) would break the bare
// struct literal in `node/src/testnet_genesis.rs` (a PROTECTED src/node file, not editable in 1.2a),
// failing the mandatory `cargo build --workspace`. Embedding is therefore deferred to 1.2b — which is
// founder-gated, edits protected node files anyway (the main.rs genesis-LOAD path, C6), and lands the
// matching one-line testnet_genesis.rs update alongside. 1.2a delivers the full substrate: the schema,
// the defaults, the storage converter, and `ChainState::set_consensus_params` (with the C1 fix).
//
// These structs are a dependency-free serde mirror of the `pouw`/`pouw-onchain` consensus-param
// bundle. Every field carries a `#[serde(default)]` and every group `impl Default` reproduces the
// upstream default constructor value EXACTLY, so:
//   * a genesis file omitting `consensus_params` (or any sub-field) == today's defaults, and
//   * `GenesisConfig::default().consensus_params` converts to the exact param structs
//     `ChainState` uses today (asserted by `commputer-storage`'s C9c test).
// Treat the field layout + defaults as a STABLE genesis schema: changing a default silently shifts
// consensus params for every genesis that omits the field, so the C9c drift-guard test must move
// with any intentional change.

/// Game/pricing knobs (mirror of `commputer_pouw::params::GameParams`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameParamsConfig {
    #[serde(default = "gp_k")] pub k: usize,
    #[serde(default = "gp_k_escalate")] pub k_escalate: usize,
    #[serde(default = "gp_sample_rate_bps")] pub sample_rate_bps: u32,
    #[serde(default = "gp_p_trap_bps")] pub p_trap_bps: u32,
    #[serde(default = "gp_quorum_num")] pub quorum_num: usize,
    #[serde(default = "gp_quorum_den")] pub quorum_den: usize,
    #[serde(default = "gp_worker_bps")] pub worker_bps: u32,
    #[serde(default = "gp_verifier_bps")] pub verifier_bps: u32,
    #[serde(default = "gp_burn_bps")] pub burn_bps: u32,
    #[serde(default = "gp_executor_bond")] pub executor_bond: u64,
    #[serde(default = "gp_verifier_bond")] pub verifier_bond: u64,
    #[serde(default = "gp_challenger_bond")] pub challenger_bond: u64,
    #[serde(default = "gp_dispute_bounty_bps")] pub dispute_bounty_bps: u32,
    #[serde(default = "gp_challenger_reward_bps")] pub challenger_reward_bps: u32,
    #[serde(default = "gp_escalation_reward_bps")] pub escalation_reward_bps: u32,
    #[serde(default = "gp_trap_jackpot_bps")] pub trap_jackpot_bps: u32,
    #[serde(default = "gp_price_per_mfuel")] pub price_per_mfuel: u64,
    #[serde(default = "gp_profit_margin_bps")] pub profit_margin_bps: u32,
    #[serde(default = "gp_bond_safety_bps")] pub bond_safety_bps: u32,
}
fn gp_k() -> usize { 3 }
fn gp_k_escalate() -> usize { 7 }
fn gp_sample_rate_bps() -> u32 { 10_000 }
fn gp_p_trap_bps() -> u32 { 1_000 }
fn gp_quorum_num() -> usize { 2 }
fn gp_quorum_den() -> usize { 3 }
fn gp_worker_bps() -> u32 { 8_500 }
fn gp_verifier_bps() -> u32 { 1_000 }
fn gp_burn_bps() -> u32 { 500 }
fn gp_executor_bond() -> u64 { 100 }
fn gp_verifier_bond() -> u64 { 20 }
fn gp_challenger_bond() -> u64 { 50 }
fn gp_dispute_bounty_bps() -> u32 { 2_000 }
fn gp_challenger_reward_bps() -> u32 { 1_000 }
fn gp_escalation_reward_bps() -> u32 { 1_000 }
fn gp_trap_jackpot_bps() -> u32 { 5_000 }
fn gp_price_per_mfuel() -> u64 { 1 }
fn gp_profit_margin_bps() -> u32 { 12_000 }
fn gp_bond_safety_bps() -> u32 { 15_000 }
impl Default for GameParamsConfig {
    fn default() -> Self {
        Self {
            k: gp_k(), k_escalate: gp_k_escalate(), sample_rate_bps: gp_sample_rate_bps(),
            p_trap_bps: gp_p_trap_bps(), quorum_num: gp_quorum_num(), quorum_den: gp_quorum_den(),
            worker_bps: gp_worker_bps(), verifier_bps: gp_verifier_bps(), burn_bps: gp_burn_bps(),
            executor_bond: gp_executor_bond(), verifier_bond: gp_verifier_bond(),
            challenger_bond: gp_challenger_bond(), dispute_bounty_bps: gp_dispute_bounty_bps(),
            challenger_reward_bps: gp_challenger_reward_bps(),
            escalation_reward_bps: gp_escalation_reward_bps(),
            trap_jackpot_bps: gp_trap_jackpot_bps(), price_per_mfuel: gp_price_per_mfuel(),
            profit_margin_bps: gp_profit_margin_bps(), bond_safety_bps: gp_bond_safety_bps(),
        }
    }
}

/// Terminal-resolution knobs (mirror of `commputer_pouw_onchain::settlement_resolution::ResolutionParams`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionParamsConfig {
    #[serde(default = "rp_cancel_burn_bps")] pub cancel_burn_bps: u32,
    #[serde(default = "rp_timeout_submitter_comp_bps")] pub timeout_submitter_comp_bps: u32,
}
fn rp_cancel_burn_bps() -> u32 { 200 }
fn rp_timeout_submitter_comp_bps() -> u32 { 2_000 }
impl Default for ResolutionParamsConfig {
    fn default() -> Self {
        Self { cancel_burn_bps: rp_cancel_burn_bps(), timeout_submitter_comp_bps: rp_timeout_submitter_comp_bps() }
    }
}

/// Phase window lengths in blocks (mirror of `commputer_pouw_onchain::consensus_params::PhaseWindows`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseWindowsConfig {
    #[serde(default = "pw_result_blocks")] pub result_blocks: u64,
    #[serde(default = "pw_commit_blocks")] pub commit_blocks: u64,
    #[serde(default = "pw_reveal_blocks")] pub reveal_blocks: u64,
    #[serde(default = "pw_claim_blocks")] pub claim_blocks: u64,
}
fn pw_result_blocks() -> u64 { 10 }
fn pw_commit_blocks() -> u64 { 10 }
fn pw_reveal_blocks() -> u64 { 10 }
fn pw_claim_blocks() -> u64 { 10 }
impl Default for PhaseWindowsConfig {
    fn default() -> Self {
        Self { result_blocks: pw_result_blocks(), commit_blocks: pw_commit_blocks(), reveal_blocks: pw_reveal_blocks(), claim_blocks: pw_claim_blocks() }
    }
}

/// Staking floors (mirror of `commputer_storage::state::StakeParams`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StakeParamsConfig {
    #[serde(default = "sp_unbonding_blocks")] pub unbonding_blocks: u64,
    #[serde(default = "sp_min_bond")] pub min_bond: u64,
}
fn sp_unbonding_blocks() -> u64 { 100 }
fn sp_min_bond() -> u64 { 1_000 }
impl Default for StakeParamsConfig {
    fn default() -> Self {
        Self { unbonding_blocks: sp_unbonding_blocks(), min_bond: sp_min_bond() }
    }
}

/// Per-block capacity split (mirror of `commputer_pouw_onchain::capacity::CapacityParams`). Carried
/// in the genesis schema now; the `ChainState` wiring for admission is 1.2b (C8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacityParamsConfig {
    #[serde(default = "cp_total_slots")] pub total_slots: u32,
    #[serde(default = "cp_flagship_reserve_bps")] pub flagship_reserve_bps: u32,
    #[serde(default = "cp_reserve_floor_bps")] pub reserve_floor_bps: u32,
    #[serde(default = "cp_reserve_max_bps")] pub reserve_max_bps: u32,
    #[serde(default = "cp_reserve_churn_coeff_bps")] pub reserve_churn_coeff_bps: u32,
}
fn cp_total_slots() -> u32 { 100 }
fn cp_flagship_reserve_bps() -> u32 { 5_100 }
fn cp_reserve_floor_bps() -> u32 { 500 }
fn cp_reserve_max_bps() -> u32 { 1_500 }
fn cp_reserve_churn_coeff_bps() -> u32 { 1_000 }
impl Default for CapacityParamsConfig {
    fn default() -> Self {
        Self {
            total_slots: cp_total_slots(), flagship_reserve_bps: cp_flagship_reserve_bps(),
            reserve_floor_bps: cp_reserve_floor_bps(), reserve_max_bps: cp_reserve_max_bps(),
            reserve_churn_coeff_bps: cp_reserve_churn_coeff_bps(),
        }
    }
}

/// The full genesis-anchored PoUW consensus-param bundle (serde mirror). `wasm_limits`/`chunking`
/// are not genesis-configurable this pass — they use the node's COMPILED defaults, which is exactly
/// what `ChainState` uses today — so only the data-driven groups appear here plus the per-job
/// fuel-cap floor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsensusParamsConfig {
    #[serde(default)] pub game: GameParamsConfig,
    #[serde(default)] pub resolution: ResolutionParamsConfig,
    #[serde(default)] pub phase_windows: PhaseWindowsConfig,
    #[serde(default)] pub stake: StakeParamsConfig,
    #[serde(default)] pub capacity: CapacityParamsConfig,
    #[serde(default = "cpc_min_fuel_cap")] pub min_fuel_cap: u64,
}
fn cpc_min_fuel_cap() -> u64 { 1_000_000 }
// Manual Default (NOT derived): the derived impl would zero `min_fuel_cap` instead of using the
// upstream 1_000_000 floor, silently shifting a consensus param for any genesis omitting the field.
impl Default for ConsensusParamsConfig {
    fn default() -> Self {
        Self {
            game: GameParamsConfig::default(),
            resolution: ResolutionParamsConfig::default(),
            phase_windows: PhaseWindowsConfig::default(),
            stake: StakeParamsConfig::default(),
            capacity: CapacityParamsConfig::default(),
            min_fuel_cap: cpc_min_fuel_cap(),
        }
    }
}

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
        genesis_timestamp: 1774656000, // 2026-03-28 00:00:00 UTC — testnet epoch
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
