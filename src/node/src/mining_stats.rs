//! Mining statistics CLI module.
//!
//! Fetches and displays mining-related metrics from a running node.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

/// Partial status response from GET /status.
#[derive(Debug, Deserialize)]
struct StatusResponse {
    #[serde(default)]
    height: u64,
    #[serde(default)]
    epoch: u64,
    #[serde(default)]
    epoch_progress: f64,
    #[serde(default)]
    validator_address: String,
}

/// Fetches mining metrics from the local node and prints them to stdout.
pub async fn show_mining_stats(rpc_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", rpc_port);

    // Fetch /status
    let status: Value = client
        .get(format!("{}/status", base))
        .send()
        .await
        .context("failed to connect to node RPC")?
        .json()
        .await
        .context("failed to parse status response")?;

    let height = status.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
    let epoch = status.get("epoch").and_then(|v| v.as_u64()).unwrap_or(0);
    let epoch_progress = status
        .get("epoch_progress")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let validator_address = status
        .get("validator_address")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Fetch /metrics
    let metrics: Value = client
        .get(format!("{}/metrics", base))
        .send()
        .await
        .context("failed to fetch metrics")?
        .json()
        .await
        .context("failed to parse metrics response")?;

    let total_mined = metrics
        .get("total_mined")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let emission_rate = metrics
        .get("emission_rate")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let blocks_until_next_epoch = metrics
        .get("blocks_until_next_epoch")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Fetch validator balance
    let balance_text = if validator_address != "unknown" {
        client
            .get(format!("{}/balance/{}", base, validator_address))
            .send()
            .await
            .ok()
            .and_then(|r| {
                if r.status().is_success() {
                    Some(r)
                } else {
                    None
                }
            })
            .map(|r| futures::executor::block_on(r.text()).unwrap_or_default())
            .unwrap_or_else(|| "unavailable".to_string())
    } else {
        "unavailable".to_string()
    };

    // Display
    println!("╔═══════════════════════════════════════╗");
    println!("║        Mining Statistics              ║");
    println!("╠═══════════════════════════════════════╣");
    println!("║ Chain Height:     {:>18} ║", height);
    println!("║ Current Epoch:    {:>18} ║", epoch);
    println!(
        "║ Epoch Progress:   {:>17.1}% ║",
        epoch_progress * 100.0
    );
    println!("║ Blocks to Epoch:  {:>18} ║", blocks_until_next_epoch);
    println!("╠═══════════════════════════════════════╣");
    println!(
        "║ Total Mined:      {:>17} ║",
        format_with_commas(total_mined)
    );
    println!("║ Emission Rate:   {:>15.4}/blk ║", emission_rate);
    println!(
        "║ Validator Balance: {:>17} ║",
        balance_text.trim()
    );
    println!("╚═══════════════════════════════════════╝");

    // Estimated time to next epoch (rough: 5 seconds per block)
    let secs = blocks_until_next_epoch * 5;
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    println!("  Est. time to next epoch: ~{}h {}m", hours, mins);

    Ok(())
}

fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_commas() {
        assert_eq!(format_with_commas(0), "0");
        assert_eq!(format_with_commas(999), "999");
        assert_eq!(format_with_commas(1000), "1,000");
        assert_eq!(format_with_commas(1234567), "1,234,567");
        assert_eq!(format_with_commas(1000000000), "1,000,000,000");
    }
}
