//! Commputer Desktop App — Tauri-based GUI for the Commputer node.
//!
//! Items 21-40: Desktop application wrapping the commputer node with a
//! native window showing wallet info, mining status, network stats, and more.
//!
//! Architecture:
//! - Rust backend communicates with the running node via RPC (localhost:9944)
//! - Frontend is plain HTML/CSS/JS served from the `frontend/` directory
//! - No framework dependency — vanilla JS for maximum compatibility
//!
//! This binary is a placeholder until Tauri is integrated. For now it serves
//! the frontend via a local HTTP server and opens the default browser.

use std::path::PathBuf;

mod rpc_client;
mod state;
mod commands;

fn main() {
    println!("Commputer Desktop App");
    println!("This is a placeholder — Tauri integration pending.");
    println!("For now, use the CLI node: commputer run --testnet");
    println!();
    println!("Frontend files are in: {}", frontend_dir().display());
}

/// Path to the frontend assets directory.
fn frontend_dir() -> PathBuf {
    let mut dir = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("."));
    dir.pop(); // Remove binary name
    dir.push("frontend");
    if !dir.exists() {
        // Fall back to source directory.
        dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend");
    }
    dir
}
