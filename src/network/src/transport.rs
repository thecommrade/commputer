use libp2p::{
    gossipsub, identify, kad, noise, tcp, yamux,
    Multiaddr, PeerId as Libp2pPeerId, Swarm, SwarmBuilder,
};
use std::time::Duration;

pub struct CommpNetwork {
    pub swarm: Swarm<CommpBehaviour>,
    pub local_peer_id: Libp2pPeerId,
}

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
            .build();

        let local_peer_id = *swarm.local_peer_id();

        let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{listen_port}").parse()?;
        swarm.listen_on(listen_addr)?;

        Ok(Self {
            swarm,
            local_peer_id,
        })
    }

    /// Dial a remote peer at the given multiaddr.
    pub fn dial(&mut self, addr: Multiaddr) -> Result<(), Box<dyn std::error::Error>> {
        self.swarm.dial(addr)?;
        Ok(())
    }
}
