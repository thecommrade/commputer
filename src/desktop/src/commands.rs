//! Tauri command handlers — these are the Rust functions that the frontend JS calls.
//! Items 24-28: Wallet display, creation wizard, recovery, mining status, send tx.

use serde::{Deserialize, Serialize};

/// Item 24: Wallet display data.
#[derive(Debug, Serialize, Deserialize)]
pub struct WalletDisplay {
    pub address: String,
    pub balance_formatted: String,
    pub balance_raw: u64,
    pub tier: String,
    pub time_to_next_tier: Option<String>,
}

/// Item 25: Wallet creation result.
#[derive(Debug, Serialize, Deserialize)]
pub struct WalletCreated {
    pub address: String,
    pub seed_phrase: Vec<String>,
}

/// Item 27: Mining status panel data.
#[derive(Debug, Serialize, Deserialize)]
pub struct MiningStatus {
    pub epoch: u64,
    pub total_mined_formatted: String,
    pub daily_estimate_formatted: String,
    pub proof_scores: ProofScores,
}

/// Proof scores per channel.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProofScores {
    pub cpu: u64,
    pub gpu: u64,
    pub storage: u64,
    pub ram: u64,
    pub bandwidth: u64,
}

/// Item 28: Send transaction request.
#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub recipient: String,
    pub amount: f64,
}

/// Item 28: Send transaction result.
#[derive(Debug, Serialize)]
pub struct SendResult {
    pub tx_hash: String,
    pub fee_formatted: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Item 33: Block explorer entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct BlockEntry {
    pub height: u64,
    pub hash: String,
    pub timestamp: u64,
    pub tx_count: usize,
    pub producer: String,
}

/// Item 35: Compliance status display.
#[derive(Debug, Serialize, Deserialize)]
pub struct ComplianceDisplay {
    pub status: String,
    pub is_compliant: bool,
    pub explanation: Option<String>,
}

/// Item 36: Grace period display data.
#[derive(Debug, Serialize, Deserialize)]
pub struct GracePeriodDisplay {
    pub remaining_secs: u64,
    pub max_secs: u64,
    pub fill_percent: f64,
    pub is_draining: bool,
}

/// Item 37: Tier progress data.
#[derive(Debug, Serialize, Deserialize)]
pub struct TierProgress {
    pub current_tier: String,
    pub next_tier: Option<String>,
    pub current_amount: u64,
    pub next_threshold: Option<u64>,
    pub progress_percent: f64,
}

/// Item 38: Transaction history entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct TxHistoryEntry {
    pub tx_hash: String,
    pub tx_type: String,
    pub amount_formatted: String,
    pub timestamp: u64,
    pub status: String,
}

/// Format a raw amount as human-readable COMME.
pub fn format_comme(raw: u64) -> String {
    let whole = raw / commputer_core::token::UNITS_PER_COMME;
    let frac = raw % commputer_core::token::UNITS_PER_COMME;
    if frac == 0 {
        format!("{} COMME", whole)
    } else {
        format!("{}.{:04} COMME", whole, frac / (commputer_core::token::UNITS_PER_COMME / 10_000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_whole_amount() {
        let raw = 5 * commputer_core::token::UNITS_PER_COMME;
        assert_eq!(format_comme(raw), "5 COMME");
    }

    #[test]
    fn format_fractional_amount() {
        let raw = commputer_core::token::UNITS_PER_COMME + commputer_core::token::UNITS_PER_COMME / 2;
        assert_eq!(format_comme(raw), "1.5000 COMME");
    }
}
