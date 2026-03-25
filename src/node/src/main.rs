mod consensus_manager;
mod event_loop;

use std::path::PathBuf;
use anyhow::Result;
use clap::Parser;
use tracing::{info, warn};

use commputer_core::block::{Block, BlockHeader, BlockHash};
use commputer_core::identity::Address;
use commputer_core::token::{TOTAL_SUPPLY, UNITS_PER_COMME};
use commputer_core::wallet::Wallet;
use commputer_storage::state::ChainState;
use commputer_consensus::emission::EmissionSchedule;
use commputer_network::transport::CommpNetwork;

use crate::event_loop::EventLoop;

#[derive(Parser, Debug)]
#[command(name = "commputer")]
#[command(about = "Commputer: a communal supercomputer coordinated by blockchain")]
#[command(version)]
struct Cli {
    /// Run in testnet mode.
    #[arg(long, default_value = "true")]
    testnet: bool,

    /// Log level (trace, debug, info, warn, error).
    #[arg(long, default_value = "info")]
    log_level: String,

    /// P2P listen port.
    #[arg(long, default_value = "9000")]
    port: u16,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| cli.log_level.parse().unwrap_or_default()),
        )
        .init();

    print_banner();

    if cli.testnet {
        info!("Running in TESTNET mode");
    }

    // Initialize persistent chain state.
    let data_dir = if cli.testnet {
        PathBuf::from("./commputer-testnet")
    } else {
        PathBuf::from("./commputer-data")
    };
    std::fs::create_dir_all(&data_dir)?;
    info!("Data directory: {}", data_dir.display());

    let mut state = ChainState::open(&data_dir)?;

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

    print_chain_status(&state);

    // Load or create wallet.
    let wallet = Wallet::generate(); // For testnet, generate fresh each time.
    info!("Wallet address: {}", wallet.address());

    // Set up network.
    let mut network = CommpNetwork::new(cli.port)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    info!("P2P peer ID: {}", network.local_peer_id);

    // Connect to seed nodes.
    let seeds_connected = network.connect_to_seeds();
    if seeds_connected > 0 {
        info!("Connected to {} seed nodes", seeds_connected);
    }

    // Create and run event loop.
    let mut event_loop = EventLoop::new(state, wallet, network);
    event_loop.run().await;

    Ok(())
}
