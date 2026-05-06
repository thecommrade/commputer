use std::net::IpAddr;
use std::str::FromStr;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
// If you fold this kit into commputer-network, swap the next line for:
//     use libp2p::identity::PeerId;
use libp2p_identity::PeerId;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Proto {
    Tcp,
    Quic,
}

#[derive(Parser, Debug)]
#[command(
    name = "commputer-multiaddr-builder",
    about = "Build a canonical libp2p multiaddr from peer-id + ip + port",
)]
struct Args {
    /// libp2p peer ID (e.g. starts with `12D3KooW...`). Validated via
    /// `PeerId::from_str` which performs the Multihash check.
    #[arg(long)]
    peer_id: String,

    /// IPv4 or IPv6 address. The transport prefix (/ip4 vs /ip6) is
    /// chosen automatically.
    #[arg(long)]
    ip: String,

    /// Transport port (TCP for `--proto tcp`, UDP for `--proto quic`).
    #[arg(long)]
    port: u16,

    /// Wire protocol layered on top of IP.
    #[arg(long, value_enum, default_value_t = Proto::Tcp)]
    proto: Proto,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Multihash check: PeerId::from_str only succeeds on a valid base58 /
    // CIDv1 multihash representation, which is exactly what we want.
    let peer = PeerId::from_str(&args.peer_id)
        .with_context(|| format!("invalid peer id: {}", args.peer_id))?;

    // Re-render the parsed peer id so the output is canonical even if the
    // operator pasted an oddly-cased copy.
    let peer_canonical = peer.to_string();

    let ip_addr: IpAddr = args
        .ip
        .parse()
        .with_context(|| format!("invalid IP address: {}", args.ip))?;
    let ip_proto = match ip_addr {
        IpAddr::V4(_) => "ip4",
        IpAddr::V6(_) => "ip6",
    };

    let multiaddr = match args.proto {
        Proto::Tcp => format!(
            "/{ip_proto}/{}/tcp/{}/p2p/{peer_canonical}",
            args.ip, args.port
        ),
        Proto::Quic => format!(
            "/{ip_proto}/{}/udp/{}/quic-v1/p2p/{peer_canonical}",
            args.ip, args.port
        ),
    };

    // Print a single line so this composes well in shell pipelines.
    println!("{multiaddr}");

    Ok(())
}
