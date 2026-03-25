use libp2p::{
    gossipsub, identify, kad, noise, tcp, yamux,
    Multiaddr, PeerId as Libp2pPeerId, Swarm, SwarmBuilder,
};
use std::time::Duration;
use tracing::{info, warn, debug};

/// The Commputer P2P network built on libp2p with gossipsub, Kademlia, and identify.
pub struct CommpNetwork {
    pub swarm: Swarm<CommpBehaviour>,
    pub local_peer_id: Libp2pPeerId,
}

/// Combined libp2p behaviour: gossipsub for broadcast, Kademlia for DHT, identify for handshake.
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct CommpBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
}

impl CommpNetwork {
    /// Create a new CommpNetwork listening on the given port.
    pub fn new(listen_port: u16) -> Result<Self, Box<dyn std::error::Error>> {
        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_behaviour(|key| {
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

                Ok(CommpBehaviour {
                    gossipsub,
                    kademlia,
                    identify,
                })
            })?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(Duration::from_secs(60))
            })
            .build();

        let local_peer_id = *swarm.local_peer_id();

        let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{listen_port}").parse()?;
        swarm.listen_on(listen_addr)?;

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

    /// Feature 178: Connect to custom seed nodes from CLI --seeds arg.
    /// Returns the number of seeds successfully dialed.
    pub fn connect_to_custom_seeds(&mut self, seeds: &[String]) -> usize {
        let mut connected = 0;
        for addr_str in seeds {
            match addr_str.parse::<Multiaddr>() {
                Ok(addr) => {
                    match self.dial(addr) {
                        Ok(()) => {
                            info!("Dialed custom seed: {}", addr_str);
                            connected += 1;
                        }
                        Err(e) => {
                            warn!("Failed to dial custom seed {}: {}", addr_str, e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Invalid custom seed multiaddr '{}': {}", addr_str, e);
                }
            }
        }
        connected
    }

    /// Feature 179: Resolve DNS seed domains (A records) and construct multiaddrs.
    /// Each domain is resolved to IPs, and we dial /ip4/<ip>/tcp/9000 for each.
    pub fn resolve_dns_seeds(&mut self, domains: &[String], port: u16) -> usize {
        let mut connected = 0;
        for domain in domains {
            match std::net::ToSocketAddrs::to_socket_addrs(&(domain.as_str(), port)) {
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
                        if let Ok(multiaddr) = multiaddr_str.parse::<Multiaddr>() {
                            match self.dial(multiaddr) {
                                Ok(()) => {
                                    info!("Dialed DNS seed {} -> {}", domain, multiaddr_str);
                                    connected += 1;
                                }
                                Err(e) => {
                                    warn!("Failed to dial DNS seed {} ({}): {}", domain, multiaddr_str, e);
                                }
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

    /// Feature 166: Trigger Kademlia bootstrap for peer discovery.
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

    /// Feature 176: Log encryption status on startup.
    pub fn log_encryption_status(&self) {
        info!("P2P encryption: Noise protocol active");
        info!("P2P transport: TCP + Yamux multiplexing");
        info!("P2P protocol: /commputer/0.1.0");
    }
}
