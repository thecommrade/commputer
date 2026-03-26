#![allow(dead_code)]
//! First-run wizard (#42)
use std::io::{self, BufRead, Write};

#[derive(Debug, Clone)]
pub struct WizardResult { pub wallet_created: bool, pub contribution_percent: u8, pub seed_nodes: Vec<String> }

const MAINNET_SEEDS: &[&str] = &["seed1.commputer.network:9000", "seed2.commputer.network:9000", "seed3.commputer.network:9000"];
const TESTNET_SEEDS: &[&str] = &["testnet-seed1.commputer.network:9000", "testnet-seed2.commputer.network:9000"];

pub async fn first_run_wizard(testnet: bool) -> Result<WizardResult, Box<dyn std::error::Error>> {
    first_run_wizard_with_io(testnet, &mut io::stdin().lock(), &mut io::stdout())
}

pub fn first_run_wizard_with_io<R: BufRead, W: Write>(testnet: bool, reader: &mut R, writer: &mut W) -> Result<WizardResult, Box<dyn std::error::Error>> {
    let net = if testnet { "TESTNET" } else { "MAINNET" };
    writeln!(writer, "\n=========================================\n   Welcome to Commputer! ({})\n=========================================\n", net)?;
    writeln!(writer, "Step 1: Wallet Setup\n  [1] Create new wallet\n  [2] Recover from seed")?;
    write!(writer, "Choose (1 or 2): ")?; writer.flush()?;
    let mut choice = String::new(); reader.read_line(&mut choice)?;
    let wallet_created = choice.trim() != "2";
    write!(writer, "Contribution % [default: 50]: ")?; writer.flush()?;
    let mut pct = String::new(); reader.read_line(&mut pct)?;
    let contribution_percent: u8 = pct.trim().parse().unwrap_or(50).min(100);
    let mut seeds: Vec<String> = if testnet { TESTNET_SEEDS } else { MAINNET_SEEDS }.iter().map(|s| s.to_string()).collect();
    write!(writer, "Custom seed node (Enter to skip): ")?; writer.flush()?;
    let mut custom = String::new(); reader.read_line(&mut custom)?;
    if !custom.trim().is_empty() { seeds.push(custom.trim().to_string()); }
    writeln!(writer, "Setup complete!")?;
    Ok(WizardResult { wallet_created, contribution_percent, seed_nodes: seeds })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_create_defaults() { let r = first_run_wizard_with_io(false, &mut &b"1\n50\n\n"[..], &mut Vec::new()).unwrap(); assert!(r.wallet_created); assert_eq!(r.contribution_percent, 50); assert_eq!(r.seed_nodes.len(), 3); }
    #[test] fn test_recover_testnet() { let r = first_run_wizard_with_io(true, &mut &b"2\n75\ncustom:9000\n"[..], &mut Vec::new()).unwrap(); assert!(!r.wallet_created); assert_eq!(r.contribution_percent, 75); assert!(r.seed_nodes.contains(&"custom:9000".to_string())); }
    #[test] fn test_invalid_pct() { let r = first_run_wizard_with_io(false, &mut &b"1\nabc\n\n"[..], &mut Vec::new()).unwrap(); assert_eq!(r.contribution_percent, 50); }
    #[test] fn test_cap_100() { let r = first_run_wizard_with_io(false, &mut &b"1\n200\n\n"[..], &mut Vec::new()).unwrap(); assert_eq!(r.contribution_percent, 100); }
}
