//! Tauri command handlers — IPC backend logic.
//! Items 177, 178, 180, 181, 182, 183, 184, 185, 186, 193, 199.

use serde::{Deserialize, Serialize};
use crate::rpc_client::{PeerInfo, render_peer_map};

/// Item 24: Wallet display data.
#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
pub struct WalletDisplay {
    pub address: String,
    pub balance_formatted: String,
    pub balance_raw: u64,
    pub tier: String,
    pub time_to_next_tier: Option<String>,
}

/// Item 177: Wallet creation result with real key generation.
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
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub recipient: String,
    pub amount: f64,
}

/// Item 28: Send transaction result.
#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub struct SendResult {
    pub tx_hash: String,
    pub fee_formatted: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Item 33: Block explorer entry.
#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
pub struct BlockEntry {
    pub height: u64,
    pub hash: String,
    pub timestamp: u64,
    pub tx_count: usize,
    pub producer: String,
}

/// Item 184: Compliance status display.
#[derive(Debug, Serialize, Deserialize)]
pub struct ComplianceDisplay {
    pub status: String,
    pub is_compliant: bool,
    pub explanation: Option<String>,
}

/// Item 185: Grace period display data.
#[derive(Debug, Serialize, Deserialize)]
pub struct GracePeriodDisplay {
    pub remaining_secs: u64,
    pub max_secs: u64,
    pub fill_percent: f64,
    pub is_draining: bool,
}

/// Item 186: Tier progress data.
#[derive(Debug, Serialize, Deserialize)]
pub struct TierProgress {
    pub current_tier: String,
    pub next_tier: Option<String>,
    pub current_amount: u64,
    pub next_threshold: Option<u64>,
    pub progress_percent: f64,
}

/// Item 182: Transaction history entry.
#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
pub struct TxHistoryEntry {
    pub tx_hash: String,
    pub tx_type: String,
    pub amount_formatted: String,
    pub timestamp: u64,
    pub status: String,
}

/// Item 193: Error display entry for the status bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEntry {
    pub message: String,
    pub severity: String,
    pub timestamp: u64,
}

/// Item 199: Export wallet data.
#[derive(Debug, Serialize, Deserialize)]
pub struct WalletExport {
    pub address: String,
    pub seed_phrase: Vec<String>,
    pub exported_at: String,
}

/// Item 198: Peer visualization data.
#[derive(Debug, Serialize, Deserialize)]
pub struct PeerVisualization {
    pub text_map: String,
    pub peer_count: usize,
    pub peers: Vec<PeerEntry>,
}

/// Simplified peer entry for the frontend.
#[derive(Debug, Serialize, Deserialize)]
pub struct PeerEntry {
    pub peer_id: String,
    pub ip: String,
    pub status: String,
    pub address: String,
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

/// Item 177: Create a new wallet using real key generation from commputer-core.
pub fn create_wallet() -> WalletCreated {
    let wallet = commputer_core::Wallet::generate();
    let address = hex::encode(wallet.address().0);
    let seed_phrase: Vec<String> = wallet.seed_phrase()
        .split_whitespace()
        .map(String::from)
        .collect();
    WalletCreated { address, seed_phrase }
}

/// Item 177: Recover a wallet from a seed phrase.
pub fn recover_wallet(phrase: &str) -> Result<WalletCreated, String> {
    let wallet = commputer_core::Wallet::from_seed_phrase(phrase)
        .map_err(|e| format!("Invalid seed phrase: {e}"))?;
    let address = hex::encode(wallet.address().0);
    let seed_phrase: Vec<String> = wallet.seed_phrase()
        .split_whitespace()
        .map(String::from)
        .collect();
    Ok(WalletCreated { address, seed_phrase })
}

/// Item 199: Export wallet (returns the seed phrase for the given address).
pub fn export_wallet(seed_phrase_str: &str) -> Result<WalletExport, String> {
    let wallet = commputer_core::Wallet::from_seed_phrase(seed_phrase_str)
        .map_err(|e| format!("Invalid seed phrase: {e}"))?;
    let address = hex::encode(wallet.address().0);
    let words: Vec<String> = seed_phrase_str.split_whitespace().map(String::from).collect();
    Ok(WalletExport {
        address,
        seed_phrase: words,
        exported_at: chrono_stub_now(),
    })
}

/// Stub for current timestamp string (avoids adding chrono dep).
fn chrono_stub_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

/// Item 186: Compute tier progress from a balance.
pub fn compute_tier_progress(balance_raw: u64) -> TierProgress {
    use commputer_core::tier::HolderTier;
    let whole = balance_raw / commputer_core::token::UNITS_PER_COMME;
    let tier = HolderTier::from_balance(whole);
    let (current_name, next_name, next_threshold) = match tier {
        HolderTier::None => ("None", Some("Base"), Some(HolderTier::BASE_THRESHOLD)),
        HolderTier::Base => ("Base", Some("Storage"), Some(HolderTier::STORAGE_THRESHOLD)),
        HolderTier::Storage => ("Storage", Some("Compute"), Some(HolderTier::COMPUTE_THRESHOLD)),
        HolderTier::Compute => ("Compute", Some("Full"), Some(HolderTier::FULL_THRESHOLD)),
        HolderTier::Full => ("Full", None, None),
    };

    let progress_percent = match next_threshold {
        Some(threshold) => {
            let prev = match tier {
                HolderTier::None => 0,
                HolderTier::Base => HolderTier::BASE_THRESHOLD,
                HolderTier::Storage => HolderTier::STORAGE_THRESHOLD,
                HolderTier::Compute => HolderTier::COMPUTE_THRESHOLD,
                HolderTier::Full => HolderTier::FULL_THRESHOLD,
            };
            let range = threshold - prev;
            if range > 0 {
                let progress = whole.saturating_sub(prev);
                (progress as f64 / range as f64 * 100.0).min(100.0)
            } else {
                100.0
            }
        }
        None => 100.0,
    };

    TierProgress {
        current_tier: current_name.to_string(),
        next_tier: next_name.map(String::from),
        current_amount: balance_raw,
        next_threshold: next_threshold.map(|t| t * commputer_core::token::UNITS_PER_COMME),
        progress_percent,
    }
}

/// Item 185: Compute grace period display from compliance info.
pub fn compute_grace_display(
    remaining_secs: Option<u64>,
    max_secs: Option<u64>,
) -> GracePeriodDisplay {
    let remaining = remaining_secs.unwrap_or(0);
    let max = max_secs.unwrap_or(1);
    let fill = if max > 0 { remaining as f64 / max as f64 * 100.0 } else { 0.0 };
    GracePeriodDisplay {
        remaining_secs: remaining,
        max_secs: max,
        fill_percent: fill,
        is_draining: remaining < max,
    }
}

/// Item 198: Build peer visualization.
pub fn build_peer_visualization(peers: &[PeerInfo]) -> PeerVisualization {
    let text_map = render_peer_map(peers);
    let entries: Vec<PeerEntry> = peers.iter().map(|p| {
        PeerEntry {
            peer_id: p.peer_id.clone(),
            ip: p.ip.clone().unwrap_or_else(|| "unknown".to_string()),
            status: p.compliance_status.clone().unwrap_or_else(|| "unknown".to_string()),
            address: p.validator_address.clone().unwrap_or_else(|| "none".to_string()),
        }
    }).collect();
    PeerVisualization {
        text_map,
        peer_count: peers.len(),
        peers: entries,
    }
}

/// Item 193: Format RPC errors into human-readable messages.
pub fn humanize_error(error: &str) -> ErrorEntry {
    let (message, severity) = if error.contains("RPC request failed") {
        ("Node is unreachable. Is the node running?".to_string(), "error")
    } else if error.contains("parse") {
        ("Received unexpected data from node. Version mismatch?".to_string(), "warning")
    } else if error.contains("timeout") {
        ("Request timed out. Node may be overloaded.".to_string(), "warning")
    } else {
        (error.to_string(), "info")
    };
    ErrorEntry {
        message,
        severity: severity.to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }
}

/// Item 180: Build mining status from RPC data.
pub fn build_mining_status(
    epoch: u64,
    total_mined: u64,
    proof_scores: ProofScores,
) -> MiningStatus {
    // Rough daily estimate: total_mined / epochs * epochs_per_day
    // Each epoch is ~10 minutes, so ~144 epochs per day.
    let daily_estimate = if epoch > 0 {
        (total_mined as f64 / epoch as f64 * 144.0) as u64
    } else {
        0
    };
    MiningStatus {
        epoch,
        total_mined_formatted: format_comme(total_mined),
        daily_estimate_formatted: format_comme(daily_estimate),
        proof_scores,
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

    #[test]
    fn item_177_create_wallet() {
        let created = create_wallet();
        assert_eq!(created.seed_phrase.len(), 24);
        assert_eq!(created.address.len(), 64); // 32 bytes hex
        // Each word is non-empty.
        for word in &created.seed_phrase {
            assert!(!word.is_empty());
        }
    }

    #[test]
    fn item_177_recover_wallet() {
        let created = create_wallet();
        let phrase = created.seed_phrase.join(" ");
        let recovered = recover_wallet(&phrase).unwrap();
        assert_eq!(recovered.address, created.address);
    }

    #[test]
    fn item_177_recover_invalid_phrase() {
        let result = recover_wallet("not a valid seed phrase at all");
        assert!(result.is_err());
    }

    #[test]
    fn item_186_tier_progress_none() {
        let progress = compute_tier_progress(0);
        assert_eq!(progress.current_tier, "None");
        assert_eq!(progress.next_tier, Some("Base".to_string()));
        assert_eq!(progress.progress_percent, 0.0);
    }

    #[test]
    fn item_186_tier_progress_base() {
        let raw = 5 * commputer_core::token::UNITS_PER_COMME;
        let progress = compute_tier_progress(raw);
        assert_eq!(progress.current_tier, "Base");
        assert_eq!(progress.next_tier, Some("Storage".to_string()));
        // 5 out of range [1..10], so progress = (5-1)/(10-1) = 4/9 ~= 44.4%
        assert!(progress.progress_percent > 40.0 && progress.progress_percent < 50.0);
    }

    #[test]
    fn item_186_tier_progress_full() {
        let raw = 100 * commputer_core::token::UNITS_PER_COMME;
        let progress = compute_tier_progress(raw);
        assert_eq!(progress.current_tier, "Full");
        assert!(progress.next_tier.is_none());
        assert_eq!(progress.progress_percent, 100.0);
    }

    #[test]
    fn item_185_grace_display() {
        let display = compute_grace_display(Some(300), Some(600));
        assert_eq!(display.remaining_secs, 300);
        assert_eq!(display.max_secs, 600);
        assert!((display.fill_percent - 50.0).abs() < 0.1);
        assert!(display.is_draining);
    }

    #[test]
    fn item_185_grace_display_full() {
        let display = compute_grace_display(Some(600), Some(600));
        assert!(!display.is_draining);
        assert!((display.fill_percent - 100.0).abs() < 0.1);
    }

    #[test]
    fn item_193_humanize_rpc_error() {
        let err = humanize_error("RPC request failed: connection refused");
        assert!(err.message.contains("unreachable"));
        assert_eq!(err.severity, "error");
    }

    #[test]
    fn item_193_humanize_parse_error() {
        let err = humanize_error("Failed to parse status: unexpected");
        assert!(err.message.contains("unexpected data"));
        assert_eq!(err.severity, "warning");
    }

    #[test]
    fn item_193_humanize_generic_error() {
        let err = humanize_error("something else went wrong");
        assert_eq!(err.message, "something else went wrong");
        assert_eq!(err.severity, "info");
    }

    #[test]
    fn item_180_mining_status() {
        let scores = ProofScores { cpu: 80, gpu: 60, storage: 70, ram: 90, bandwidth: 50 };
        let status = build_mining_status(10, 100 * commputer_core::token::UNITS_PER_COMME, scores);
        assert_eq!(status.epoch, 10);
        assert!(status.total_mined_formatted.contains("100 COMME"));
    }

    #[test]
    fn item_198_peer_visualization() {
        let peers = vec![
            crate::rpc_client::PeerInfo {
                peer_id: "12D3KooWAbcdef123456".to_string(),
                ip: Some("10.0.0.1".to_string()),
                validator_address: Some("aabbccdd".to_string()),
                compliance_status: Some("compliant".to_string()),
            },
        ];
        let viz = build_peer_visualization(&peers);
        assert_eq!(viz.peer_count, 1);
        assert!(viz.text_map.contains("[YOU]"));
        assert_eq!(viz.peers[0].ip, "10.0.0.1");
    }

    #[test]
    fn item_199_export_wallet() {
        let created = create_wallet();
        let phrase = created.seed_phrase.join(" ");
        let exported = export_wallet(&phrase).unwrap();
        assert_eq!(exported.address, created.address);
        assert_eq!(exported.seed_phrase.len(), 24);
        // exported_at is a unix timestamp string.
        assert!(!exported.exported_at.is_empty());
    }
}
