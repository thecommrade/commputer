use libp2p::{
    gossipsub, identify, kad, noise, tcp, yamux,
    relay, dcutr, upnp,
    Multiaddr, PeerId as Libp2pPeerId, Swarm, SwarmBuilder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use tracing::{info, warn, debug};

// ---------------------------------------------------------------------------
// Item 102: NAT type detection based on observed external addresses.
// ---------------------------------------------------------------------------

/// Detected NAT type based on heuristics from the identify protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NatType {
    /// Full cone NAT — any external host can send packets to internal host.
    FullCone,
    /// Restricted cone — only hosts the internal host has sent to can reply.
    RestrictedCone,
    /// Port-restricted cone — only (host, port) pairs the internal host has sent to can reply.
    PortRestricted,
    /// Symmetric NAT — different external port for each destination.
    Symmetric,
    /// Unable to determine NAT type.
    Unknown,
}

impl std::fmt::Display for NatType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NatType::FullCone => write!(f, "Full Cone"),
            NatType::RestrictedCone => write!(f, "Restricted Cone"),
            NatType::PortRestricted => write!(f, "Port Restricted"),
            NatType::Symmetric => write!(f, "Symmetric"),
            NatType::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Detect NAT type from a set of observed external addresses reported by peers.
/// Uses heuristics: if we see the same external IP:port from multiple peers,
/// we're likely behind a full-cone or restricted-cone NAT. If we see different
/// ports, it's symmetric NAT.
pub fn detect_nat_type(observed_addrs: &[String]) -> NatType {
    if observed_addrs.is_empty() {
        return NatType::Unknown;
    }

    // Extract IP:port pairs from multiaddr-style or plain addresses
    let mut ips: HashSet<String> = HashSet::new();
    let mut ports: HashSet<String> = HashSet::new();

    for addr in observed_addrs {
        // Extract IP and port from multiaddr format /ip4/X.X.X.X/tcp/PORT
        let parts: Vec<&str> = addr.split('/').collect();
        let mut ip = None;
        let mut port = None;
        for (i, part) in parts.iter().enumerate() {
            if (*part == "ip4" || *part == "ip6") && i + 1 < parts.len() {
                ip = Some(parts[i + 1].to_string());
            }
            if (*part == "tcp" || *part == "udp") && i + 1 < parts.len() {
                port = Some(parts[i + 1].to_string());
            }
        }
        if let Some(ip_val) = ip {
            ips.insert(ip_val);
        }
        if let Some(port_val) = port {
            ports.insert(port_val);
        }
    }

    if ips.is_empty() {
        return NatType::Unknown;
    }

    // Heuristics:
    // - Multiple different external IPs: likely behind a load balancer or multi-homed (treat as Unknown)
    // - Single IP, single port: Full Cone or Restricted Cone
    // - Single IP, multiple ports: Symmetric NAT
    if ips.len() > 1 {
        // Multiple external IPs — could be multi-homed, hard to classify
        NatType::Unknown
    } else if ports.len() <= 1 {
        // Consistent port across observations — Full Cone or Restricted Cone
        if observed_addrs.len() >= 3 {
            NatType::FullCone
        } else {
            NatType::RestrictedCone
        }
    } else {
        // Different external ports for different peers — Symmetric
        NatType::Symmetric
    }
}

// ---------------------------------------------------------------------------
// Item 103: UPnP status tracking.
// ---------------------------------------------------------------------------

/// Status of UPnP port mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UpnpStatus {
    /// UPnP has not been attempted yet.
    NotAttempted,
    /// UPnP mapping succeeded on the given external address.
    Mapped(String),
    /// UPnP mapping failed.
    Failed(String),
    /// UPnP is not available on this network.
    Unavailable,
}

/// The Commputer P2P network built on libp2p.
///
/// Transport stack:
/// - TCP + Noise + Yamux (traditional, works everywhere)
/// - QUIC (UDP-based, punches through NATs and VPN firewalls better)
/// - Relay + DCUtR (hole-punching for nodes behind restrictive NATs)
/// - UPnP (automatic port mapping on home routers)
///
/// A regular user behind a VPN needs zero configuration. The node:
/// 1. Tries QUIC (UDP) first — works through most firewalls
/// 2. Falls back to TCP if QUIC fails
/// 3. If neither works inbound, connects outbound to relay nodes
/// 4. DCUtR upgrades relay connections to direct connections via hole-punching
/// 5. UPnP attempts automatic port forwarding on the local router
pub struct CommpNetwork {
    pub swarm: Swarm<CommpBehaviour>,
    pub local_peer_id: Libp2pPeerId,
    /// Item 102: Observed external addresses from identify protocol.
    pub observed_addrs: Vec<String>,
    /// Item 103: Current UPnP status.
    pub upnp_status: UpnpStatus,
    /// Item 104: Whether this node is running in relay mode.
    pub relay_mode: bool,
    /// Task T (seed keepalive): seed-key (`"host:port"`) -> peer id learned
    /// from an established connection, so later keepalive ticks can ask the
    /// swarm "still connected?" instead of blind re-dialing.
    pub seed_peer_ids: std::collections::HashMap<String, Libp2pPeerId>,
    /// Task T: exponential dial backoff per seed key. Only its failure
    /// counter is consulted; scheduling goes through `seed_next_allowed_ms`,
    /// which clamps seed waits to `SEED_MAX_BACKOFF_SECS` (60s) — the
    /// generic 300s cap is too long for the network's rendezvous point.
    pub seed_backoff: crate::peer::ConnectionBackoff,
    /// Task T: seed keys learned to be OUR OWN address (the public seed box
    /// runs this same binary) — permanently skipped by the keepalive.
    pub seed_self_keys: std::collections::HashSet<String>,
    /// Task T: seed-key -> earliest next dial attempt (unix ms). Carries both
    /// the clamped failure backoff and the provisional hold after an
    /// unanswered dial-initiation.
    seed_next_allowed_ms: std::collections::HashMap<String, u64>,
}

/// Combined libp2p behaviour.
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct CommpBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
    pub upnp: upnp::tokio::Behaviour,
    /// Dedicated sync protocol — direct peer-to-peer block download,
    /// separate from gossipsub. No rate limiting.
    pub sync: libp2p::request_response::Behaviour<crate::sync_protocol::SyncCodec>,
    /// Direct consensus protocol — block proposals and votes via request-response.
    pub consensus: libp2p::request_response::Behaviour<crate::consensus_protocol::ConsensusCodec>,
    /// Track-2 DA protocol — serve/fetch erasure-coded job chunks over `/commputer/da/1`
    /// (`DaRequest::GetChunk` → `DaResponse::Chunk`). Registered unconditionally (consistent
    /// with `sync`/`consensus`), so the node negotiates the protocol always; but until the
    /// PROTECTED Phase-B `event_loop` `Da` arm lands there is no handler, so inbound requests
    /// go unanswered (inert). Phase B adds the serve path (from the local `DaStore`) with a
    /// per-peer rate limit (P8). `CommpBehaviourEvent::Da` is auto-derived by
    /// `NetworkBehaviour` from this field.
    pub da: libp2p::request_response::Behaviour<crate::da_protocol::DaCodec>,
    /// Peer -> validator attestation — `/commputer/attest/1`. One signed
    /// challenge/response per connection binds a PeerId to a validator Address so
    /// vote intake can count only proven consensus-set peers (QC-009).
    /// `CommpBehaviourEvent::Attest` is auto-derived from this field.
    pub attest: libp2p::request_response::Behaviour<crate::attest_protocol::AttestCodec>,
}

/// Write the persistent libp2p identity key to `path`, owner-only (0600) from
/// the very first byte and refusing to follow a pre-planted symlink.
///
/// On unix we open with `create_new(true).mode(0o600)` (O_CREAT|O_EXCL):
///   - `create_new` fails with `AlreadyExists` if the path already exists OR is
///     a symlink (POSIX: O_CREAT|O_EXCL on a symlink errors EEXIST regardless of
///     the link target), giving O_NOFOLLOW semantics. This closes the old
///     write-then-chmod TOCTOU window where the private key was briefly
///     world/group-readable, and prevents a pre-planted symlink in the data dir
///     from redirecting the key write to an attacker-chosen location.
///   - `mode(0o600)` applies the restrictive permission at creation time, so the
///     key is never readable by group/other even for an instant.
///
/// Mirrors the batch-B `core::keystore` hardening. On non-unix targets we fall
/// back to a plain write (mode bits are not meaningful there).
fn write_new_peer_key(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        f.sync_all().ok();
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

impl CommpNetwork {
    /// Create a new CommpNetwork listening on the given port via TCP and QUIC.
    pub fn new(listen_port: u16) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_with_keypair_path(listen_port, None, "")
    }

    /// Item 3: Create a CommpNetwork with a persistent keypair.
    /// If `keypair_path` is provided, loads the keypair from disk or generates
    /// and saves a new one. This ensures the peer ID survives restarts.
    /// `genesis_hash_hex` is included in the identify agent_version so peers
    /// can verify chain compatibility during handshake.
    pub fn new_with_keypair_path(listen_port: u16, keypair_path: Option<&std::path::Path>, genesis_hash_hex: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let identity = if let Some(path) = keypair_path {
            if path.exists() {
                let bytes = std::fs::read(path)?;
                let keypair = libp2p::identity::Keypair::from_protobuf_encoding(&bytes)?;
                info!("Loaded persistent peer identity from {}", path.display());
                keypair
            } else {
                let keypair = libp2p::identity::Keypair::generate_ed25519();
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let bytes = keypair.to_protobuf_encoding()?;
                write_new_peer_key(path, &bytes)?;
                info!("Generated and saved new peer identity to {}", path.display());
                keypair
            }
        } else {
            libp2p::identity::Keypair::generate_ed25519()
        };

        let mut swarm = SwarmBuilder::with_existing_identity(identity)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                || {
                    let mut cfg = yamux::Config::default();
                    cfg.set_max_num_streams(64);
                    cfg
                },
            )?
            .with_quic()
            // DNS resolution wraps the TCP+QUIC transports so /dns4/ and /dnsaddr/
            // multiaddrs (e.g. a future seed.commputer.xyz seed) resolve at all.
            // Without this, DNS-based multiaddrs are never dialable.
            .with_dns()?
            .with_relay_client(noise::Config::new, || {
                let mut cfg = yamux::Config::default();
                cfg.set_max_num_streams(64);
                cfg
            })?
            .with_behaviour(|key, relay_client| {
                // Gossipsub with 1-second heartbeat
                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    .heartbeat_interval(Duration::from_secs(1))
                    .build()
                    .expect("valid gossipsub config");

                let gossipsub = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config,
                )
                .expect("valid gossipsub behaviour");

                // Kademlia with in-memory store
                let peer_id = key.public().to_peer_id();
                let kademlia = kad::Behaviour::new(
                    peer_id,
                    kad::store::MemoryStore::new(peer_id),
                );

                // Identify with our protocol version and genesis hash
                let agent_version = if genesis_hash_hex.is_empty() {
                    "commputer/0.1.0".to_string()
                } else {
                    format!("commputer/0.1.0/{}", genesis_hash_hex)
                };
                let identify = identify::Behaviour::new(
                    identify::Config::new(
                        "/commputer/0.1.0".to_string(),
                        key.public(),
                    )
                    .with_agent_version(agent_version),
                );

                // DCUtR for direct connection upgrades after relay
                let dcutr = dcutr::Behaviour::new(peer_id);

                // UPnP for automatic port mapping
                let upnp = upnp::tokio::Behaviour::default();

                // Dedicated sync protocol — direct peer-to-peer, no gossipsub
                let sync = crate::sync_protocol::sync_behaviour();
                let consensus = crate::consensus_protocol::consensus_behaviour();
                let da = crate::da_protocol::da_behaviour();
                let attest = crate::attest_protocol::attest_behaviour();

                Ok(CommpBehaviour {
                    gossipsub,
                    kademlia,
                    identify,
                    relay_client,
                    dcutr,
                    upnp,
                    sync,
                    consensus,
                    da,
                    attest,
                })
            })?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(Duration::from_secs(600))
            })
            .build();

        let local_peer_id = *swarm.local_peer_id();

        // Item 7: If listen_port is 0, run in outbound-only mode (no listening).
        if listen_port > 0 {
            // Listen on TCP (traditional)
            let tcp_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{listen_port}").parse()?;
            swarm.listen_on(tcp_addr)?;

            // Listen on QUIC (UDP — better NAT/VPN traversal)
            let quic_addr: Multiaddr = format!("/ip4/0.0.0.0/udp/{listen_port}/quic-v1").parse()?;
            swarm.listen_on(quic_addr)?;

            info!("P2P transport: TCP + QUIC (dual-stack)");
        } else {
            info!("P2P transport: outbound-only mode (no listening ports)");
        }
        info!("P2P encryption: Noise (TCP) / TLS 1.3 (QUIC)");
        info!("P2P features: relay, hole-punching (DCUtR), UPnP");
        info!("P2P protocol: /commputer/0.1.0");

        let mut network = Self {
            swarm,
            local_peer_id,
            observed_addrs: Vec::new(),
            upnp_status: UpnpStatus::NotAttempted,
            relay_mode: false,
            seed_peer_ids: std::collections::HashMap::new(),
            seed_backoff: crate::peer::ConnectionBackoff::new(),
            seed_self_keys: std::collections::HashSet::new(),
            seed_next_allowed_ms: std::collections::HashMap::new(),
        };

        for topic in crate::topics::all_topics() {
            network.swarm.behaviour_mut().gossipsub.subscribe(&topic)?;
        }

        Ok(network)
    }

    /// Dial a remote peer at the given multiaddr.
    pub fn dial(&mut self, addr: Multiaddr) -> Result<(), Box<dyn std::error::Error>> {
        self.swarm.dial(addr)?;
        Ok(())
    }
}

/// Founder-operated seed nodes with a KNOWN, fixed peer identity. Pinned
/// `/p2p/<PEER_ID>`-qualified addresses let libp2p verify the expected peer
/// identity on dial (and dedupe against an existing connection), not just
/// the host. The public seed's peer key is persisted server-side (see
/// `seed-node-kit`) and survives data wipes, so the pin is stable.
///
/// `seed_targets()` prefers these pinned forms over the bare
/// `DEFAULT_TESTNET_SEED_HOSTS` forms of the same host.
pub const SEED_NODES: &[&str] = &[
    // Format: /ip4/<IP>/tcp/<PORT>/p2p/<PEER_ID>
    // Or QUIC: /ip4/<IP>/udp/<PORT>/quic-v1/p2p/<PEER_ID>
    // Or DNS:  /dns4/<HOST>/tcp/<PORT>/p2p/<PEER_ID>
    "/dns4/seed.commputer.xyz/tcp/9000/p2p/12D3KooWLvTRavtUp4q4cNidc2tWcVty6vzoMtAELeqCcmJhVs5t",
    "/dns4/seed.commputer.xyz/udp/9000/quic-v1/p2p/12D3KooWLvTRavtUp4q4cNidc2tWcVty6vzoMtAELeqCcmJhVs5t",
];

/// Compiled-in default testnet seed(s), as `"host:port"`.
///
/// This mirrors `commputer::config::DEFAULT_TESTNET_SEEDS`
/// (`src/node/src/config.rs:9`) — the node crate's display twin, logged (not
/// dialed) at `src/node/src/main.rs:1160`. The network crate cannot depend on
/// the node crate (`node` depends on `network`; the reverse would be a
/// dependency cycle), so the literal is hand-mirrored here rather than
/// imported. Keep the two lists in sync if the seed host/port ever changes.
///
/// Unlike `SEED_NODES` (which expects a founder-curated, peer-id-qualified
/// address), these entries carry no peer id — `connect_to_seeds()` dials
/// them as bare `/dns4/<host>/tcp/<port>` (+ QUIC) multiaddrs, the same
/// without-a-peer-id shape `connect_to_custom_seeds()`/`resolve_dns_seeds()`
/// already dial for CLI `--seeds`/`--dns-seeds` values (see
/// `tcp_to_quic_v1_without_peer_id`, `connect_to_custom_seeds`) — a
/// known-working path, not a new one.
const DEFAULT_TESTNET_SEED_HOSTS: &[&str] = &["seed.commputer.xyz:9000"];

/// Convert a `"host:port"` seed literal into its `/dns4/<host>/tcp/<port>`
/// and `/dns4/<host>/udp/<port>/quic-v1` dial forms.
///
/// Both forms are dialable: `.with_dns()` wraps *both* the TCP and QUIC
/// transports in the swarm builder (see `new_with_keypair_path`), so DNS
/// resolution isn't TCP-only.
///
/// Returns `None` if `host_port` isn't `host:port` shaped (no colon, empty
/// host, or a port that doesn't parse as `u16`). Callers must treat `None`
/// as "skip this entry, log it" — never propagate a panic — since a bad
/// compiled-in literal must not block node startup.
fn dns_seed_multiaddrs(host_port: &str) -> Option<(String, String)> {
    let (host, port_str) = host_port.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    let port: u16 = port_str.parse().ok()?;
    Some((
        format!("/dns4/{host}/tcp/{port}"),
        format!("/dns4/{host}/udp/{port}/quic-v1"),
    ))
}

/// Convert a TCP multiaddr into its QUIC-v1 equivalent.
///
/// Two cases:
///   - `/ip4/X/tcp/N/p2p/<id>`  →  `/ip4/X/udp/N/quic-v1/p2p/<id>`
///     (`/quic-v1` inserted before the embedded peer ID)
///   - `/ip4/X/tcp/N`           →  `/ip4/X/udp/N/quic-v1`
///     (`/quic-v1` appended at the end since there's no embedded peer ID)
///
/// The previous one-liner used `/p2p/` as the insertion anchor and silently
/// emitted a bare `/ip4/X/udp/N` (no codec) for seeds without a peer ID,
/// which libp2p rejected as `MultiaddrNotSupported`.
fn tcp_to_quic_v1(addr_str: &str) -> String {
    if addr_str.contains("/p2p/") {
        addr_str
            .replace("/tcp/", "/udp/")
            .replace("/p2p/", "/quic-v1/p2p/")
    } else {
        format!("{}/quic-v1", addr_str.replace("/tcp/", "/udp/"))
    }
}

// ---------------------------------------------------------------------------
// Task T: seed keepalive — the 2026-07-24 formation wedge happened because
// compiled-in seeds were dialed exactly once at boot and never again (the
// event-loop reconnect only re-dials CLI --seeds, and only at 0 peers). The
// machinery below re-dials any seed we are not visibly connected to, on a
// periodic tick, with a seed-specific backoff cap.
// ---------------------------------------------------------------------------

/// Seeds are the network's rendezvous point: never wait longer than this
/// between re-dial attempts of a seed, regardless of the generic
/// `ConnectionBackoff` 300s cap.
const SEED_MAX_BACKOFF_SECS: u64 = 60;

/// Provisional hold after initiating a dial: an unanswered dial must not be
/// re-fired on every maintenance tick (~30s), but a dead seed must be
/// retried within a minute.
const SEED_DIAL_HOLD_MS: u64 = 60_000;

/// `"host:port"` grouping key for a seed multiaddr — first host component
/// (dns/dns4/dns6/ip4/ip6) plus first port (tcp/udp). All transport forms
/// (TCP, QUIC) and pinned/un-pinned variants of one seed share one key.
fn seed_key_of(addr: &Multiaddr) -> Option<String> {
    use libp2p::multiaddr::Protocol;
    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    for proto in addr.iter() {
        match proto {
            Protocol::Dns(h) | Protocol::Dns4(h) | Protocol::Dns6(h) => {
                host.get_or_insert(h.to_string());
            }
            Protocol::Ip4(ip) => {
                host.get_or_insert(ip.to_string());
            }
            Protocol::Ip6(ip) => {
                host.get_or_insert(ip.to_string());
            }
            Protocol::Tcp(p) | Protocol::Udp(p) => {
                port.get_or_insert(p);
            }
            _ => {}
        }
        if host.is_some() && port.is_some() {
            break;
        }
    }
    Some(format!("{}:{}", host?, port?))
}

/// The peer id embedded in a `/p2p/<id>` component, if any.
fn embedded_peer_id(addr: &Multiaddr) -> Option<Libp2pPeerId> {
    use libp2p::multiaddr::Protocol;
    addr.iter().find_map(|proto| match proto {
        Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

/// `addr` with any `/p2p/<id>` component removed.
fn strip_p2p(addr: &Multiaddr) -> Multiaddr {
    use libp2p::multiaddr::Protocol;
    addr.iter()
        .filter(|proto| !matches!(proto, Protocol::P2p(_)))
        .collect()
}

/// Equality ignoring any trailing `/p2p/<id>` component: a connection's
/// remote address may carry (or omit) the peer id relative to the seed
/// target we dialed.
pub fn addr_matches_seed(remote: &Multiaddr, target: &Multiaddr) -> bool {
    strip_p2p(remote) == strip_p2p(target)
}

/// Union of all seed sources, grouped by stable seed key:
/// `SEED_NODES` (pinned) + `DEFAULT_TESTNET_SEED_HOSTS` (bare TCP + QUIC)
/// + each CLI custom seed as given plus its QUIC twin. When a key has any
/// pinned (`/p2p`) form, only pinned forms are kept — the pin lets libp2p
/// verify identity and dedupe against an existing connection.
pub fn seed_targets(custom_seeds: &[String]) -> Vec<(String, Vec<Multiaddr>)> {
    fn push_addr(
        order: &mut Vec<String>,
        groups: &mut std::collections::HashMap<String, Vec<Multiaddr>>,
        addr: Multiaddr,
    ) {
        let Some(key) = seed_key_of(&addr) else { return };
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        let entry = groups.entry(key).or_default();
        if !entry.contains(&addr) {
            entry.push(addr);
        }
    }

    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<Multiaddr>> =
        std::collections::HashMap::new();

    for addr_str in SEED_NODES {
        if let Ok(addr) = addr_str.parse::<Multiaddr>() {
            push_addr(&mut order, &mut groups, addr);
        }
    }
    for host_port in DEFAULT_TESTNET_SEED_HOSTS {
        if let Some((tcp_str, quic_str)) = dns_seed_multiaddrs(host_port) {
            if let Ok(addr) = tcp_str.parse::<Multiaddr>() {
                push_addr(&mut order, &mut groups, addr);
            }
            if let Ok(addr) = quic_str.parse::<Multiaddr>() {
                push_addr(&mut order, &mut groups, addr);
            }
        }
    }
    for addr_str in custom_seeds {
        if let Ok(addr) = addr_str.parse::<Multiaddr>() {
            push_addr(&mut order, &mut groups, addr);
            if addr_str.contains("/tcp/") {
                if let Ok(quic) = tcp_to_quic_v1(addr_str).parse::<Multiaddr>() {
                    push_addr(&mut order, &mut groups, quic);
                }
            }
        }
    }

    order
        .into_iter()
        .map(|key| {
            let mut addrs = groups.remove(&key).unwrap_or_default();
            if addrs.iter().any(|a| embedded_peer_id(a).is_some()) {
                addrs.retain(|a| embedded_peer_id(a).is_some());
            }
            (key, addrs)
        })
        .collect()
}

/// Outcome of the per-seed keepalive decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeedDialDecision {
    Dial,
    SkipSelf,
    SkipConnected,
    SkipBackoff,
}

/// Pure skip/dial decision for one seed key — computed from snapshots and an
/// `is_connected` probe so it is unit-testable without a live swarm.
#[allow(clippy::too_many_arguments)]
fn plan_seed_dial(
    key: &str,
    addrs: &[Multiaddr],
    seed_self_keys: &HashSet<String>,
    seed_peer_ids: &std::collections::HashMap<String, Libp2pPeerId>,
    next_allowed_ms: &std::collections::HashMap<String, u64>,
    is_connected: impl Fn(&Libp2pPeerId) -> bool,
    local_peer: &Libp2pPeerId,
    now_ms: u64,
) -> SeedDialDecision {
    if seed_self_keys.contains(key) {
        return SeedDialDecision::SkipSelf;
    }
    // A pinned form naming our own id is the seed box looking at itself.
    if addrs.iter().filter_map(embedded_peer_id).any(|id| id == *local_peer) {
        return SeedDialDecision::SkipSelf;
    }
    if addrs.iter().filter_map(embedded_peer_id).any(|id| is_connected(&id)) {
        return SeedDialDecision::SkipConnected;
    }
    if let Some(id) = seed_peer_ids.get(key) {
        if is_connected(id) {
            return SeedDialDecision::SkipConnected;
        }
    }
    match next_allowed_ms.get(key) {
        Some(next) if now_ms < *next => SeedDialDecision::SkipBackoff,
        _ => SeedDialDecision::Dial,
    }
}

/// Failure wait for a seed key: the generic exponential backoff value,
/// clamped to `SEED_MAX_BACKOFF_SECS`.
fn clamped_seed_wait_ms(backoff_secs: u64) -> u64 {
    backoff_secs.clamp(1, SEED_MAX_BACKOFF_SECS) * 1000
}

impl CommpNetwork {
    /// Dial all built-in seed nodes: the founder-curated `SEED_NODES`
    /// literal (empty until the seed box has a fixed peer id) plus the
    /// compiled-in DNS defaults (`DEFAULT_TESTNET_SEED_HOSTS`). Returns the
    /// number of dials successfully queued.
    ///
    /// A default host that fails to resolve is NOT an error here: DNS
    /// resolution happens later, asynchronously, inside the `.with_dns()`-
    /// wrapped transport (see `new_with_keypair_path`), so a bad or
    /// unreachable seed hostname surfaces only as a later
    /// `SwarmEvent::OutgoingConnectionError` — never a panic and never a
    /// block on startup here.
    pub fn connect_to_seeds(&mut self) -> usize {
        let mut connected = 0;
        for addr_str in SEED_NODES {
            if let Ok(addr) = addr_str.parse::<Multiaddr>()
                && self.dial(addr).is_ok() {
                    connected += 1;
                }
        }
        for host_port in DEFAULT_TESTNET_SEED_HOSTS {
            match dns_seed_multiaddrs(host_port) {
                Some((tcp_str, quic_str)) => {
                    if let Ok(addr) = tcp_str.parse::<Multiaddr>()
                        && self.dial(addr).is_ok() {
                            info!("Dialed default DNS seed: {}", tcp_str);
                            connected += 1;
                        }
                    if let Ok(addr) = quic_str.parse::<Multiaddr>()
                        && self.dial(addr).is_ok() {
                            info!("Dialed default DNS seed (QUIC): {}", quic_str);
                            connected += 1;
                        }
                }
                None => {
                    warn!(
                        "Default seed literal '{}' is not host:port shaped, skipping",
                        host_port
                    );
                }
            }
        }
        connected
    }

    /// Connect to custom seed nodes from CLI --seeds arg.
    /// Tries both TCP and QUIC for each seed address.
    pub fn connect_to_custom_seeds(&mut self, seeds: &[String]) -> usize {
        let mut connected = 0;
        for addr_str in seeds {
            // Try the address as given
            match addr_str.parse::<Multiaddr>() {
                Ok(addr) => {
                    match self.dial(addr) {
                        Ok(()) => {
                            info!("Dialed seed: {}", addr_str);
                            connected += 1;
                        }
                        Err(e) => {
                            warn!("Failed to dial seed {}: {}", addr_str, e);
                        }
                    }

                    // If the given address is TCP, also try QUIC variant.
                    if addr_str.contains("/tcp/") {
                        let quic_addr = tcp_to_quic_v1(&addr_str);
                        if let Ok(addr) = quic_addr.parse::<Multiaddr>() {
                            match self.dial(addr) {
                                Ok(()) => {
                                    info!("Dialed seed via QUIC: {}", quic_addr);
                                    connected += 1;
                                }
                                Err(e) => {
                                    debug!("QUIC dial to seed failed (TCP may still work): {}", e);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Invalid seed multiaddr '{}': {}", addr_str, e);
                }
            }
        }
        connected
    }

    /// Task T: record an established connection against the seed table.
    /// Wire-in point: the (PROTECTED) event loop's `ConnectionEstablished`
    /// arm, with the endpoint's remote address and the same `custom_seeds`
    /// slice passed to `ensure_seed_connections`. Learns the seed's peer id
    /// so keepalive ticks can ask the swarm "still connected?" instead of
    /// blind re-dialing; a seed that answers with OUR OWN peer id (the
    /// public seed box runs this same binary) is permanently skipped.
    pub fn note_seed_connection(
        &mut self,
        peer_id: &Libp2pPeerId,
        remote: &Multiaddr,
        custom_seeds: &[String],
    ) {
        for (key, addrs) in seed_targets(custom_seeds) {
            if !addrs.iter().any(|target| addr_matches_seed(remote, target)) {
                continue;
            }
            if *peer_id == self.local_peer_id {
                info!(seed = %key, "seed address is our own — keepalive will skip it permanently");
                self.seed_self_keys.insert(key);
                continue;
            }
            debug!(seed = %key, peer = %peer_id, "seed connection established — learned peer id");
            self.seed_peer_ids.insert(key.clone(), *peer_id);
            self.seed_backoff.record_success(&key);
            self.seed_next_allowed_ms.remove(&key);
        }
    }

    /// Task T: keepalive tick — re-dial every seed we are not visibly
    /// connected to. Wire-in point: the (PROTECTED) event loop's periodic
    /// maintenance tick (~30s), unconditionally — NOT only at 0 peers, which
    /// is exactly how the star topology wedged: compiled-in seeds were never
    /// re-dialed after the boot attempt. Returns the number of seed keys for
    /// which a dial was initiated.
    pub fn ensure_seed_connections(&mut self, custom_seeds: &[String]) -> usize {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut dialed = 0;
        for (key, addrs) in seed_targets(custom_seeds) {
            let decision = {
                let swarm = &self.swarm;
                plan_seed_dial(
                    &key,
                    &addrs,
                    &self.seed_self_keys,
                    &self.seed_peer_ids,
                    &self.seed_next_allowed_ms,
                    |id| swarm.is_connected(id),
                    &self.local_peer_id,
                    now_ms,
                )
            };
            match decision {
                SeedDialDecision::SkipSelf | SeedDialDecision::SkipConnected => {}
                SeedDialDecision::SkipBackoff => {
                    debug!(seed = %key, "seed re-dial suppressed by backoff");
                }
                SeedDialDecision::Dial => {
                    info!(seed = %key, "seed not connected — re-dialing all address forms");
                    let mut initiated = false;
                    let mut immediate_err = false;
                    let mut self_hit = false;
                    for addr in &addrs {
                        match self.swarm.dial(addr.clone()) {
                            Ok(()) => initiated = true,
                            Err(libp2p::swarm::DialError::LocalPeerId { .. }) => {
                                self_hit = true;
                                break;
                            }
                            Err(e) => {
                                debug!(seed = %key, addr = %addr, error = %e, "seed dial failed to initiate");
                                immediate_err = true;
                            }
                        }
                    }
                    if self_hit {
                        info!(seed = %key, "seed is our own identity — keepalive will skip it permanently");
                        self.seed_self_keys.insert(key);
                        continue;
                    }
                    if immediate_err {
                        self.seed_backoff.record_failure(&key, now_ms);
                    }
                    if initiated {
                        dialed += 1;
                        // Provisional hold: an unanswered dial must not be
                        // re-fired every maintenance tick.
                        self.seed_next_allowed_ms.insert(key, now_ms + SEED_DIAL_HOLD_MS);
                    } else {
                        let wait_ms =
                            clamped_seed_wait_ms(self.seed_backoff.current_backoff_secs(&key));
                        self.seed_next_allowed_ms.insert(key, now_ms + wait_ms);
                    }
                }
            }
        }
        dialed
    }

    /// Resolve DNS seed domains (A records) and construct multiaddrs.
    pub fn resolve_dns_seeds(&mut self, domains: &[String], port: u16) -> usize {
        let mut connected = 0;
        for domain in domains {
            match std::net::ToSocketAddrs::to_socket_addrs(&(domain.as_str(), port)) {
                Ok(addrs) => {
                    for addr in addrs {
                        let (tcp_str, quic_str) = match addr {
                            std::net::SocketAddr::V4(v4) => (
                                format!("/ip4/{}/tcp/{}", v4.ip(), v4.port()),
                                format!("/ip4/{}/udp/{}/quic-v1", v4.ip(), v4.port()),
                            ),
                            std::net::SocketAddr::V6(v6) => (
                                format!("/ip6/{}/tcp/{}", v6.ip(), v6.port()),
                                format!("/ip6/{}/udp/{}/quic-v1", v6.ip(), v6.port()),
                            ),
                        };
                        // Try TCP
                        if let Ok(multiaddr) = tcp_str.parse::<Multiaddr>()
                            && self.dial(multiaddr).is_ok() {
                                info!("Dialed DNS seed {} -> {} (TCP)", domain, tcp_str);
                                connected += 1;
                            }
                        // Also try QUIC
                        if let Ok(multiaddr) = quic_str.parse::<Multiaddr>()
                            && self.dial(multiaddr).is_ok() {
                                info!("Dialed DNS seed {} -> {} (QUIC)", domain, quic_str);
                                connected += 1;
                            }
                    }
                }
                Err(e) => {
                    warn!("Failed to resolve DNS seed '{}': {}", domain, e);
                }
            }
        }
        connected
    }

    /// Trigger Kademlia bootstrap for peer discovery.
    pub fn bootstrap_kademlia(&mut self) {
        match self.swarm.behaviour_mut().kademlia.bootstrap() {
            Ok(_query_id) => {
                info!("Kademlia bootstrap initiated");
            }
            Err(e) => {
                debug!("Kademlia bootstrap failed (may be no known peers yet): {:?}", e);
            }
        }
    }

    /// Log transport status on startup.
    pub fn log_encryption_status(&self) {
        info!("P2P encryption: Noise (TCP) / TLS 1.3 (QUIC)");
        info!("P2P transport: TCP + QUIC dual-stack");
        info!("P2P features: relay, DCUtR hole-punching, UPnP");
        info!("P2P protocol: /commputer/0.1.0");
    }

    /// Item 102: Detect NAT type from observed external addresses.
    pub fn detect_nat_type(&self) -> NatType {
        let nat_type = detect_nat_type(&self.observed_addrs);
        info!("NAT type detected: {}", nat_type);
        nat_type
    }

    /// Item 102: Record an observed external address from the identify protocol.
    pub fn record_observed_addr(&mut self, addr: String) {
        if !self.observed_addrs.contains(&addr) {
            debug!("New observed external address: {}", addr);
            self.observed_addrs.push(addr);
        }
    }

    /// Item 103: Get UPnP mapping status.
    pub fn upnp_status(&self) -> &UpnpStatus {
        &self.upnp_status
    }

    /// Item 103: Update UPnP status and log.
    pub fn set_upnp_status(&mut self, status: UpnpStatus) {
        match &status {
            UpnpStatus::Mapped(addr) => info!("UPnP mapping succeeded: {}", addr),
            UpnpStatus::Failed(reason) => warn!("UPnP mapping failed: {}", reason),
            UpnpStatus::Unavailable => info!("UPnP not available on this network"),
            UpnpStatus::NotAttempted => {}
        }
        self.upnp_status = status;
    }

    /// Item 104: Enable relay mode — node forwards traffic for NAT-ed peers.
    pub fn enable_relay_mode(&mut self) {
        self.relay_mode = true;
        info!("Relay mode enabled — this node will forward traffic for NAT-ed peers");
    }

    /// Item 104: Check if running in relay mode.
    pub fn is_relay_mode(&self) -> bool {
        self.relay_mode
    }

    /// Item 111: Check if a connected peer supports QUIC and attempt upgrade.
    /// Returns true if a QUIC dial was attempted.
    pub fn try_upgrade_to_quic(&mut self, peer_addr: &Multiaddr) -> bool {
        let addr_str = peer_addr.to_string();
        // Only upgrade TCP connections
        if !addr_str.contains("/tcp/") {
            return false;
        }
        // Construct QUIC equivalent
        let quic_addr_str = addr_str
            .replace("/tcp/", "/udp/")
            .replace("/p2p/", "/quic-v1/p2p/");
        if let Ok(quic_addr) = quic_addr_str.parse::<Multiaddr>() {
            match self.dial(quic_addr) {
                Ok(()) => {
                    debug!("Attempting QUIC upgrade for peer at {}", addr_str);
                    true
                }
                Err(e) => {
                    debug!("QUIC upgrade failed for {}: {}", addr_str, e);
                    false
                }
            }
        } else {
            false
        }
    }

    /// Item 113: Resolve DNS TXT records for seed node multiaddrs.
    /// TXT records should contain multiaddr strings, one per record.
    pub fn resolve_dns_txt_seeds(&mut self, domains: &[String]) -> usize {
        let mut connected = 0;
        for domain in domains {
            // Use the trust-dns/hickory resolver or fall back to system resolver
            // For now, parse TXT-record-style multiaddrs from a well-known subdomain
            let txt_domain = format!("_dnsaddr.{}", domain);
            info!("Resolving DNS TXT seeds from {}", txt_domain);

            // System DNS resolution for TXT records is not directly available in std.
            // We document the format and resolve A/AAAA records as a fallback.
            match std::net::ToSocketAddrs::to_socket_addrs(&(domain.as_str(), 9000u16)) {
                Ok(addrs) => {
                    for addr in addrs {
                        let multiaddr_str = match addr {
                            std::net::SocketAddr::V4(v4) => {
                                format!("/ip4/{}/tcp/{}", v4.ip(), v4.port())
                            }
                            std::net::SocketAddr::V6(v6) => {
                                format!("/ip6/{}/tcp/{}", v6.ip(), v6.port())
                            }
                        };
                        if let Ok(multiaddr) = multiaddr_str.parse::<Multiaddr>()
                            && self.dial(multiaddr).is_ok()
                        {
                            info!("Connected to DNS TXT seed {} -> {}", domain, multiaddr_str);
                            connected += 1;
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to resolve DNS TXT seed '{}': {}", txt_domain, e);
                }
            }
        }
        connected
    }
}

// ---------------------------------------------------------------------------
// Item 116: Network traffic statistics.
// ---------------------------------------------------------------------------

/// Tracks bytes sent/received per protocol per peer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrafficStats {
    /// Total bytes sent.
    pub bytes_sent: u64,
    /// Total bytes received.
    pub bytes_received: u64,
    /// Per-peer byte counts: peer_id_hex -> (sent, received).
    pub per_peer: std::collections::HashMap<String, (u64, u64)>,
    /// Per-protocol byte counts: protocol_name -> (sent, received).
    pub per_protocol: std::collections::HashMap<String, (u64, u64)>,
}

impl TrafficStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record bytes sent to a peer on a given protocol.
    pub fn record_sent(&mut self, peer_id: &str, protocol: &str, bytes: u64) {
        self.bytes_sent += bytes;
        let entry = self.per_peer.entry(peer_id.to_string()).or_insert((0, 0));
        entry.0 += bytes;
        let proto_entry = self.per_protocol.entry(protocol.to_string()).or_insert((0, 0));
        proto_entry.0 += bytes;
    }

    /// Record bytes received from a peer on a given protocol.
    pub fn record_received(&mut self, peer_id: &str, protocol: &str, bytes: u64) {
        self.bytes_received += bytes;
        let entry = self.per_peer.entry(peer_id.to_string()).or_insert((0, 0));
        entry.1 += bytes;
        let proto_entry = self.per_protocol.entry(protocol.to_string()).or_insert((0, 0));
        proto_entry.1 += bytes;
    }

    /// Get a summary for display / RPC.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "bytes_sent": self.bytes_sent,
            "bytes_received": self.bytes_received,
            "per_protocol": self.per_protocol,
            "peer_count": self.per_peer.len(),
        })
    }
}

// ---------------------------------------------------------------------------
// Item 117: Bandwidth throttling with token bucket algorithm.
// ---------------------------------------------------------------------------

/// Token bucket bandwidth throttler.
/// Allows bursts up to the bucket capacity, refills at `rate_bytes_per_sec`.
#[derive(Debug, Clone)]
pub struct BandwidthThrottler {
    /// Maximum upload rate in bytes per second.
    pub max_upload_bps: u64,
    /// Maximum download rate in bytes per second.
    pub max_download_bps: u64,
    /// Current upload tokens available.
    upload_tokens: u64,
    /// Current download tokens available.
    download_tokens: u64,
    /// Last refill timestamp (unix ms).
    last_refill_ms: u64,
    /// Bucket capacity (max burst size).
    bucket_capacity: u64,
}

impl BandwidthThrottler {
    /// Create a new throttler with the given upload/download limits in bytes/sec.
    pub fn new(max_upload_bps: u64, max_download_bps: u64) -> Self {
        let capacity = max_upload_bps.max(max_download_bps);
        Self {
            max_upload_bps,
            max_download_bps,
            upload_tokens: capacity,
            download_tokens: capacity,
            last_refill_ms: 0,
            bucket_capacity: capacity,
        }
    }

    /// Create an unlimited throttler (no rate limiting).
    pub fn unlimited() -> Self {
        Self::new(u64::MAX / 2, u64::MAX / 2)
    }

    /// Refill tokens based on elapsed time.
    pub fn refill(&mut self, now_ms: u64) {
        if self.last_refill_ms == 0 {
            self.last_refill_ms = now_ms;
            return;
        }
        let elapsed_ms = now_ms.saturating_sub(self.last_refill_ms);
        if elapsed_ms == 0 {
            return;
        }
        let upload_refill = self.max_upload_bps * elapsed_ms / 1000;
        let download_refill = self.max_download_bps * elapsed_ms / 1000;
        self.upload_tokens = (self.upload_tokens + upload_refill).min(self.bucket_capacity);
        self.download_tokens = (self.download_tokens + download_refill).min(self.bucket_capacity);
        self.last_refill_ms = now_ms;
    }

    /// Try to consume upload tokens. Returns true if allowed.
    pub fn try_upload(&mut self, bytes: u64) -> bool {
        if bytes <= self.upload_tokens {
            self.upload_tokens -= bytes;
            true
        } else {
            false
        }
    }

    /// Try to consume download tokens. Returns true if allowed.
    pub fn try_download(&mut self, bytes: u64) -> bool {
        if bytes <= self.download_tokens {
            self.download_tokens -= bytes;
            true
        } else {
            false
        }
    }

    /// Check remaining upload capacity.
    pub fn upload_remaining(&self) -> u64 {
        self.upload_tokens
    }

    /// Check remaining download capacity.
    pub fn download_remaining(&self) -> u64 {
        self.download_tokens
    }
}

// ---------------------------------------------------------------------------
// Item 112: Peer exchange protocol.
// ---------------------------------------------------------------------------

/// Periodically shares known peer addresses via gossipsub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerExchange {
    /// Known peer addresses to share.
    pub peer_addrs: Vec<PeerExchangeEntry>,
    /// Timestamp of the exchange.
    pub timestamp_ms: u64,
}

/// A single entry in a peer exchange message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerExchangeEntry {
    /// Peer's multiaddr.
    pub addr: String,
    /// When we last successfully connected to this peer.
    pub last_connected_ms: u64,
}

impl PeerExchange {
    /// Create a new peer exchange message from known peers.
    pub fn from_peers(peers: &[PeerExchangeEntry], now_ms: u64) -> Self {
        Self {
            peer_addrs: peers.to_vec(),
            timestamp_ms: now_ms,
        }
    }

    /// Serialize to bytes for gossipsub transmission.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from bytes received via gossipsub.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nat_type_unknown_empty() {
        assert_eq!(detect_nat_type(&[]), NatType::Unknown);
    }

    #[test]
    fn nat_type_full_cone() {
        let addrs = vec![
            "/ip4/1.2.3.4/tcp/9000".to_string(),
            "/ip4/1.2.3.4/tcp/9000".to_string(),
            "/ip4/1.2.3.4/tcp/9000".to_string(),
        ];
        assert_eq!(detect_nat_type(&addrs), NatType::FullCone);
    }

    #[test]
    fn nat_type_symmetric() {
        let addrs = vec![
            "/ip4/1.2.3.4/tcp/9000".to_string(),
            "/ip4/1.2.3.4/tcp/9001".to_string(),
            "/ip4/1.2.3.4/tcp/9002".to_string(),
        ];
        assert_eq!(detect_nat_type(&addrs), NatType::Symmetric);
    }

    #[test]
    fn traffic_stats_tracking() {
        let mut stats = TrafficStats::new();
        stats.record_sent("peer1", "gossipsub", 1000);
        stats.record_received("peer1", "gossipsub", 2000);
        stats.record_sent("peer2", "kademlia", 500);

        assert_eq!(stats.bytes_sent, 1500);
        assert_eq!(stats.bytes_received, 2000);
        assert_eq!(stats.per_peer.len(), 2);
        assert_eq!(stats.per_protocol.len(), 2);
    }

    #[test]
    fn bandwidth_throttler_basic() {
        let mut throttler = BandwidthThrottler::new(1000, 1000);
        throttler.refill(0);
        assert!(throttler.try_upload(500));
        assert_eq!(throttler.upload_remaining(), 500);
        assert!(throttler.try_upload(500));
        assert!(!throttler.try_upload(1)); // Exhausted
    }

    #[test]
    fn bandwidth_throttler_refill() {
        let mut throttler = BandwidthThrottler::new(1000, 1000);
        throttler.refill(1000); // Initialize timestamp
        throttler.upload_tokens = 0;
        throttler.download_tokens = 0;
        throttler.refill(2000); // 1 second later
        assert_eq!(throttler.upload_remaining(), 1000);
    }

    #[test]
    fn peer_exchange_roundtrip() {
        let entries = vec![PeerExchangeEntry {
            addr: "/ip4/1.2.3.4/tcp/9000".to_string(),
            last_connected_ms: 1000,
        }];
        let exchange = PeerExchange::from_peers(&entries, 2000);
        let bytes = exchange.to_bytes();
        let decoded = PeerExchange::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.peer_addrs.len(), 1);
        assert_eq!(decoded.timestamp_ms, 2000);
    }

    #[test]
    fn upnp_status_display() {
        let status = UpnpStatus::Mapped("1.2.3.4:9000".to_string());
        assert!(matches!(status, UpnpStatus::Mapped(_)));
    }

    #[test]
    fn tcp_to_quic_v1_with_embedded_peer_id() {
        // Use a real PeerId so the transformed string round-trips through parse().
        let peer_id = libp2p::PeerId::random();
        let tcp = format!("/ip4/127.0.0.1/tcp/19001/p2p/{}", peer_id);
        let quic = tcp_to_quic_v1(&tcp);
        assert_eq!(quic, format!("/ip4/127.0.0.1/udp/19001/quic-v1/p2p/{}", peer_id));
        assert!(quic.parse::<Multiaddr>().is_ok(), "expected valid multiaddr, got {}", quic);
    }

    #[test]
    fn dns4_multiaddr_parses() {
        // With `.with_dns()` in the transport chain, /dns4/ seed multiaddrs are
        // dialable. Confirm the multiaddr form itself is valid so a future
        // seed.commputer.xyz seed can be listed in SEED_NODES.
        assert!(
            "/dns4/seed.commputer.xyz/tcp/9000".parse::<Multiaddr>().is_ok(),
            "dns4 tcp multiaddr should parse"
        );
        assert!(
            "/dns4/seed.commputer.xyz/udp/9000/quic-v1".parse::<Multiaddr>().is_ok(),
            "dns4 quic multiaddr should parse"
        );
    }

    #[tokio::test]
    async fn network_builds_with_dns_transport() {
        // Outbound-only build exercises the full SwarmBuilder chain including
        // `.with_dns()`. If DNS transport failed to construct, this would Err.
        let net = CommpNetwork::new(0);
        assert!(net.is_ok(), "network with DNS transport should build: {:?}", net.err());
    }

    #[test]
    fn tcp_to_quic_v1_without_peer_id() {
        // Regression: seeds without an embedded peer ID (common for local
        // bootstrap or DNS-resolved seeds) used to produce /ip4/X/udp/N
        // with no codec, which libp2p rejected.
        let tcp = "/ip4/127.0.0.1/tcp/19001";
        let quic = tcp_to_quic_v1(tcp);
        assert_eq!(quic, "/ip4/127.0.0.1/udp/19001/quic-v1");
        assert!(quic.parse::<Multiaddr>().is_ok(), "expected valid multiaddr, got {}", quic);
    }

    // -- Task A: default seed DNS-dialing (empty-SEED_NODES gap) ------------

    #[test]
    fn dns_seed_multiaddrs_converts_host_port() {
        let (tcp, quic) = dns_seed_multiaddrs("seed.commputer.xyz:9000")
            .expect("well-formed host:port must convert");
        assert_eq!(tcp, "/dns4/seed.commputer.xyz/tcp/9000");
        assert_eq!(quic, "/dns4/seed.commputer.xyz/udp/9000/quic-v1");
        assert!(tcp.parse::<Multiaddr>().is_ok(), "expected valid multiaddr, got {}", tcp);
        assert!(quic.parse::<Multiaddr>().is_ok(), "expected valid multiaddr, got {}", quic);
    }

    #[test]
    fn dns_seed_multiaddrs_rejects_missing_colon() {
        assert_eq!(dns_seed_multiaddrs("seed.commputer.xyz"), None);
    }

    #[test]
    fn dns_seed_multiaddrs_rejects_non_numeric_port() {
        assert_eq!(dns_seed_multiaddrs("seed.commputer.xyz:notaport"), None);
    }

    #[test]
    fn dns_seed_multiaddrs_rejects_empty_host() {
        assert_eq!(dns_seed_multiaddrs(":9000"), None);
    }

    #[test]
    fn dns_seed_multiaddrs_rejects_empty_port() {
        assert_eq!(dns_seed_multiaddrs("seed.commputer.xyz:"), None);
    }

    #[test]
    fn default_testnet_seed_hosts_are_well_formed() {
        // Whatever the mirrored default-seed literal is, it must convert —
        // catches a future typo'd literal at test time rather than at
        // startup (where a malformed entry is merely skipped + logged).
        for host_port in DEFAULT_TESTNET_SEED_HOSTS {
            assert!(
                dns_seed_multiaddrs(host_port).is_some(),
                "default seed literal '{}' is not host:port shaped",
                host_port
            );
        }
    }

    #[tokio::test]
    async fn connect_to_seeds_dials_default_dns_seeds() {
        // Outbound-only (port 0) network, same construction the existing
        // `network_builds_with_dns_transport` test uses. The default DNS
        // seed(s) should be queued for dial (TCP + QUIC each) without
        // requiring the hostname to actually resolve — resolution happens
        // later, asynchronously, inside the `.with_dns()`-wrapped transport.
        let mut net = CommpNetwork::new(0).expect("network should build");
        let connected = net.connect_to_seeds();
        assert!(
            connected >= DEFAULT_TESTNET_SEED_HOSTS.len() * 2,
            "expected at least {} dials (TCP+QUIC per default seed), got {}",
            DEFAULT_TESTNET_SEED_HOSTS.len() * 2,
            connected
        );
    }

    #[tokio::test]
    async fn dial_of_nonresolving_dns_seed_does_not_panic_or_block() {
        // Tolerance test (no live DNS involved): a seed hostname that will
        // never resolve must not panic or hang the caller. DNS resolution
        // is deferred into the async transport, so `dial()` queuing a
        // `/dns4/` address for a bogus host must still return promptly —
        // any resolution failure surfaces later as a swarm event, not here.
        let mut net = CommpNetwork::new(0).expect("network should build");
        let addr: Multiaddr = "/dns4/this-host-does-not-exist.invalid.commputer-test/tcp/9000"
            .parse()
            .expect("dns4 multiaddr should parse even for a bogus host");
        // Must not panic. The Result is intentionally not asserted either
        // way: what matters is that queuing the dial returns instead of
        // blocking on resolution.
        let _ = net.dial(addr);
    }

    // -- Task T: seed keepalive machinery -----------------------------------

    #[test]
    fn seed_targets_include_defaults_and_custom() {
        let custom = vec!["/ip4/10.1.2.3/tcp/9100".to_string()];
        let targets = seed_targets(&custom);

        let (_, seed_addrs) = targets
            .iter()
            .find(|(k, _)| k == "seed.commputer.xyz:9000")
            .expect("default seed key present");
        // Pinned forms are preferred: every addr carries the pinned peer id,
        // and both TCP and QUIC transports are covered.
        assert!(
            seed_addrs.iter().all(|a| embedded_peer_id(a).is_some()),
            "default seed group must keep only pinned forms, got {:?}",
            seed_addrs
        );
        assert!(seed_addrs.iter().any(|a| a.to_string().contains("/tcp/9000")));
        assert!(seed_addrs.iter().any(|a| a.to_string().contains("/quic-v1")));

        let (_, custom_addrs) = targets
            .iter()
            .find(|(k, _)| k == "10.1.2.3:9100")
            .expect("custom seed key present");
        assert!(custom_addrs.iter().any(|a| a.to_string() == "/ip4/10.1.2.3/tcp/9100"));
        assert!(custom_addrs.iter().any(|a| a.to_string() == "/ip4/10.1.2.3/udp/9100/quic-v1"));
    }

    #[test]
    fn addr_matches_seed_ignores_p2p_suffix() {
        let id = libp2p::PeerId::random();
        let bare: Multiaddr = "/dns4/seed.commputer.xyz/tcp/9000".parse().unwrap();
        let pinned: Multiaddr = format!("/dns4/seed.commputer.xyz/tcp/9000/p2p/{id}")
            .parse()
            .unwrap();
        assert!(addr_matches_seed(&bare, &pinned));
        assert!(addr_matches_seed(&pinned, &bare));
        assert!(addr_matches_seed(&pinned, &pinned));
        let other: Multiaddr = "/dns4/other.example/tcp/9000".parse().unwrap();
        assert!(!addr_matches_seed(&other, &pinned));
    }

    #[tokio::test]
    async fn note_seed_connection_learns_mapping_and_self_key() {
        let mut net = CommpNetwork::new(0).expect("network should build");
        let custom = vec!["/ip4/10.0.0.7/tcp/9100".to_string()];

        // Learned mapping: remote matches the default seed (p2p suffix ignored).
        let seed_peer = libp2p::PeerId::random();
        let remote: Multiaddr = "/dns4/seed.commputer.xyz/tcp/9000".parse().unwrap();
        net.note_seed_connection(&seed_peer, &remote, &custom);
        assert_eq!(
            net.seed_peer_ids.get("seed.commputer.xyz:9000"),
            Some(&seed_peer)
        );
        assert!(net.seed_self_keys.is_empty());

        // Self key: a seed address that answers with OUR OWN peer id is
        // recorded as self, not as a learned mapping.
        let local = net.local_peer_id;
        let custom_remote: Multiaddr = "/ip4/10.0.0.7/tcp/9100".parse().unwrap();
        net.note_seed_connection(&local, &custom_remote, &custom);
        assert!(net.seed_self_keys.contains("10.0.0.7:9100"));
        assert!(!net.seed_peer_ids.contains_key("10.0.0.7:9100"));
    }

    #[test]
    fn plan_skips_connected_seed() {
        let local = libp2p::PeerId::random();
        let seed_id = libp2p::PeerId::random();
        let key = "seed.example:9000".to_string();
        let pinned: Vec<Multiaddr> =
            vec![format!("/dns4/seed.example/tcp/9000/p2p/{seed_id}").parse().unwrap()];
        let empty_self = HashSet::new();
        let no_learned = std::collections::HashMap::new();
        let no_sched = std::collections::HashMap::new();

        // Connected via the pinned id embedded in the addr.
        let d = plan_seed_dial(
            &key, &pinned, &empty_self, &no_learned, &no_sched,
            |id| *id == seed_id, &local, 0,
        );
        assert_eq!(d, SeedDialDecision::SkipConnected);

        // Connected via a learned mapping on an un-pinned addr.
        let bare: Vec<Multiaddr> = vec!["/dns4/seed.example/tcp/9000".parse().unwrap()];
        let mut learned = std::collections::HashMap::new();
        learned.insert(key.clone(), seed_id);
        let d = plan_seed_dial(
            &key, &bare, &empty_self, &learned, &no_sched,
            |id| *id == seed_id, &local, 0,
        );
        assert_eq!(d, SeedDialDecision::SkipConnected);

        // Same learned mapping but no longer connected: dial again.
        let d = plan_seed_dial(
            &key, &bare, &empty_self, &learned, &no_sched,
            |_| false, &local, 0,
        );
        assert_eq!(d, SeedDialDecision::Dial);
    }

    #[test]
    fn plan_skips_self_seed() {
        let local = libp2p::PeerId::random();
        let key = "seed.example:9000".to_string();
        let bare: Vec<Multiaddr> = vec!["/dns4/seed.example/tcp/9000".parse().unwrap()];
        let no_learned = std::collections::HashMap::new();
        let no_sched = std::collections::HashMap::new();

        // Key previously learned to be our own address.
        let mut self_keys = HashSet::new();
        self_keys.insert(key.clone());
        let d = plan_seed_dial(
            &key, &bare, &self_keys, &no_learned, &no_sched,
            |_| false, &local, 0,
        );
        assert_eq!(d, SeedDialDecision::SkipSelf);

        // Pinned form naming our own peer id (the seed box itself).
        let pinned_self: Vec<Multiaddr> =
            vec![format!("/dns4/seed.example/tcp/9000/p2p/{local}").parse().unwrap()];
        let d = plan_seed_dial(
            &key, &pinned_self, &HashSet::new(), &no_learned, &no_sched,
            |_| false, &local, 0,
        );
        assert_eq!(d, SeedDialDecision::SkipSelf);
    }

    #[test]
    fn plan_rate_limits_per_seed_with_60s_cap() {
        let local = libp2p::PeerId::random();
        let key = "seed.example:9000".to_string();
        let bare: Vec<Multiaddr> = vec!["/dns4/seed.example/tcp/9000".parse().unwrap()];
        let empty_self = HashSet::new();
        let no_learned = std::collections::HashMap::new();

        // A seed with a deep generic backoff (300s) still gets scheduled at
        // most 60s out — the write side clamps before storing next-allowed.
        let mut backoff = crate::peer::ConnectionBackoff::new();
        for i in 0..12 {
            backoff.record_failure(&key, i * 1000);
        }
        assert_eq!(backoff.current_backoff_secs(&key), 300, "generic cap");
        assert_eq!(clamped_seed_wait_ms(backoff.current_backoff_secs(&key)), 60_000);

        let mut sched = std::collections::HashMap::new();
        sched.insert(
            key.clone(),
            10_000 + clamped_seed_wait_ms(backoff.current_backoff_secs(&key)),
        );

        // Suppressed strictly before the 60s mark...
        let d = plan_seed_dial(
            &key, &bare, &empty_self, &no_learned, &sched,
            |_| false, &local, 10_000 + 59_999,
        );
        assert_eq!(d, SeedDialDecision::SkipBackoff);

        // ...allowed at the 60s mark, even though the generic 300s backoff
        // would have held until 310_000.
        let d = plan_seed_dial(
            &key, &bare, &empty_self, &no_learned, &sched,
            |_| false, &local, 10_000 + 60_000,
        );
        assert_eq!(d, SeedDialDecision::Dial);
    }

    #[test]
    fn plan_dials_unconnected_default_seed() {
        let targets = seed_targets(&[]);
        let (key, addrs) = targets
            .iter()
            .find(|(k, _)| k == "seed.commputer.xyz:9000")
            .expect("default seed key present");
        let d = plan_seed_dial(
            key,
            addrs,
            &HashSet::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            |_| false,
            &libp2p::PeerId::random(),
            0,
        );
        assert_eq!(d, SeedDialDecision::Dial);
    }

    #[tokio::test]
    async fn ensure_seed_connections_holds_off_second_tick() {
        // First tick dials the (unconnected) default seed; the immediate
        // second tick must be suppressed by the provisional hold — an
        // unanswered dial is not re-fired every ~30s maintenance tick.
        let mut net = CommpNetwork::new(0).expect("network should build");
        let dialed = net.ensure_seed_connections(&[]);
        assert!(dialed >= 1, "expected at least the default seed dialed, got {dialed}");
        let again = net.ensure_seed_connections(&[]);
        assert_eq!(again, 0, "unanswered seed dial must be held, not re-fired");
    }

    // -- Peer-key hardening (finding [31]) ----------------------------------

    /// Make a fresh, unique temp directory for a test and return its path.
    #[cfg(unix)]
    fn unique_tmp_dir(tag: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "commp_peerkey_{tag}_{}_{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn peer_key_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_tmp_dir("perm");
        let path = dir.join("peer.key");
        write_new_peer_key(&path, b"super-secret-identity-bytes").unwrap();

        let contents = std::fs::read(&path).unwrap();
        assert_eq!(contents, b"super-secret-identity-bytes");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "key must be created owner-only, got {:o}", mode & 0o777);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// NON-VACUOUS: a pre-planted symlink at the key path must NOT be followed.
    /// The old `std::fs::write(path, ..)` followed the symlink and wrote the
    /// private key into the attacker-chosen target; `create_new` refuses it.
    #[cfg(unix)]
    #[test]
    fn peer_key_refuses_preplanted_symlink() {
        let dir = unique_tmp_dir("symlink");
        let target = dir.join("attacker_target");
        let key_path = dir.join("peer.key");
        // Dangling symlink: target does not exist yet, so `path.exists()` on the
        // symlink is false — exactly the branch the writer takes on a fresh dir.
        std::os::unix::fs::symlink(&target, &key_path).unwrap();

        let result = write_new_peer_key(&key_path, b"KEYBYTES");

        assert!(
            result.is_err(),
            "writing through a pre-planted symlink must fail, but it succeeded"
        );
        assert!(
            !target.exists(),
            "symlink was followed: the key was written to the attacker target"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
