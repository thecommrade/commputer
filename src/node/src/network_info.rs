//! Network info CLI module.
//!
//! Fetches and displays network information from a running node,
//! including peers, validators, and overall health.

use anyhow::{Context, Result};
use serde_json::Value;

/// Fetches network information from the local node and prints it to stdout.
pub async fn show_network_info(rpc_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", rpc_port);

    // Fetch /peers
    let peers: Value = client
        .get(format!("{}/peers", base))
        .send()
        .await
        .context("failed to connect to node RPC")?
        .json()
        .await
        .context("failed to parse peers response")?;

    // Fetch /status
    let status: Value = client
        .get(format!("{}/status", base))
        .send()
        .await
        .context("failed to fetch status")?
        .json()
        .await
        .context("failed to parse status response")?;

    // Fetch /metrics
    let metrics: Value = client
        .get(format!("{}/metrics", base))
        .send()
        .await
        .context("failed to fetch metrics")?
        .json()
        .await
        .context("failed to parse metrics response")?;

    // Extract peer data
    let peer_list = peers.as_array();
    let peer_count = peer_list.map(|p| p.len()).unwrap_or(0);

    // Extract status data
    let height = status.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
    let node_id = status
        .get("node_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let version = status
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Extract metrics data
    let total_validators = metrics
        .get("total_validators")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let network_health = metrics
        .get("network_health")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Display
    println!("╔═══════════════════════════════════════════════════╗");
    println!("║              Network Information                  ║");
    println!("╠═══════════════════════════════════════════════════╣");
    println!("║ Node ID:         {:<32} ║", truncate(node_id, 32));
    println!("║ Version:         {:<32} ║", version);
    println!("║ Chain Height:    {:<32} ║", height);
    println!("║ Peer Count:      {:<32} ║", peer_count);
    println!("║ Total Validators:{:<32} ║", total_validators);
    println!(
        "║ Network Health:  {:<32} ║",
        network_health.to_uppercase()
    );
    println!("╠═══════════════════════════════════════════════════╣");

    // Peer list
    if let Some(peers) = peer_list {
        println!("║ Connected Peers:                                  ║");
        for peer in peers.iter().take(20) {
            let addr = peer
                .get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let latency = peer
                .get("latency_ms")
                .and_then(|v| v.as_u64())
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "?".to_string());
            let peer_id = peer
                .get("peer_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!(
                "║   {} {:>6}  {} ║",
                truncate_pad(addr, 20),
                latency,
                truncate_pad(peer_id, 18),
            );
        }
        if peers.len() > 20 {
            println!("║   ... and {} more peers                         ║", peers.len() - 20);
        }
    } else {
        println!("║ No peers connected                                ║");
    }

    println!("╚═══════════════════════════════════════════════════╝");

    Ok(())
}

/// Truncate a string to max_len characters.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len]
    }
}

/// Truncate and pad a string to exactly `width` characters.
fn truncate_pad(s: &str, width: usize) -> String {
    if s.len() >= width {
        s[..width].to_string()
    } else {
        format!("{:<width$}", s, width = width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_pad() {
        assert_eq!(truncate_pad("hi", 5), "hi   ");
        assert_eq!(truncate_pad("hello world", 5), "hello");
    }
}
