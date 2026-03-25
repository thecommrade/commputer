use libp2p::{
    gossipsub, identify, kad, noise, tcp, yamux, quic,
    relay, dcutr, upnp,
    Multiaddr, PeerId as Libp2pPeerId, Swarm, SwarmBuilder,
};
use std::time::Duration;
use tracing::{info, warn, debug};

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
}

impl CommpNetwork {
    /// Create a new CommpNetwork listening on the given port via TCP and QUIC.
    pub fn new(listen_port: u16) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_with_keypair_path(listen_port, None)
    }

    /// Item 3: Create a CommpNetwork with a persistent keypair.
    /// If `keypair_path` is provided, loads the keypair from disk or generates
    /// and saves a new one. This ensures the peer ID survives restarts.
    pub fn new_with_keypair_path(listen_port: u16, keypair_path: Option<&std::path::Path>) -> Result<Self, Box<dyn std::error::Error>> {
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
                std::fs::write(path, &bytes)?;
                // Set restrictive permissions on the key file (Unix only).
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                }
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
                yamux::Config::default,
            )?
            .with_quic()
            .with_relay_client(noise::Config::new, yamux::Config::default)?
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

                // Identify with our protocol version
                let identify = identify::Behaviour::new(
                    identify::Config::new(
                        "/commputer/0.1.0".to_string(),
                        key.public(),
                    ),
                );

                // DCUtR for direct connection upgrades after relay
                let dcutr = dcutr::Behaviour::new(peer_id);

                // UPnP for automatic port mapping
                let upnp = upnp::tokio::Behaviour::default();

                Ok(CommpBehaviour {
                    gossipsub,
                    kademlia,
                    identify,
                    relay_client,
                    dcutr,
                    upnp,
                })
            })?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(Duration::from_secs(60))
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

/// Founder-operated seed nodes. Replace with real addresses at launch.
pub const SEED_NODES: &[&str] = &[
    // Format: /ip4/<IP>/tcp/<PORT>/p2p/<PEER_ID>
    // Or QUIC: /ip4/<IP>/udp/<PORT>/quic-v1/p2p/<PEER_ID>
];

impl CommpNetwork {
    /// Dial all built-in seed nodes. Returns the number successfully dialed.
    pub fn connect_to_seeds(&mut self) -> usize {
        let mut connected = 0;
        for addr_str in SEED_NODES {
            if let Ok(addr) = addr_str.parse::<Multiaddr>() {
                if self.dial(addr).is_ok() {
                    connected += 1;
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

                    // If the given address is TCP, also try QUIC variant
                    if addr_str.contains("/tcp/") {
                        let quic_addr = addr_str
                            .replace("/tcp/", "/udp/")
                            .replace("/p2p/", "/quic-v1/p2p/");
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
                        if let Ok(multiaddr) = tcp_str.parse::<Multiaddr>() {
                            if self.dial(multiaddr).is_ok() {
                                info!("Dialed DNS seed {} -> {} (TCP)", domain, tcp_str);
                                connected += 1;
                            }
                        }
                        // Also try QUIC
                        if let Ok(multiaddr) = quic_str.parse::<Multiaddr>() {
                            if self.dial(multiaddr).is_ok() {
                                info!("Dialed DNS seed {} -> {} (QUIC)", domain, quic_str);
                                connected += 1;
                            }
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
}
