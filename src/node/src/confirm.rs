//! Transaction confirmation waiting module.
//!
//! Polls a node's RPC endpoint to wait for a transaction to reach
//! the desired number of confirmations.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::{Duration, Instant};

/// Response structure from GET /receipt/:tx_hash.
#[derive(Debug, Deserialize)]
struct TxReceipt {
    /// The block height at which this transaction was included.
    block_height: u64,
}

/// Response structure from GET /status (partial - we only need height).
#[derive(Debug, Deserialize)]
struct NodeStatus {
    /// Current chain height.
    height: u64,
}

/// Polls the node RPC every 2 seconds waiting for `tx_hash` to achieve the
/// required number of `confirmations`.
///
/// Returns `true` when the transaction has enough confirmations, or `false`
/// if `timeout_secs` elapses first.
pub async fn wait_for_confirmation(
    tx_hash: &str,
    rpc_port: u16,
    confirmations: u64,
    timeout_secs: u64,
) -> Result<bool> {
    let client = reqwest::Client::new();
    let receipt_url = format!("http://127.0.0.1:{}/receipt/{}", rpc_port, tx_hash);
    let status_url = format!("http://127.0.0.1:{}/status", rpc_port);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let poll_interval = Duration::from_secs(2);

    loop {
        if Instant::now() >= deadline {
            return Ok(false);
        }

        // Try to fetch the receipt
        let receipt_resp = client.get(&receipt_url).send().await;
        if let Ok(resp) = receipt_resp
            && resp.status().is_success()
                && let Ok(receipt) = resp.json::<TxReceipt>().await {
                    // Fetch current chain height
                    let status_resp = client
                        .get(&status_url)
                        .send()
                        .await
                        .context("failed to fetch node status")?;

                    if status_resp.status().is_success()
                        && let Ok(status) = status_resp.json::<NodeStatus>().await {
                            let current_confirmations =
                                status.height.saturating_sub(receipt.block_height);
                            if current_confirmations >= confirmations {
                                return Ok(true);
                            }
                        }
                }

        // Sleep until next poll, but not past deadline
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(poll_interval.min(remaining)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_deserialize() {
        let json = r#"{"block_height": 42}"#;
        let receipt: TxReceipt = serde_json::from_str(json).unwrap();
        assert_eq!(receipt.block_height, 42);
    }

    #[test]
    fn status_deserialize() {
        let json = r#"{"height": 100}"#;
        let status: NodeStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.height, 100);
    }

    #[tokio::test]
    async fn timeout_returns_false() {
        // Connecting to a port with no server should timeout quickly
        let result = wait_for_confirmation("deadbeef", 19999, 1, 1).await.unwrap();
        assert!(!result);
    }
}
