mod consensus_manager;
mod event_loop;
mod proof_manager;
mod rpc;

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
        #[arg(long, default_value = "true")]
        testnet: bool,
        #[arg(long, default_value = "info")]
        log_level: String,
        #[arg(long, default_value = "9000")]
        port: u16,
        /// Port for the JSON RPC server (transaction submission, status queries)
        #[arg(long, default_value = "9944")]
        rpc_port: u16,
    },
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn data_dir(testnet: bool) -> PathBuf {
    if testnet {
        PathBuf::from("./commputer-testnet")
    } else {
        PathBuf::from("./commputer-data")
    }
}

fn wallet_path(testnet: bool) -> PathBuf {
    data_dir(testnet).join("wallet.json")
}

fn read_password(prompt: &str) -> String {
    eprint!("{}", prompt);
    let mut password = String::new();
    std::io::stdin().read_line(&mut password).unwrap();
    password.trim().to_string()
}

fn read_line(prompt: &str) -> String {
    eprint!("{}", prompt);
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
}

fn create_genesis() -> Block {
    Block {
        header: BlockHeader {
            height: 0,
            parent_hash: BlockHash::GENESIS,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 0, // Epoch zero.
            producer: Address([0u8; 32]), // No producer for genesis.
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
        },
        transactions: vec![],
        proof_summaries: vec![],
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

    let state = open_chain_state(testnet)?;
    let from_addr = *wallet.address();

    // Look up sender nonce.
    let nonce = state
        .accounts
        .get(&from_addr)
        .map(|a| a.nonce)
        .unwrap_or(0);

    // Verify balance.
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

async fn run_node(testnet: bool, log_level: String, port: u16, rpc_port: u16) -> Result<()> {
    // Initialize logging.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.parse().unwrap_or_default()),
        )
        .init();

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

    // Load or create wallet.
    let wallet_file = wallet_path(testnet);
    let wallet = if wallet_file.exists() {
        let password = read_password("Wallet password: ");
        Keystore::load(&wallet_file, &password)
            .map_err(|e| anyhow::anyhow!("{}", e))?
    } else {
        info!("No wallet found — generating ephemeral wallet for this session.");
        Wallet::generate()
    };
    info!("Wallet address: {}", wallet.address());

    // Set up network.
    let mut network = CommpNetwork::new(port)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    info!("P2P peer ID: {}", network.local_peer_id);

    // Connect to seed nodes.
    let seeds_connected = network.connect_to_seeds();
    if seeds_connected > 0 {
        info!("Connected to {} seed nodes", seeds_connected);
    }

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
    });

    // Create event loop and attach RPC channel (shares status with RPC server).
    let mut event_loop = EventLoop::new(state, wallet, network);
    event_loop.attach_rpc(tx_receiver, rpc_state.clone());
    event_loop.auto_register_validator(100);

    // Spawn RPC server in the background.
    tokio::spawn(rpc::start_rpc_server(rpc_port, rpc_state));

    event_loop.run().await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { testnet, log_level, port, rpc_port } => {
            run_node(testnet, log_level, port, rpc_port).await?;
        }
        Commands::Wallet { action } => match action {
            WalletAction::Create { testnet } => cmd_wallet_create(testnet)?,
            WalletAction::Recover { testnet } => cmd_wallet_recover(testnet)?,
            WalletAction::Show { testnet } => cmd_wallet_show(testnet)?,
            WalletAction::Export { testnet } => cmd_wallet_export(testnet)?,
        },
        Commands::Version => {
            println!("commputer {}", env!("CARGO_PKG_VERSION"));
            println!("  Protocol:  /commputer/0.1.0");
            println!("  Network:   testnet");
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
        Commands::Send { to, amount, testnet, rpc_port } => {
            cmd_send(&to, amount, testnet, rpc_port).await?;
        }
    }

    Ok(())
}
