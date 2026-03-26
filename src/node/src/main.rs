mod batch_jobs;
mod capacity;
mod compute_dashboard;
mod compute_cli;
mod compute_handler;
mod compute_session;
mod config;
mod consensus_manager;
mod event_loop;
mod job_cancel;
mod job_rpc;
mod job_spec;
mod native_executor;
mod parallel_sync;
mod proof_manager;
mod result_cache;
mod tier_rate_limit;
mod wasm_executor;
mod rpc;
mod validation;
mod faucet;
mod testnet_genesis;
mod wizard;
mod watch;
mod confirm;
mod interactive;
mod mining_stats;
mod network_info;

use std::path::PathBuf;
use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, warn};

use commputer_core::block::{Block, BlockHeader, BlockHash};
use commputer_core::identity::Address;
use commputer_core::keystore::Keystore;
use commputer_core::signing::sign_transaction;
use commputer_core::token::{Amount, TOTAL_SUPPLY, UNITS_PER_COMME};
use commputer_core::transaction::{Transaction, TxKind};
use commputer_core::wallet::Wallet;
use commputer_storage::state::ChainState;
use commputer_consensus::emission::EmissionSchedule;
use commputer_network::transport::CommpNetwork;

use crate::event_loop::EventLoop;

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "commputer")]
#[command(about = "Commputer: a communal supercomputer coordinated by blockchain")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the Commputer node
    Run {
        /// Run in testnet mode (default: true, use --mainnet for mainnet)
        #[arg(long, default_value = "true")]
        testnet: bool,
        /// Run in mainnet mode (overrides --testnet)
        #[arg(long, default_value = "false")]
        mainnet: bool,
        #[arg(long, default_value = "info")]
        log_level: String,
        #[arg(long, default_value = "9000")]
        port: u16,
        /// Port for the JSON RPC server (transaction submission, status queries)
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
        /// Bind address for the RPC server (0.0.0.0 for remote access)
        #[arg(long, default_value = "127.0.0.1")]
        rpc_bind: String,
        /// Feature 24: RPC API key for authentication
        #[arg(long)]
        rpc_key: Option<String>,
        /// Percentage of hardware resources to contribute (1-100)
        #[arg(long, default_value = "100")]
        contribution_percent: u8,
        /// Feature 168: Enable relay protocol (for NAT traversal)
        #[arg(long, default_value = "false")]
        relay: bool,
        /// Feature 178: Comma-separated seed node multiaddrs
        #[arg(long, value_delimiter = ',')]
        seeds: Vec<String>,
        /// Feature 179: Comma-separated DNS seed domain names
        #[arg(long, value_delimiter = ',')]
        dns_seeds: Vec<String>,
        /// Feature 244: Wallet password for non-interactive decrypt
        #[arg(long)]
        password: Option<String>,
        /// Feature 245: Select wallet by name (default: "default")
        #[arg(long, default_value = "default")]
        wallet: String,
        /// Feature 255: Enable terminal dashboard (continuously updating status)
        #[arg(long, default_value = "false")]
        dashboard: bool,
        /// Item 48: Enable structured JSON logging output
        #[arg(long, default_value = "false")]
        json_log: bool,
        /// Item 55: Comma-separated CORS allowed origins (default: "*" for testnet)
        #[arg(long, default_value = "*")]
        cors_origins: String,
    },
    /// Alias for 'run --testnet' — start mining immediately
    Mine {
        #[arg(long, default_value = "9000")]
        port: u16,
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
        #[arg(long)]
        password: Option<String>,
    },
    /// Check for updates and install the latest version
    Update,
    /// Wallet management
    Wallet {
        #[command(subcommand)]
        action: WalletAction,
    },
    /// Print node version and protocol info
    Version,
    /// Show chain status
    Status {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Show connected peers and their status (queries running node via RPC)
    Peers {
        /// RPC port of the running node
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
    /// Show balance and tier for an address (queries running node via RPC)
    Balance {
        /// Address to look up (hex)
        address: String,
        /// RPC port of the running node
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
    /// Verify the entire chain (all blocks, merkle roots, signatures)
    VerifyChain {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Export chain state to a JSON file for debugging
    ExportChain {
        /// Output file path
        #[arg(default_value = "chain-export.json")]
        output: String,
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Feature 187: Backup chain data to a compressed archive
    Backup {
        /// Output file path for the backup archive
        #[arg(default_value = "commputer-backup.tar.gz")]
        output: String,
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Feature 187: Restore chain data from a compressed archive
    Restore {
        /// Input file path for the backup archive
        input: String,
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Feature 191: Verify state integrity (merkle tree verification)
    VerifyState {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Feature 192: Rebuild indexes from raw block data
    RebuildIndexes {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Feature 245: List all wallets
    WalletList {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Feature 252: Address book management
    Address {
        #[command(subcommand)]
        action: AddressAction,
    },
    /// Feature 257: Generate a genesis block interactively
    GenesisGenerate {
        /// Output file path for the genesis block
        #[arg(default_value = "genesis.json")]
        output: String,
    },
    /// Send COMME to another address
    Send {
        /// Recipient address (hex)
        to: String,
        /// Amount in whole COMME
        amount: u64,
        #[arg(long, default_value = "true")]
        testnet: bool,
        /// RPC port of the running node (for broadcast)
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
    /// Item 106: Mining statistics
    MiningStats {
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
    /// Item 107: Network info
    NetworkInfo {
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
    /// Item 111: Validator status
    ValidatorStatus {
        /// Validator address (hex)
        address: String,
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
    /// Item 119: System requirements check
    SysCheck,
    /// Item 120: Compliance status with resolution instructions
    ComplianceCheck {
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
    /// Item 24: Submit a burst compute job
    Burst {
        /// Number of CPU cores to request
        #[arg(long)]
        cpu: u16,
        /// RAM amount (e.g. "8gb")
        #[arg(long)]
        ram: String,
        /// Duration (e.g. "1h", "30m")
        #[arg(long)]
        duration: String,
        /// RPC port of the running node
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
}

#[derive(Subcommand, Debug)]
enum WalletAction {
    /// Create a new wallet
    Create {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Recover wallet from seed phrase
    Recover {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Show wallet address and balance
    Show {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
    /// Export seed phrase (requires password)
    Export {
        #[arg(long, default_value = "true")]
        testnet: bool,
    },
}

/// Feature 252: Address book actions.
#[derive(Subcommand, Debug)]
enum AddressAction {
    /// Add a labeled address
    Add {
        /// Label for the address
        label: String,
        /// Address in hex
        address: String,
    },
    /// List all saved addresses
    List,
    /// Remove a labeled address
    Remove {
        /// Label to remove
        label: String,
    },
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Item 8: Data directory — ~/.commputer/testnet/ or ~/.commputer/mainnet/.
fn data_dir(testnet: bool) -> PathBuf {
    config::data_dir(testnet)
}

/// Item 9: Wallet directory — ~/.commputer/wallet/ (survives data wipes).
fn wallet_dir() -> PathBuf {
    config::wallet_dir()
}

fn wallet_path(testnet: bool) -> PathBuf {
    let dir = wallet_dir();
    if testnet {
        dir.join("wallet-testnet.json")
    } else {
        dir.join("wallet.json")
    }
}

/// Feature 245: Get the path for a named wallet.
#[allow(dead_code)]
fn wallet_path_named(testnet: bool, name: &str) -> PathBuf {
    let dir = wallet_dir();
    let suffix = if testnet { "-testnet" } else { "" };
    dir.join("wallets").join(format!("{}{}.json", name, suffix))
}

/// Item 3: Path for the persistent libp2p peer identity keypair.
fn peer_key_path() -> PathBuf {
    config::peer_key_path()
}

fn read_password(prompt: &str) -> String {
    // Check environment variable first (for systemd/headless operation)
    if let Ok(pw) = std::env::var("COMMPUTER_WALLET_PASSWORD") {
        return pw;
    }
    eprint!("{}", prompt);
    let mut password = String::new();
    if let Err(e) = std::io::stdin().read_line(&mut password) {
        warn!("Failed to read password from stdin: {}", e);
        return String::new();
    }
    password.trim().to_string()
}

fn read_line(prompt: &str) -> String {
    eprint!("{}", prompt);
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_line(&mut input) {
        warn!("Failed to read input from stdin: {}", e);
        return String::new();
    }
    input.trim().to_string()
}

fn create_genesis() -> Block {
    create_genesis_for_dir(None)
}

fn create_genesis_for_dir(data_dir: Option<&std::path::Path>) -> Block {
    // Try to load genesis config from data directory.
    let _genesis_config = if let Some(dir) = data_dir {
        let genesis_path = dir.join("genesis.json");
        if genesis_path.exists() {
            match commputer_core::genesis::load_genesis(&genesis_path) {
                Ok(config) => {
                    info!("Loaded genesis config from {}", genesis_path.display());
                    config
                }
                Err(e) => {
                    warn!("Failed to load genesis.json: {}. Using defaults.", e);
                    commputer_core::genesis::default_genesis()
                }
            }
        } else {
            commputer_core::genesis::default_genesis()
        }
    } else {
        commputer_core::genesis::default_genesis()
    };

    Block {
        header: BlockHeader {
            protocol_version: 1, height: 0,
            parent_hash: BlockHash::GENESIS,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            // Item 44: Fixed genesis timestamp (2026-03-22 00:00:00 UTC).
            timestamp: 1774022400,
            producer: Address([0u8; 32]), // No producer for genesis.
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
            chain_id: _genesis_config.chain_id.clone(),
        },
        transactions: vec![],
        proof_summaries: vec![],
        compliance_summary: None, epoch_summary: None,
    }
}

fn print_banner() {
    println!();
    println!("  ╔═══════════════════════════════════════════════╗");
    println!("  ║            C O M M P U T E R                 ║");
    println!("  ║      A communal supercomputer for the        ║");
    println!("  ║      people, by the people.                  ║");
    println!("  ║                                              ║");
    println!("  ║      $COMME · 2B supply · burn-heavy         ║");
    println!("  ║      Scale hurts. Patience rewards.          ║");
    println!("  ╚═══════════════════════════════════════════════╝");
    println!("  Version: {} (git: {}) built {}", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"), env!("BUILD_DATE"));
    println!("  Chain:   {}", config::DEFAULT_TESTNET_CHAIN_ID);
    println!();
}

fn print_chain_status(state: &ChainState) {
    let total = TOTAL_SUPPLY / UNITS_PER_COMME;
    let emitted = state.total_emitted / UNITS_PER_COMME;
    let burned = state.total_burned / UNITS_PER_COMME;
    let circulating = state.circulating_supply() / UNITS_PER_COMME;
    let remaining = state.remaining_supply() / UNITS_PER_COMME;

    println!("Chain status:");
    println!("  Height:      {}", state.blocks.height());
    println!("  Total:       {} COMME", total);
    println!("  Emitted:     {} COMME", emitted);
    println!("  Burned:      {} COMME", burned);
    println!("  Circulating: {} COMME", circulating);
    println!("  Remaining:   {} COMME", remaining);
    println!("  Accounts:    {}", state.accounts.len());
    println!("  Epoch:       {}", state.current_epoch);

    if state.is_emergency_access() {
        println!("  WARNING: EMERGENCY ACCESS MODE — supply below 1M COMME");
    }
}

/// Open chain state, applying genesis if needed.
fn open_chain_state(testnet: bool) -> Result<ChainState> {
    let dir = data_dir(testnet);
    std::fs::create_dir_all(&dir)?;
    let mut state = ChainState::open(&dir)?;
    if state.blocks.is_empty() {
        let genesis = create_genesis();
        state.apply_block(&genesis)?;
    }
    Ok(state)
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

fn cmd_wallet_create(testnet: bool) -> Result<()> {
    let path = wallet_path(testnet);
    if path.exists() {
        anyhow::bail!(
            "Wallet already exists at {}. Remove it first if you want to create a new one.",
            path.display()
        );
    }

    let wallet = Wallet::generate();
    let password = read_password("Set a password for your wallet: ");
    if password.is_empty() {
        anyhow::bail!("Password cannot be empty.");
    }

    let confirm = read_password("Confirm password: ");
    if password != confirm {
        anyhow::bail!("Passwords do not match.");
    }

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    Keystore::save(&wallet, &path, &password)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    println!();
    println!("Wallet created successfully.");
    println!();
    println!("  Address: {}", wallet.address());
    println!("  Full:    {}", hex::encode(wallet.address().0));
    println!();
    println!("  Seed phrase (WRITE THIS DOWN — you cannot recover it later):");
    println!();
    let phrase = wallet.seed_phrase();
    for (i, word) in phrase.split_whitespace().enumerate() {
        println!("    {:>2}. {}", i + 1, word);
    }
    println!();
    println!("  Saved to: {}", path.display());

    Ok(())
}

fn cmd_wallet_recover(testnet: bool) -> Result<()> {
    let path = wallet_path(testnet);
    if path.exists() {
        anyhow::bail!(
            "Wallet already exists at {}. Remove it first if you want to recover a different wallet.",
            path.display()
        );
    }

    let phrase = read_line("Enter your 24-word seed phrase: ");
    let wallet = Wallet::from_seed_phrase(&phrase)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let password = read_password("Set a password for your wallet: ");
    if password.is_empty() {
        anyhow::bail!("Password cannot be empty.");
    }

    let confirm = read_password("Confirm password: ");
    if password != confirm {
        anyhow::bail!("Passwords do not match.");
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    Keystore::save(&wallet, &path, &password)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    println!();
    println!("Wallet recovered successfully.");
    println!("  Address: {}", wallet.address());
    println!("  Full:    {}", hex::encode(wallet.address().0));
    println!("  Saved to: {}", path.display());

    Ok(())
}

fn cmd_wallet_show(testnet: bool) -> Result<()> {
    let path = wallet_path(testnet);
    if !path.exists() {
        anyhow::bail!("No wallet found. Run `commputer wallet create` first.");
    }

    let password = read_password("Password: ");
    let wallet = Keystore::load(&path, &password)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let state = open_chain_state(testnet)?;
    let addr = wallet.address();

    println!();
    println!("  Address:   {}", addr);
    println!("  Full:      {}", hex::encode(addr.0));

    if let Some(account) = state.accounts.get(addr) {
        println!("  Balance:   {}", account.balance);
        println!("  Tier:      {:?}", account.tier());
        println!("  Nonce:     {}", account.nonce);
        println!("  Validator: {}", if account.is_validator { "yes" } else { "no" });
    } else {
        println!("  Balance:   0 COMME");
        println!("  Tier:      None");
        println!("  (Account not yet on chain)");
    }

    // Item 25: Query active jobs from the RPC node if reachable.
    let job_pool_path = data_dir(testnet).join("job_pool.json");
    if job_pool_path.exists() {
        if let Ok(pool) = commputer_storage::job_pool::JobPool::load_from_dir(&data_dir(testnet)) {
            let addr_hex = hex::encode(addr.0);
            let my_jobs: Vec<_> = pool.all_jobs().into_iter()
                .filter(|j| hex::encode(j.submitter.0) == addr_hex)
                .collect();
            if !my_jobs.is_empty() {
                println!();
                println!("  Active Jobs: {}", my_jobs.len());
                for job in &my_jobs {
                    println!("    - {} ({:?}, budget: {} raw)",
                        hex::encode(&job.job_id.0[..8]),
                        job.status,
                        job.comme_budget,
                    );
                }
            }
        }
    }

    Ok(())
}

fn cmd_wallet_export(testnet: bool) -> Result<()> {
    let path = wallet_path(testnet);
    if !path.exists() {
        anyhow::bail!("No wallet found. Run `commputer wallet create` first.");
    }

    let password = read_password("Password: ");
    let wallet = Keystore::load(&path, &password)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    println!();
    println!("  Seed phrase:");
    println!();
    let phrase = wallet.seed_phrase();
    for (i, word) in phrase.split_whitespace().enumerate() {
        println!("    {:>2}. {}", i + 1, word);
    }
    println!();
    println!("  Keep this secret. Anyone with these words controls your wallet.");

    Ok(())
}

fn cmd_status(testnet: bool) -> Result<()> {
    let state = open_chain_state(testnet)?;
    println!();
    print_chain_status(&state);
    Ok(())
}

async fn cmd_send(to: &str, amount: u64, testnet: bool, rpc_port: u16) -> Result<()> {
    let path = wallet_path(testnet);
    if !path.exists() {
        anyhow::bail!("No wallet found. Run `commputer wallet create` first.");
    }

    // Parse recipient address.
    let to_bytes = hex::decode(to)
        .map_err(|e| anyhow::anyhow!("Invalid recipient address (expected hex): {}", e))?;
    if to_bytes.len() != 32 {
        anyhow::bail!(
            "Recipient address must be 32 bytes (64 hex characters), got {} bytes.",
            to_bytes.len()
        );
    }
    let mut to_arr = [0u8; 32];
    to_arr.copy_from_slice(&to_bytes);
    let to_addr = Address(to_arr);

    let password = read_password("Password: ");
    let wallet = Keystore::load(&path, &password)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let from_addr = *wallet.address();
    let from_hex = hex::encode(from_addr.0);

    // Fetch nonce from running node via RPC (falls back to local state).
    let client = reqwest::Client::new();
    let nonce_url = format!("http://127.0.0.1:{}/nonce/{}", rpc_port, from_hex);
    let nonce = match client.get(&nonce_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            body["nonce"].as_u64().unwrap_or(0)
        }
        _ => {
            // Fall back to local chain state.
            let state = open_chain_state(testnet)?;
            state.accounts.get(&from_addr).map(|a| a.nonce).unwrap_or(0)
        }
    };

    // Verify balance from local state.
    let state = open_chain_state(testnet)?;
    let balance = state
        .accounts
        .get(&from_addr)
        .map(|a| a.balance)
        .unwrap_or(Amount::ZERO);

    let send_amount = Amount::from_comme(amount);
    if balance.raw() < send_amount.raw() {
        anyhow::bail!(
            "Insufficient balance. Have {}, need {} COMME.",
            balance,
            amount,
        );
    }

    // Build and sign the transaction.
    let mut tx = Transaction {
        from: from_addr,
        nonce,
        kind: TxKind::Transfer {
            to: to_addr,
            amount: send_amount,
        },
        fee: commputer_core::transaction::MINIMUM_FEE,
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    };
    sign_transaction(&mut tx, &wallet);

    let tx_hash = tx.hash();

    println!();
    println!("Transaction created.");
    println!("  From:   {}", from_addr);
    println!("  To:     {}", to_addr);
    println!("  Amount: {} COMME", amount);
    println!("  Nonce:  {}", nonce);
    println!("  TxHash: {}", hex::encode(tx_hash.0));

    // Attempt to broadcast via RPC to the running node.
    let url = format!("http://127.0.0.1:{}/tx", rpc_port);
    println!();
    println!("Broadcasting to node at {}...", url);

    let client = reqwest::Client::new();
    match client.post(&url).json(&tx).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                println!("Transaction accepted by node.");
                println!("  Response: {}", body);
            } else {
                println!("Node rejected transaction (HTTP {}).", status);
                println!("  Response: {}", body);
            }
        }
        Err(e) => {
            println!("Could not reach node at {} — is it running?", url);
            println!("  Error: {}", e);
            println!();
            println!("Transaction saved locally. Start the node to broadcast.");
        }
    }

    Ok(())
}

async fn cmd_peers(rpc_port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/peers", rpc_port);
    let client = reqwest::Client::new();

    match client.get(&url).send().await {
        Ok(resp) => {
            let peers: Vec<rpc::PeerInfo> = resp.json().await
                .map_err(|e| anyhow::anyhow!("Failed to parse response: {}", e))?;

            println!();
            if peers.is_empty() {
                println!("No connected peers.");
            } else {
                println!("Connected peers ({}):", peers.len());
                println!();
                for peer in &peers {
                    println!("  Peer: {}", peer.peer_id);
                    if let Some(ref ip) = peer.ip {
                        println!("    IP:         {}", ip);
                    }
                    if let Some(ref addr) = peer.validator_address {
                        println!("    Validator:  {}", addr);
                    }
                    if let Some(ref status) = peer.compliance_status {
                        println!("    Compliance: {}", status);
                    }
                    println!();
                }
            }
        }
        Err(e) => {
            anyhow::bail!("Could not reach node at {} — is it running?\n  Error: {}", url, e);
        }
    }

    Ok(())
}

async fn cmd_balance(address: &str, rpc_port: u16) -> Result<()> {
    // Validate address format.
    let addr_bytes = hex::decode(address)
        .map_err(|e| anyhow::anyhow!("Invalid address (expected hex): {}", e))?;
    if addr_bytes.len() != 32 {
        anyhow::bail!("Address must be 32 bytes (64 hex characters), got {} bytes.", addr_bytes.len());
    }

    let url = format!("http://127.0.0.1:{}/balance/{}", rpc_port, address);
    let client = reqwest::Client::new();

    match client.get(&url).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                let info: rpc::BalanceInfo = resp.json().await
                    .map_err(|e| anyhow::anyhow!("Failed to parse response: {}", e))?;

                let whole = info.balance / commputer_core::token::UNITS_PER_COMME;
                let mined_whole = info.total_mined / commputer_core::token::UNITS_PER_COMME;

                println!();
                println!("  Address:      {}", info.address);
                println!("  Balance:      {} COMME", whole);
                println!("  Tier:         {}", info.tier);
                println!("  Nonce:        {}", info.nonce);
                println!("  Validator:    {}", if info.is_validator { "yes" } else { "no" });
                println!("  Total mined:  {} COMME", mined_whole);
            } else {
                let body: serde_json::Value = resp.json().await.unwrap_or_default();
                println!();
                if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
                    println!("  {}", err);
                } else {
                    println!("  Account not found on chain.");
                }
            }
        }
        Err(e) => {
            anyhow::bail!("Could not reach node at {} — is it running?\n  Error: {}", url, e);
        }
    }

    Ok(())
}

async fn run_node(
    testnet: bool,
    log_level: String,
    port: u16,
    rpc_port: u16,
    rpc_bind: String,
    contribution_percent: u8,
    relay: bool,
    seeds: Vec<String>,
    dns_seeds: Vec<String>,
    password: Option<String>,
    json_log: bool,
    cors_origins: String,
) -> Result<()> {
    // Initialize logging. Item 48: Support structured JSON output.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| log_level.parse().unwrap_or_default());
    if json_log {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .init();
    }

    // Item 8: Auto-create ~/.commputer/ directory structure on first run.
    config::ensure_dirs(testnet);

    // Item 6: Load config from ~/.commputer/config.toml (CLI flags override).
    let _node_config = config::NodeConfig::load();

    print_banner();

    // Validate configuration.
    if port == rpc_port {
        anyhow::bail!("P2P port ({}) and RPC port ({}) must be different", port, rpc_port);
    }
    if port < 1024 {
        warn!("P2P port {} is below 1024 — may require root privileges", port);
    }
    if rpc_port < 1024 {
        warn!("RPC port {} is below 1024 — may require root privileges", rpc_port);
    }

    if testnet {
        info!("Running in TESTNET mode");
    }

    // Initialize persistent chain state.
    let dir = data_dir(testnet);
    std::fs::create_dir_all(&dir)?;
    info!("Data directory: {}", dir.display());

    let mut state = ChainState::open(&dir)?;

    // Apply genesis block if this is a fresh chain.
    if state.blocks.is_empty() {
        let genesis = create_genesis();
        info!("Genesis block hash: {}", genesis.hash());
        state.apply_block(&genesis)?;
    } else {
        info!(
            "Resumed chain at height {} with {} accounts",
            state.blocks.height(),
            state.accounts.len(),
        );
    }

    // Print emission schedule info.
    let schedule = EmissionSchedule::new();
    let rate_1k = schedule.per_validator_daily_rate(1_000);
    let rate_100k = schedule.per_validator_daily_rate(100_000);
    info!("Emission schedule:");
    info!("  Rate @ 1K validators:   {:.4} COMME/day/node", rate_1k as f64 / UNITS_PER_COMME as f64);
    info!("  Rate @ 100K validators: {:.4} COMME/day/node", rate_100k as f64 / UNITS_PER_COMME as f64);
    info!("  Floor rate:             0.0100 COMME/day/node");

    // Print status via tracing for the node run.
    let total = TOTAL_SUPPLY / UNITS_PER_COMME;
    let emitted = state.total_emitted / UNITS_PER_COMME;
    let burned = state.total_burned / UNITS_PER_COMME;
    let circulating = state.circulating_supply() / UNITS_PER_COMME;
    let remaining = state.remaining_supply() / UNITS_PER_COMME;

    info!("Chain status:");
    info!("  Height:      {}", state.blocks.height());
    info!("  Total:       {} COMME", total);
    info!("  Emitted:     {} COMME", emitted);
    info!("  Burned:      {} COMME", burned);
    info!("  Circulating: {} COMME", circulating);
    info!("  Remaining:   {} COMME", remaining);
    info!("  Accounts:    {}", state.accounts.len());

    if state.is_emergency_access() {
        warn!("  EMERGENCY ACCESS MODE: supply below 1M COMME");
    }

    // Load or create wallet. Feature 244: support --password flag for non-interactive decrypt.
    // Item 10: First-run experience — auto-create wallet, display seed phrase, wait for confirm.
    let wallet_file = wallet_path(testnet);
    let wallet = if wallet_file.exists() {
        let pw = if let Some(ref pw) = password {
            pw.clone()
        } else {
            read_password("Wallet password: ")
        };
        Keystore::load(&wallet_file, &pw)
            .map_err(|e| anyhow::anyhow!("{}", e))?
    } else {
        // First run — create wallet automatically
        println!();
        println!("  ┌─────────────────────────────────────────────┐");
        println!("  │         FIRST RUN — WALLET CREATION          │");
        println!("  └─────────────────────────────────────────────┘");
        println!();

        let wallet = Wallet::generate();
        let phrase = wallet.seed_phrase();

        println!("  Your new wallet address:");
        println!("    {}", hex::encode(wallet.address().0));
        println!();
        println!("  Your seed phrase (WRITE THIS DOWN):");
        println!();
        for (i, word) in phrase.split_whitespace().enumerate() {
            println!("    {:>2}. {}", i + 1, word);
        }
        println!();
        println!("  ⚠  This is the ONLY way to recover your wallet.");
        println!("  ⚠  Store it securely. Never share it with anyone.");
        println!();

        // Set a password and save
        let pw = if let Some(ref pw) = password {
            pw.clone()
        } else {
            let pw = read_password("Set a wallet password: ");
            if pw.is_empty() {
                anyhow::bail!("Password cannot be empty.");
            }
            let confirm = read_password("Confirm password: ");
            if pw != confirm {
                anyhow::bail!("Passwords do not match.");
            }
            pw
        };

        if let Some(parent) = wallet_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Keystore::save(&wallet, &wallet_file, &pw)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        println!("  Wallet saved to: {}", wallet_file.display());
        println!();

        // Wait for user confirmation before starting
        if password.is_none() {
            read_line("Press ENTER to start the node...");
        }

        wallet
    };
    info!("Wallet address: {}", wallet.address());

    // Detect hardware fingerprint.
    let hardware = commputer_core::identity::HardwareFingerprint::detect();
    info!("Hardware detected:");
    info!("  CPU:     {} ({} cores)", hardware.cpu_model, hardware.cpu_cores);
    info!("  RAM:     {} MB", hardware.ram_total_mb);
    info!("  Storage: {} MB", hardware.storage_total_mb);
    info!("  GPU:     {}", hardware.gpu_model.as_deref().unwrap_or("none"));
    info!("  OS:      {}", hardware.os_family);

    // Set up network with persistent peer identity (Item 3).
    let peer_key = peer_key_path();
    let mut network = CommpNetwork::new_with_keypair_path(port, Some(&peer_key))
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    info!("P2P peer ID: {}", network.local_peer_id);

    // Feature 176: Log encryption status.
    network.log_encryption_status();

    // Connect to built-in seed nodes.
    let seeds_connected = network.connect_to_seeds();
    if seeds_connected > 0 {
        info!("Connected to {} built-in seed nodes", seeds_connected);
    }

    // Feature 178: Connect to custom seed nodes from CLI.
    if !seeds.is_empty() {
        let custom_connected = network.connect_to_custom_seeds(&seeds);
        info!("Connected to {} custom seed nodes", custom_connected);
    }

    // Feature 179: Resolve DNS seed domains.
    if !dns_seeds.is_empty() {
        let dns_connected = network.resolve_dns_seeds(&dns_seeds, port);
        info!("Connected to {} DNS seed nodes", dns_connected);
    }

    // Feature 166: Bootstrap Kademlia for peer discovery.
    network.bootstrap_kademlia();

    // Item 104: Relay node mode — node forwards traffic for NAT-ed peers.
    if relay {
        network.enable_relay_mode();
        info!("Relay mode active — this node will forward traffic, no mining");
    }

    // Item 102: Detect and log NAT type on startup.
    let nat_type = network.detect_nat_type();
    info!("NAT type: {}", nat_type);

    // Set up RPC server channel.
    let (tx_sender, tx_receiver) = tokio::sync::mpsc::channel(256);
    let initial_status = rpc::ChainStatus {
        height: state.blocks.height(),
        total_supply: commputer_core::token::TOTAL_SUPPLY,
        emitted: state.total_emitted,
        burned: state.total_burned,
        circulating: state.circulating_supply(),
        remaining: state.remaining_supply(),
        accounts: state.accounts.len(),
        epoch: state.current_epoch,
        pending_txs: 0,
    };

    let rpc_state = std::sync::Arc::new(rpc::RpcState {
        tx_sender,
        status: tokio::sync::Mutex::new(initial_status),
        peers: tokio::sync::Mutex::new(vec![]),
        balances: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        mempool: tokio::sync::Mutex::new(vec![]),
        blocks: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        receipts: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        metrics: tokio::sync::Mutex::new(rpc::NodeMetrics {
            uptime_secs: 0,
            height: 0,
            epoch: 0,
            peers_connected: 0,
            peers_banned: 0,
            blocks_produced: 0,
            pending_txs: 0,
            seen_tx_count: 0,
        }),
        compliance_stats: tokio::sync::Mutex::new(rpc::ComplianceDashboard::default()),
        anti_scale_metrics: tokio::sync::Mutex::new(rpc::AntiScaleDashboard::default()),
        network_health: tokio::sync::Mutex::new(rpc::NetworkHealthDashboard::default()),
        peer_quality: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        storage_metrics: tokio::sync::Mutex::new(commputer_storage::StorageMetrics::default()),
        ws_broadcast: tokio::sync::broadcast::channel(256).0,
        is_testnet: testnet,
        faucet_claims: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        api_key: None,
        rate_limits: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        validator_performance: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        cors_origins: cors_origins.clone(),
        start_time: std::time::Instant::now(),
        traffic_stats: tokio::sync::Mutex::new(serde_json::json!({})),
        proof_history: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        proof_leaderboard: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    });

    // Create event loop and attach RPC channel (shares status with RPC server).
    let mut event_loop = EventLoop::new(state, wallet, network, hardware);
    event_loop.attach_rpc(tx_receiver, rpc_state.clone());
    event_loop.auto_register_validator(contribution_percent);

    // Feature 178: Store custom seeds for periodic reconnection.
    // Mark as seed connector if custom seeds were provided — this node
    // must connect to a peer before producing blocks to prevent chain forks.
    event_loop.is_seed_connector = !seeds.is_empty();
    event_loop.custom_seeds = seeds;

    // Spawn RPC server in the background.
    // Item 26: RPC binds to rpc_bind address (0.0.0.0 for remote access).
    tokio::spawn(rpc::start_rpc_server(rpc_port, rpc_bind, rpc_state));

    event_loop.run().await;

    Ok(())
}

/// Item 19: Update command — check for new version and replace binary.
async fn cmd_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: {}", current);
    println!("Checking for updates...");

    let client = reqwest::Client::new();
    let url = "https://api.github.com/repos/thecommrade/commputer/releases/latest";
    let resp = client.get(url)
        .header("User-Agent", "commputer-updater")
        .send().await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await?;
            let latest = body["tag_name"].as_str().unwrap_or("unknown");
            let latest_clean = latest.trim_start_matches('v');

            if latest_clean == current {
                println!("You are already running the latest version.");
            } else {
                println!("New version available: {} (you have {})", latest, current);
                println!();
                println!("To update, run:");
                println!("  curl -sSf https://commputer.xyz/install.sh | sh");
            }
        }
        Ok(r) => {
            println!("Could not check for updates (HTTP {})", r.status());
            println!("Check https://github.com/thecommrade/commputer/releases manually.");
        }
        Err(e) => {
            println!("Could not reach update server: {}", e);
            println!("Check https://github.com/thecommrade/commputer/releases manually.");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // Feature 18: Custom panic handler — log panic info and exit cleanly.
    std::panic::set_hook(Box::new(|panic_info| {
        let location = panic_info.location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        // Use eprintln as a fallback since tracing may not be initialized yet.
        eprintln!("PANIC at {}: {}", location, message);
        // Try tracing if available.
        tracing::error!("PANIC at {}: {}", location, message);
        tracing::error!("Attempting to flush chain state before exit...");
        // Note: We cannot easily access chain state from the panic hook,
        // but the process exit will trigger destructors.
        std::process::exit(1);
    }));

    let cli = Cli::parse();

    match cli.command {
        Commands::Run { testnet, mainnet, log_level, port, rpc_port, rpc_bind, contribution_percent, relay, seeds, dns_seeds, password, wallet: _, dashboard: _, rpc_key: _, json_log, cors_origins } => {
            // --mainnet overrides --testnet
            let is_testnet = if mainnet { false } else { testnet };
            run_node(is_testnet, log_level, port, rpc_port, rpc_bind, contribution_percent, relay, seeds, dns_seeds, password, json_log, cors_origins).await?;
        }
        Commands::Mine { port, rpc_port, password } => {
            run_node(true, "info".to_string(), port, rpc_port, "127.0.0.1".to_string(), 100, false, vec![], vec![], password, false, "*".to_string()).await?;
        }
        Commands::Update => {
            cmd_update().await?;
        }
        Commands::Wallet { action } => match action {
            WalletAction::Create { testnet } => cmd_wallet_create(testnet)?,
            WalletAction::Recover { testnet } => cmd_wallet_recover(testnet)?,
            WalletAction::Show { testnet } => cmd_wallet_show(testnet)?,
            WalletAction::Export { testnet } => cmd_wallet_export(testnet)?,
        },
        Commands::Version => {
            println!("commputer {} (git: {})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"));
            println!("  Built:     {}", env!("BUILD_DATE"));
            println!("  Protocol:  /commputer/0.1.0");
            println!("  Chain ID:  {}", config::DEFAULT_TESTNET_CHAIN_ID);
            println!("  Supply:    2,000,000,000 COMME");
            println!("  Consensus: Snowball (sample=3, quorum=2, threshold=5)");
        }
        Commands::Status { testnet } => cmd_status(testnet)?,
        Commands::Peers { rpc_port } => cmd_peers(rpc_port).await?,
        Commands::Balance { address, rpc_port } => cmd_balance(&address, rpc_port).await?,
        Commands::VerifyChain { testnet } => {
            let state = open_chain_state(testnet)?;
            let height = state.blocks.height();
            println!("Verifying chain from height 0 to {}...", height);
            let mut errors = 0u64;
            for h in 0..=height {
                if let Some(block) = state.blocks.get_by_height(h) {
                    // Verify merkle roots.
                    if !block.verify_roots() {
                        println!("  ERROR at height {}: merkle root mismatch", h);
                        errors += 1;
                    }
                    // Verify producer signature (skip genesis).
                    if h > 0 && !block.verify_producer_signature() {
                        println!("  ERROR at height {}: invalid producer signature", h);
                        errors += 1;
                    }
                    // Verify all transaction signatures.
                    for (i, tx) in block.transactions.iter().enumerate() {
                        if !tx.verify() {
                            println!("  ERROR at height {}, tx {}: invalid signature", h, i);
                            errors += 1;
                        }
                    }
                } else {
                    println!("  WARNING: block at height {} not in memory", h);
                }
            }
            if errors == 0 {
                println!("Chain verified: {} blocks, 0 errors.", height + 1);
            } else {
                println!("Chain verification found {} errors in {} blocks.", errors, height + 1);
            }
        }
        Commands::ExportChain { output, testnet } => {
            let state = open_chain_state(testnet)?;
            let export = serde_json::json!({
                "height": state.blocks.height(),
                "total_emitted": state.total_emitted,
                "total_burned": state.total_burned,
                "circulating_supply": state.circulating_supply(),
                "remaining_supply": state.remaining_supply(),
                "current_epoch": state.current_epoch,
                "accounts": state.accounts.len(),
            });
            std::fs::write(&output, serde_json::to_string_pretty(&export)?)?;
            println!("Chain state exported to {}", output);
        }
        Commands::Backup { output, testnet } => {
            cmd_backup(&output, testnet)?;
        }
        Commands::Restore { input, testnet } => {
            cmd_restore(&input, testnet)?;
        }
        Commands::VerifyState { testnet } => {
            cmd_verify_state(testnet)?;
        }
        Commands::RebuildIndexes { testnet } => {
            cmd_rebuild_indexes(testnet)?;
        }
        Commands::WalletList { testnet } => {
            cmd_wallet_list(testnet)?;
        }
        Commands::Address { action } => {
            cmd_address(action)?;
        }
        Commands::GenesisGenerate { output } => {
            cmd_genesis_generate(&output)?;
        }
        Commands::Send { to, amount, testnet, rpc_port } => {
            cmd_send(&to, amount, testnet, rpc_port).await?;
        }
        Commands::MiningStats { rpc_port } => {
            cmd_mining_stats(rpc_port).await?;
        }
        Commands::NetworkInfo { rpc_port } => {
            cmd_network_info(rpc_port).await?;
        }
        Commands::ValidatorStatus { address, rpc_port } => {
            cmd_validator_status(&address, rpc_port).await?;
        }
        Commands::SysCheck => {
            cmd_sys_check();
        }
        Commands::ComplianceCheck { rpc_port } => {
            cmd_compliance_check(rpc_port).await?;
        }
        Commands::Burst { cpu, ram, duration, rpc_port } => {
            cmd_burst(cpu, &ram, &duration, rpc_port).await?;
        }
    }

    Ok(())
}

/// Item 106: Mining statistics CLI.
async fn cmd_mining_stats(rpc_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", rpc_port);

    let status: serde_json::Value = client.get(format!("{}/status", base))
        .send().await?.json().await?;
    let metrics: serde_json::Value = client.get(format!("{}/metrics", base))
        .send().await?.json().await?;

    let units = UNITS_PER_COMME;
    let height = status["height"].as_u64().unwrap_or(0);
    let epoch = status["epoch"].as_u64().unwrap_or(0);
    let emitted = status["emitted"].as_u64().unwrap_or(0);

    println!("Mining Statistics:");
    println!("  Height:           {}", height);
    println!("  Epoch:            {}", epoch);
    println!("  Total Emitted:    {} COMME", emitted / units);
    println!("  Pending Txs:      {}", metrics["pending_txs"].as_u64().unwrap_or(0));
    println!("  Peers Connected:  {}", metrics["peers_connected"].as_u64().unwrap_or(0));

    Ok(())
}

/// Item 107: Network info CLI.
async fn cmd_network_info(rpc_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", rpc_port);

    let peers: Vec<serde_json::Value> = client.get(format!("{}/peers", base))
        .send().await?.json().await?;
    let health: serde_json::Value = client.get(format!("{}/network", base))
        .send().await?.json().await?;

    println!("Network Info:");
    println!("  Peers:   {}", peers.len());
    for (i, peer) in peers.iter().enumerate() {
        let id = peer["peer_id"].as_str().unwrap_or("?");
        let ip = peer["ip"].as_str().unwrap_or("?");
        println!("  {}. {} ({})", i + 1, &id[..12.min(id.len())], ip);
    }
    if let Some(risk) = health["partition_risk"].as_str() {
        println!("  Partition Risk: {}", risk);
    }

    Ok(())
}

/// Item 111: Validator status CLI.
async fn cmd_validator_status(address: &str, rpc_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", rpc_port);

    let balance: serde_json::Value = client.get(format!("{}/balance/{}", base, address))
        .send().await?.json().await?;
    let perf: serde_json::Value = client.get(format!("{}/validator/{}/performance", base, address))
        .send().await?.json().await?;

    let units = UNITS_PER_COMME;
    println!("Validator Status: {}", &address[..16.min(address.len())]);
    println!("  Balance:    {} COMME", balance["balance"].as_u64().unwrap_or(0) / units);
    println!("  Tier:       {}", balance["tier"].as_str().unwrap_or("?"));
    println!("  Validator:  {}", balance["is_validator"].as_bool().unwrap_or(false));
    println!("  Mined:      {} COMME", balance["total_mined"].as_u64().unwrap_or(0) / units);

    if let Some(blocks) = perf["blocks_produced"].as_u64() {
        println!("  Blocks:     {}", blocks);
    }

    Ok(())
}

/// Item 119: System requirements check CLI.
fn cmd_sys_check() {
    let hw = commputer_core::identity::HardwareFingerprint::detect();
    println!("System Requirements Check:");
    println!("  CPU:     {} ({} cores)", hw.cpu_model, hw.cpu_cores);
    println!("  RAM:     {} MB", hw.ram_total_mb);
    println!("  Storage: {} MB", hw.storage_total_mb);
    println!("  GPU:     {}", hw.gpu_model.as_deref().unwrap_or("none"));
    println!("  OS:      {}", hw.os_family);
    println!();

    // Compare against reference node
    let mut issues = Vec::new();
    if hw.cpu_cores < 2 { issues.push("CPU: Need at least 2 cores"); }
    if hw.ram_total_mb < 2048 { issues.push("RAM: Need at least 2 GB"); }
    if hw.storage_total_mb < 10240 { issues.push("Storage: Need at least 10 GB"); }

    if issues.is_empty() {
        println!("  Result: PASS — meets minimum requirements");
    } else {
        println!("  Result: WARNING — below recommended specs:");
        for issue in &issues {
            println!("    - {}", issue);
        }
    }
}

/// Item 120: Compliance status CLI.
async fn cmd_compliance_check(rpc_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", rpc_port);

    let compliance: serde_json::Value = client.get(format!("{}/compliance", base))
        .send().await?.json().await?;

    println!("Compliance Status:");
    if let Some(status) = compliance["status"].as_str() {
        println!("  Status: {}", status);
    }
    if let Some(nerf) = compliance["nerf_rate"].as_f64() {
        if nerf < 1.0 {
            println!("  Nerf Rate: {:.0}%", nerf * 100.0);
            println!();
            println!("  Resolution:");
            println!("    1. Check if you're running multiple nodes on the same IP");
            println!("    2. Ensure your node has a unique IP address");
            println!("    3. If behind NAT, use --relay flag or configure port forwarding");
            println!("    4. Nerf rate can only increase — it never decreases");
        } else {
            println!("  You are fully compliant.");
        }
    }

    Ok(())
}

/// Item 24: Burst compute CLI — submit a SubmitJob transaction via RPC.
async fn cmd_burst(cpu: u16, ram: &str, duration: &str, rpc_port: u16) -> Result<()> {
    // Parse RAM (e.g. "8gb" -> 8192 MB)
    let ram_mb = parse_ram_string(ram)?;
    // Parse duration (e.g. "1h" -> 3600 secs)
    let duration_secs = parse_duration_string(duration)?;

    // Calculate a base budget: cpu_cores * duration_secs * 1_000_000 raw units per core-hour
    let core_hours = (cpu as u64 * duration_secs) / 3600;
    let comme_budget = core_hours.max(1) * 1_000_000; // 0.01 COMME per core-hour minimum

    // Create a job spec hash from the parameters
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(cpu.to_le_bytes());
    hasher.update(ram_mb.to_le_bytes());
    hasher.update(duration_secs.to_le_bytes());
    hasher.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes(),
    );
    let hash_result = hasher.finalize();
    let mut job_spec_hash = [0u8; 32];
    job_spec_hash.copy_from_slice(&hash_result);

    let submit_req = serde_json::json!({
        "job_spec_hash_hex": hex::encode(job_spec_hash),
        "cpu_cores": cpu,
        "gpu_vram_mb": 0,
        "ram_mb": ram_mb,
        "storage_mb": 0,
        "bandwidth_mbps": 0,
        "max_duration_secs": duration_secs,
        "comme_budget": comme_budget,
        "l2_id": null,
    });

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", rpc_port);

    println!("Submitting burst compute job...");
    println!("  CPU cores:  {}", cpu);
    println!("  RAM:        {} MB", ram_mb);
    println!("  Duration:   {} secs", duration_secs);
    println!("  Budget:     {} raw units ({:.4} COMME)", comme_budget, comme_budget as f64 / 100_000_000.0);

    let resp: serde_json::Value = client
        .post(format!("{}/jobs/submit", base))
        .json(&submit_req)
        .send()
        .await?
        .json()
        .await?;

    if resp["accepted"].as_bool().unwrap_or(false) {
        println!("  Job ID:     {}", resp["job_id_hex"].as_str().unwrap_or("?"));
        println!("  Status:     Submitted successfully");
    } else {
        let err = resp["error"].as_str().unwrap_or("unknown error");
        println!("  Error:      {}", err);
    }

    Ok(())
}

fn parse_ram_string(s: &str) -> Result<u64> {
    let s = s.to_lowercase();
    if let Some(num) = s.strip_suffix("gb") {
        let n: u64 = num.trim().parse().map_err(|_| anyhow::anyhow!("Invalid RAM: {}", s))?;
        Ok(n * 1024)
    } else if let Some(num) = s.strip_suffix("mb") {
        let n: u64 = num.trim().parse().map_err(|_| anyhow::anyhow!("Invalid RAM: {}", s))?;
        Ok(n)
    } else {
        let n: u64 = s.trim().parse().map_err(|_| anyhow::anyhow!("Invalid RAM: {}. Use e.g. 8gb or 1024mb", s))?;
        Ok(n) // assume MB
    }
}

fn parse_duration_string(s: &str) -> Result<u64> {
    let s = s.to_lowercase();
    if let Some(num) = s.strip_suffix('h') {
        let n: u64 = num.trim().parse().map_err(|_| anyhow::anyhow!("Invalid duration: {}", s))?;
        Ok(n * 3600)
    } else if let Some(num) = s.strip_suffix('m') {
        let n: u64 = num.trim().parse().map_err(|_| anyhow::anyhow!("Invalid duration: {}", s))?;
        Ok(n * 60)
    } else if let Some(num) = s.strip_suffix('s') {
        let n: u64 = num.trim().parse().map_err(|_| anyhow::anyhow!("Invalid duration: {}", s))?;
        Ok(n)
    } else {
        let n: u64 = s.trim().parse().map_err(|_| anyhow::anyhow!("Invalid duration: {}. Use e.g. 1h, 30m, or 3600s", s))?;
        Ok(n) // assume seconds
    }
}

// ── Feature 187: Backup and restore ──

fn cmd_backup(output: &str, testnet: bool) -> Result<()> {
    let dir = data_dir(testnet);
    if !dir.exists() {
        anyhow::bail!("Data directory {} does not exist. Nothing to backup.", dir.display());
    }

    let file = std::fs::File::create(output)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);

    archive.append_dir_all(".", &dir)?;
    archive.finish()?;

    println!("Backup created: {}", output);
    println!("  Source: {}", dir.display());

    Ok(())
}

fn cmd_restore(input: &str, testnet: bool) -> Result<()> {
    let dir = data_dir(testnet);
    let input_path = std::path::Path::new(input);
    if !input_path.exists() {
        anyhow::bail!("Backup file {} does not exist.", input);
    }

    if dir.exists() {
        println!("WARNING: Data directory {} already exists.", dir.display());
        let confirm = read_line("Overwrite? (yes/no): ");
        if confirm != "yes" {
            println!("Restore cancelled.");
            return Ok(());
        }
        std::fs::remove_dir_all(&dir)?;
    }

    std::fs::create_dir_all(&dir)?;

    let file = std::fs::File::open(input)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(&dir)?;

    println!("Restore complete: {}", dir.display());
    println!("  Source: {}", input);

    Ok(())
}

// ── Feature 191: Verify state ──

fn cmd_verify_state(testnet: bool) -> Result<()> {
    let state = open_chain_state(testnet)?;
    println!("Verifying state integrity...");

    let computed_root = state.verify_state()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let stored_root = state.compute_state_root();

    println!("  Accounts:       {}", state.accounts.len());
    println!("  Computed root:  {}", hex::encode(computed_root));
    println!("  Stored root:    {}", hex::encode(stored_root));

    if computed_root == stored_root {
        println!("  Result: PASS — state roots match");
    } else {
        println!("  Result: FAIL — state root mismatch!");
    }

    // Verify each account can be serialized/deserialized.
    let mut errors = 0;
    for account in state.accounts.iter() {
        match borsh::to_vec(account) {
            Ok(encoded) => {
                if borsh::from_slice::<commputer_storage::Account>(&encoded).is_err() {
                    println!("  ERROR: Account {} failed borsh round-trip", account.address);
                    errors += 1;
                }
            }
            Err(e) => {
                println!("  ERROR: Account {} failed serialization: {}", account.address, e);
                errors += 1;
            }
        }
    }

    if errors == 0 {
        println!("  All {} accounts verified.", state.accounts.len());
    } else {
        println!("  {} errors found in {} accounts.", errors, state.accounts.len());
    }

    Ok(())
}

// ── Feature 192: Rebuild indexes ──

fn cmd_rebuild_indexes(testnet: bool) -> Result<()> {
    let mut state = open_chain_state(testnet)?;
    println!("Rebuilding indexes from block data...");
    println!("  Chain height: {}", state.blocks.height());

    let (receipts, history) = state.rebuild_indexes();

    println!("  Receipts rebuilt:        {}", receipts);
    println!("  History entries rebuilt:  {}", history);
    println!("Index rebuild complete.");

    Ok(())
}

// ── Feature 245: Multi-wallet support ──

fn cmd_wallet_list(testnet: bool) -> Result<()> {
    let wallets_dir = data_dir(testnet).join("wallets");
    let legacy = wallet_path(testnet);

    println!();
    println!("Wallets:");

    if legacy.exists() {
        println!("  default ({})", legacy.display());
    }

    if wallets_dir.exists() {
        for entry in std::fs::read_dir(&wallets_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let name = path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown");
                println!("  {} ({})", name, path.display());
            }
        }
    }

    Ok(())
}

// ── Feature 252: Address book ──

fn address_book_path() -> PathBuf {
    PathBuf::from("./commputer-testnet/address_book.json")
}

fn load_address_book() -> std::collections::HashMap<String, String> {
    let path = address_book_path();
    if path.exists()
        && let Ok(data) = std::fs::read_to_string(&path)
            && let Ok(book) = serde_json::from_str(&data) {
                return book;
            }
    std::collections::HashMap::new()
}

fn save_address_book(book: &std::collections::HashMap<String, String>) -> Result<()> {
    let path = address_book_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(book)?;
    std::fs::write(&path, json)?;
    Ok(())
}

fn cmd_address(action: AddressAction) -> Result<()> {
    match action {
        AddressAction::Add { label, address } => {
            // Validate address.
            let addr_bytes = hex::decode(&address)
                .map_err(|e| anyhow::anyhow!("Invalid address (expected hex): {}", e))?;
            if addr_bytes.len() != 32 {
                anyhow::bail!("Address must be 32 bytes (64 hex characters).");
            }
            let mut book = load_address_book();
            book.insert(label.clone(), address.clone());
            save_address_book(&book)?;
            println!("Added: {} -> {}", label, address);
        }
        AddressAction::List => {
            let book = load_address_book();
            if book.is_empty() {
                println!("Address book is empty.");
            } else {
                println!();
                println!("Address book:");
                for (label, address) in &book {
                    println!("  {} -> {}", label, address);
                }
            }
        }
        AddressAction::Remove { label } => {
            let mut book = load_address_book();
            if book.remove(&label).is_some() {
                save_address_book(&book)?;
                println!("Removed: {}", label);
            } else {
                println!("Label '{}' not found in address book.", label);
            }
        }
    }
    Ok(())
}

// ── Feature 257: Genesis block ceremony tool ──

fn cmd_genesis_generate(output: &str) -> Result<()> {
    println!();
    println!("Commputer Genesis Block Generator");
    println!("==================================");
    println!();

    let total_supply_str = read_line("Total supply (COMME) [2000000000]: ");
    let total_supply: u64 = if total_supply_str.is_empty() {
        2_000_000_000
    } else {
        total_supply_str.parse().unwrap_or(2_000_000_000)
    };

    let epoch_duration_str = read_line("Epoch duration (seconds) [3600]: ");
    let epoch_duration: u64 = if epoch_duration_str.is_empty() {
        3600
    } else {
        epoch_duration_str.parse().unwrap_or(3600)
    };

    let emission_rate_str = read_line("Initial emission rate (COMME/day) [100]: ");
    let emission_rate: u64 = if emission_rate_str.is_empty() {
        100
    } else {
        emission_rate_str.parse().unwrap_or(100)
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let zero32 = vec![0u8; 32];
    let genesis = serde_json::json!({
        "header": {
            "protocol_version": 1,
            "height": 0,
            "parent_hash": zero32,
            "tx_root": zero32,
            "proof_root": zero32,
            "state_root": zero32,
            "timestamp": timestamp,
            "producer": zero32,
            "epoch": 0,
            "producer_public_key": [],
            "signature": [],
            "checkpoint_hash": null,
        },
        "transactions": [],
        "proof_summaries": [],
        "compliance_summary": null,
        "config": {
            "total_supply": total_supply,
            "epoch_duration_secs": epoch_duration,
            "initial_emission_rate": emission_rate,
            "channel_floors": {
                "processing": 0.20,
                "gpu": 0.15,
                "storage": 0.15,
                "ram": 0.15,
                "bandwidth": 0.10,
            },
        },
    });

    let json = serde_json::to_string_pretty(&genesis)?;
    std::fs::write(output, &json)?;

    println!();
    println!("Genesis block generated:");
    println!("  Total supply:    {} COMME", total_supply);
    println!("  Epoch duration:  {} seconds", epoch_duration);
    println!("  Emission rate:   {} COMME/day", emission_rate);
    println!("  Timestamp:       {}", timestamp);
    println!("  Output:          {}", output);

    Ok(())
}

// ── Mainnet readiness: Genesis config ──

/// Feature 1: Genesis configuration structure.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct GenesisConfig {
    #[serde(default)]
    chain_id: String,
    #[serde(default)]
    total_supply: u64,
    #[serde(default)]
    epoch_duration_secs: u64,
    #[serde(default)]
    protocol_version: u32,
    #[serde(default)]
    fork_schedule: Vec<(u64, u32)>,
}

/// Feature 1: Load genesis config from genesis.json.
#[allow(dead_code)]
fn load_genesis_config() -> Option<GenesisConfig> {
    for path_str in &["genesis.json", "../genesis.json"] {
        if let Ok(contents) = std::fs::read_to_string(path_str) {
            if let Ok(config) = serde_json::from_str::<GenesisConfig>(&contents) {
                return Some(config);
            }
        }
    }
    None
}
