use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
// If you fold this kit into commputer-network, you can use the existing
// re-exports `libp2p::Multiaddr`, `libp2p::multiaddr::Protocol`, and
// `libp2p::PeerId`.
use libp2p::Multiaddr;
use libp2p::PeerId;
use libp2p::multiaddr::Protocol;

#[derive(Parser, Debug)]
#[command(
    name = "commputer-verify-multiaddr",
    about = "Parse + best-effort dial check on a libp2p multiaddr",
)]
struct Args {
    /// The multiaddr to verify, e.g.
    /// /ip4/1.2.3.4/tcp/9000/p2p/12D3KooW...
    addr: String,

    /// TCP dial timeout in milliseconds.
    #[arg(long, default_value_t = 2000)]
    timeout_ms: u64,

    /// Exit non-zero if the dial probe fails. Default behavior is to
    /// report unreachable but still exit 0 (parse-only success).
    #[arg(long, default_value_t = false)]
    strict: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Layer 1: parse.
    let addr = Multiaddr::from_str(&args.addr)
        .with_context(|| format!("multiaddr parse failed: {}", args.addr))?;
    println!("parse        : ok");
    println!("multiaddr    : {addr}");

    // Layer 2: extract components.
    let mut host: Option<String> = None;
    let mut tcp_port: Option<u16> = None;
    let mut udp_port: Option<u16> = None;
    let mut is_quic = false;
    let mut peer_id: Option<PeerId> = None;

    for proto in addr.iter() {
        match proto {
            Protocol::Ip4(ip) => host = Some(ip.to_string()),
            Protocol::Ip6(ip) => host = Some(ip.to_string()),
            Protocol::Dns(name) | Protocol::Dns4(name) | Protocol::Dns6(name) => {
                host = Some(name.into_owned());
            }
            Protocol::Tcp(p) => tcp_port = Some(p),
            Protocol::Udp(p) => udp_port = Some(p),
            Protocol::QuicV1 | Protocol::Quic => is_quic = true,
            Protocol::P2p(pid) => peer_id = Some(pid),
            _ => {}
        }
    }

    match peer_id {
        Some(pid) => {
            // PeerId::from_multihash check is implicit in successful parse,
            // but re-run the round-trip for clarity in the report.
            let _round_trip = PeerId::from_str(&pid.to_string())
                .map_err(|e| anyhow!("peer id failed multihash round-trip: {e}"))?;
            println!("peer id      : {pid}");
        }
        None => {
            // Not strictly invalid (a seed entry without /p2p is rare but
            // legal as a hint), but seed nodes really should embed it.
            println!("peer id      : MISSING (seed entries should include /p2p/<id>)");
        }
    }

    // Layer 3: best-effort reachability.
    let host = match host {
        Some(h) => h,
        None => {
            println!("reachable    : unknown (no /ip4 /ip6 or /dns* component)");
            return Ok(());
        }
    };

    if is_quic || (udp_port.is_some() && tcp_port.is_none()) {
        let port = udp_port.unwrap_or(0);
        println!(
            "reachable    : skipped (QUIC/UDP port {port} — not probed by this tool)"
        );
        return Ok(());
    }

    let Some(port) = tcp_port else {
        println!("reachable    : unknown (no /tcp/<port> component)");
        return Ok(());
    };

    let socket_str = format!("{host}:{port}");
    let sock_addrs: Vec<SocketAddr> = match tokio::net::lookup_host(&socket_str).await {
        Ok(iter) => iter.collect(),
        Err(e) => {
            println!("reachable    : false (DNS/parse: {e})");
            if args.strict {
                bail!("strict mode: address {socket_str} did not resolve");
            }
            return Ok(());
        }
    };

    let timeout = Duration::from_millis(args.timeout_ms);
    let mut last_err: Option<String> = None;
    for sa in sock_addrs {
        match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(sa)).await {
            Ok(Ok(_stream)) => {
                println!("reachable    : true ({sa}, tcp dial within {}ms)", args.timeout_ms);
                return Ok(());
            }
            Ok(Err(e)) => last_err = Some(format!("{sa}: {e}")),
            Err(_) => last_err = Some(format!("{sa}: timeout after {}ms", args.timeout_ms)),
        }
    }

    let err = last_err.unwrap_or_else(|| "no candidates".into());
    println!("reachable    : false ({err})");
    if args.strict {
        bail!("strict mode: dial failed: {err}");
    }
    Ok(())
}
