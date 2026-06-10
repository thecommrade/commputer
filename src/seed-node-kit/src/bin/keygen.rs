use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
// If you fold this kit into commputer-network, swap the next line for:
//     use libp2p::identity::Keypair;
use libp2p_identity::Keypair;

#[derive(Parser, Debug)]
#[command(
    name = "commputer-keygen",
    about = "Generate a libp2p Ed25519 node keypair for a Commputer seed node",
    long_about = "Generates a fresh Ed25519 keypair, prints the corresponding \
                  libp2p peer ID, and writes the private key to disk in \
                  libp2p protobuf encoding (the format the node expects)."
)]
struct Args {
    /// Output path for the private key file (libp2p protobuf encoding).
    #[arg(long, default_value = "./seed_key.bin")]
    out: PathBuf,

    /// Overwrite the output file if it already exists.
    #[arg(long, default_value_t = false)]
    force: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.out.exists() && !args.force {
        bail!(
            "refusing to overwrite existing file {} (pass --force to override)",
            args.out.display()
        );
    }

    let keypair = Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();

    let bytes = keypair
        .to_protobuf_encoding()
        .context("failed to encode keypair to protobuf")?;

    if let Some(parent) = args.out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir {}", parent.display()))?;
    }

    // Open with truncate so --force replaces content; create_new path is
    // already gated above.
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&args.out)
        .with_context(|| format!("opening {} for write", args.out.display()))?;

    file.write_all(&bytes)
        .with_context(|| format!("writing key bytes to {}", args.out.display()))?;
    file.sync_all().ok();
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&args.out, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", args.out.display()))?;
    }

    println!("Generated libp2p Ed25519 keypair");
    println!("  peer id : {peer_id}");
    println!("  key file: {}", args.out.display());
    println!("  bytes   : {}", bytes.len());
    println!();
    println!("NEXT STEPS");
    println!("  1. Install this key as your node's persistent identity.");
    println!("     The node reads its libp2p identity from ONE fixed path:");
    println!("       ~/.commputer/peer_id");
    println!("     Put this key there BEFORE the first node start:");
    println!("       mkdir -p ~/.commputer && cp {} ~/.commputer/peer_id", args.out.display());
    println!("       chmod 600 ~/.commputer/peer_id");
    println!("     (Or generate straight there: commputer-keygen --out ~/.commputer/peer_id --force)");
    println!("     If you skip this, the node generates a DIFFERENT random identity");
    println!("     on first boot and the peer id above will NOT match your node.");
    println!("  2. Build the multiaddr with:");
    println!("       commputer-multiaddr-builder \\");
    println!("         --peer-id {peer_id} \\");
    println!("         --ip <PUBLIC_IP> --port <PORT>");
    println!("  3. Send that multiaddr to the founder for inclusion in");
    println!("     SEED_NODES (src/network/src/transport.rs:316). It must carry the");
    println!("     peer id of the key installed at ~/.commputer/peer_id.");

    Ok(())
}
