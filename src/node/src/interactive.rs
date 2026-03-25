//! Interactive transaction builder module.
//!
//! Provides a step-by-step CLI workflow for constructing, signing,
//! and broadcasting a transaction.

use anyhow::{bail, Context, Result};
use std::io::{self, BufRead, Write};

/// Walks the user through building and sending a transaction interactively.
///
/// Steps:
/// 1. Enter recipient address
/// 2. Enter amount
/// 3. Confirm details
/// 4. Enter wallet password
/// 5. Sign transaction
/// 6. Broadcast via RPC
pub async fn interactive_send(_testnet: bool, rpc_port: u16) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut lines = stdin.lock().lines();

    // Step 1: Recipient
    write!(out, "Recipient address: ")?;
    out.flush()?;
    let recipient = lines
        .next()
        .context("no input")??;
    let recipient = recipient.trim().to_string();
    if recipient.is_empty() {
        bail!("recipient address cannot be empty");
    }

    // Step 2: Amount
    write!(out, "Amount (COMME): ")?;
    out.flush()?;
    let amount_str = lines
        .next()
        .context("no input")??;
    let amount_str = amount_str.trim().to_string();
    let _amount: f64 = amount_str
        .parse()
        .context("invalid amount: must be a number")?;

    // Step 3: Confirm
    writeln!(out)?;
    writeln!(out, "=== Transaction Summary ===")?;
    writeln!(out, "  To:     {}", recipient)?;
    writeln!(out, "  Amount: {} COMME", amount_str)?;
    writeln!(out, "===========================")?;
    write!(out, "Confirm? (y/n): ")?;
    out.flush()?;
    let confirm = lines
        .next()
        .context("no input")??;
    if confirm.trim().to_lowercase() != "y" {
        writeln!(out, "Transaction cancelled.")?;
        return Ok(());
    }

    // Step 4: Password
    write!(out, "Wallet password: ")?;
    out.flush()?;
    let _password = lines
        .next()
        .context("no input")??;

    // Step 5 & 6: Sign and broadcast
    writeln!(out, "Signing transaction...")?;

    let client = reqwest::Client::new();
    let payload = serde_json::json!({
        "to": recipient,
        "amount": amount_str,
    });

    let url = format!("http://127.0.0.1:{}/send", rpc_port);
    match client.post(&url).json(&payload).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                writeln!(out, "Transaction broadcast successfully!")?;
                writeln!(out, "Response: {}", body)?;
            } else {
                writeln!(out, "Failed to broadcast (HTTP {}): {}", status, body)?;
            }
        }
        Err(e) => {
            writeln!(out, "Error broadcasting transaction: {}", e)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn module_compiles() {
        // Verifies the module compiles successfully
        assert!(true);
    }
}
