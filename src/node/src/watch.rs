//! Wallet balance watching module.
//!
//! Polls a node's RPC endpoint to monitor balance changes for a given address.

use anyhow::{Context, Result};
use chrono::Local;
use tokio::signal;

/// Continuously polls the balance of `address` via the node RPC and prints
/// changes to stdout with timestamps.
///
/// Runs until Ctrl+C is received.
pub async fn watch_balance(address: &str, rpc_port: u16, interval_secs: u64) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/balance/{}", rpc_port, address);
    let mut last_balance: Option<String> = None;

    println!(
        "[{}] Watching balance for {} (poll every {}s)",
        Local::now().format("%H:%M:%S"),
        address,
        interval_secs,
    );

    loop {
        tokio::select! {
            _ = signal::ctrl_c() => {
                println!("\n[{}] Stopped watching.", Local::now().format("%H:%M:%S"));
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(interval_secs)) => {
                match client.get(&url).send().await {
                    Ok(resp) => {
                        match resp.text().await {
                            Ok(body) => {
                                let changed = match &last_balance {
                                    Some(prev) => prev != &body,
                                    None => true,
                                };
                                if changed {
                                    println!(
                                        "[{}] Balance: {}",
                                        Local::now().format("%H:%M:%S"),
                                        body.trim(),
                                    );
                                    last_balance = Some(body);
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "[{}] Error reading response: {}",
                                    Local::now().format("%H:%M:%S"),
                                    e,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[{}] Error polling balance: {}",
                            Local::now().format("%H:%M:%S"),
                            e,
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn url_format() {
        let url = format!("http://127.0.0.1:{}/balance/{}", 9000, "abc123");
        assert_eq!(url, "http://127.0.0.1:9000/balance/abc123");
    }
}
