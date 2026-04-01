// peer_exchange_fix.rs — Fix peer exchange to propagate all known peer addresses
//
// WHAT IT DOES:
//   Fixes the peer exchange handler so it broadcasts addresses of ALL connected
//   peers (not just our own listen addresses). This is why the laptop only sees
//   1 peer: each node announces itself, but doesn't tell others who ELSE it knows.
//
// ROOT CAUSE:
//   The current handle_peer_exchange_tick() (in event_loop.rs) likely broadcasts
//   only our own external addresses on topic `commputer/peers/0.1`. Each node
//   announces itself, but doesn't propagate the addresses of other peers it knows.
//   Result: 3 nodes each only learn about the seed, not about each other.
//
// WHERE IT SHOULD GO:
//   Replaces handle_peer_exchange_tick() in src/node/src/event_loop.rs.
//
// WIRING INSTRUCTIONS:
//   1. FIND handle_peer_exchange_tick() in event_loop.rs.
//      Search for: `fn handle_peer_exchange_tick` or `peer_exchange_interval`
//
//   2. REPLACE the entire function body with the logic in
//      `build_peer_exchange_message()` + `broadcast_peer_exchange()` below.
//
//   3. The fix: include both our listen addresses AND addresses of connected peers.
//
// EXISTING FILE THAT NEEDS CHANGES: src/node/src/event_loop.rs

use std::collections::HashMap;
use libp2p::PeerId;
use serde::{Serialize, Deserialize};
use tracing::{info, debug};

/// Maximum number of peer addresses to include in a single exchange message.
/// Keeps gossip messages small and predictable.
pub const MAX_PEERS_PER_EXCHANGE: usize = 20;

/// A peer exchange message — includes our address and addresses of known peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerExchangeMessage {
    /// The peer IDs and their known addresses.
    /// Key: peer_id as string, Value: list of multiaddr strings
    pub peers: HashMap<String, Vec<String>>,
    /// Sender's own listen addresses.
    pub our_addresses: Vec<String>,
}

/// Build a peer exchange message that includes both our addresses AND
/// the addresses of connected peers.
///
/// This is the fix: the current implementation only broadcasts `our_addresses`.
/// The fix adds `connected_peer_addresses` so that peers can discover each other
/// transitively.
///
/// # Arguments
/// * `our_listen_addrs` — our own listen addresses (TCP + QUIC)
/// * `connected_peers` — peer_ips map from EventLoop
/// * `peer_observed_addrs` — addresses we've observed for each peer
///   (from identify protocol responses or explicit addr announcements)
pub fn build_peer_exchange_message(
    our_listen_addrs: &[String],
    connected_peers: &HashMap<PeerId, String>,
    peer_observed_addrs: &HashMap<PeerId, Vec<String>>,
) -> PeerExchangeMessage {
    let mut peers = HashMap::new();

    // Add ourselves
    let our_peer_entry = format!("us");
    peers.insert(our_peer_entry, our_listen_addrs.to_vec());

    // Add connected peers — this is the KEY FIX
    for (peer_id, ip) in connected_peers.iter().take(MAX_PEERS_PER_EXCHANGE - 1) {
        let peer_key = peer_id.to_string();

        // Use known addresses if available, otherwise construct from IP
        let addrs = if let Some(known) = peer_observed_addrs.get(peer_id) {
            known.clone()
        } else {
            // Fallback: construct a likely address from the observed IP
            // This is approximate — prefer identify protocol data when available
            vec![
                format!("/ip4/{}/tcp/30303", ip),   // default TCP port
                format!("/ip4/{}/udp/30303/quic-v1", ip), // default QUIC port
            ]
        };

        if !addrs.is_empty() {
            peers.insert(peer_key, addrs);
        }
    }

    debug!(
        "[peer_exchange] building message: {} peer entries (including self)",
        peers.len()
    );

    PeerExchangeMessage {
        peers,
        our_addresses: our_listen_addrs.to_vec(),
    }
}

/// Serialize the exchange message for gossipsub broadcast.
pub fn serialize_exchange_message(msg: &PeerExchangeMessage) -> Option<Vec<u8>> {
    serde_json::to_vec(msg).ok()
}

/// Parse a received peer exchange message.
pub fn parse_exchange_message(data: &[u8]) -> Option<PeerExchangeMessage> {
    serde_json::from_slice(data).ok()
}

/// Process a received peer exchange message and extract new peer addresses to try.
///
/// Returns a list of (multiaddr_string, optional_peer_id) to dial.
/// The caller should attempt to connect to each address.
///
/// # Arguments
/// * `msg` — the received exchange message
/// * `already_connected` — set of already-connected peer IDs (don't reconnect)
/// * `banned` — set of banned peers (skip these)
pub fn extract_new_peers(
    msg: &PeerExchangeMessage,
    already_connected: &HashMap<PeerId, String>,
    banned: &std::collections::HashSet<PeerId>,
) -> Vec<String> {
    let mut new_addrs = Vec::new();

    for (peer_str, addrs) in &msg.peers {
        // Skip "us" (our own entry)
        if peer_str == "us" {
            continue;
        }

        // Parse the peer ID string
        let peer_id = match peer_str.parse::<PeerId>() {
            Ok(id) => id,
            Err(_) => continue,
        };

        // Skip if already connected
        if already_connected.contains_key(&peer_id) {
            continue;
        }

        // Skip if banned
        if banned.contains(&peer_id) {
            continue;
        }

        // Add their addresses
        for addr in addrs.iter().take(2) {
            new_addrs.push(addr.clone());
        }
    }

    info!("[peer_exchange] extracted {} new peer addresses to try", new_addrs.len());
    new_addrs
}

/// Complete replacement for handle_peer_exchange_tick().
///
/// This is the exact method body to paste into the EventLoop impl block,
/// replacing the existing handle_peer_exchange_tick().
///
/// ```rust
/// // In event_loop.rs, inside `impl EventLoop`:
/// fn handle_peer_exchange_tick(&mut self) {
///     use commputer_network::topics;
///
///     // Build exchange message with our addresses + connected peer addresses
///     let our_addrs: Vec<String> = self.network.swarm.listeners()
///         .map(|a| a.to_string())
///         .collect();
///
///     // Gather observed addresses from identify protocol
///     // (stored when we receive identify::Event::Received)
///     let peer_observed_addrs: HashMap<PeerId, Vec<String>> = self.peer_ips.iter()
///         .map(|(&p, ip)| {
///             // Use the IP we know; TCP and QUIC ports
///             let addrs = vec![
///                 format!("/ip4/{}/tcp/30303", ip),
///                 format!("/ip4/{}/udp/30303/quic-v1", ip),
///             ];
///             (p, addrs)
///         })
///         .collect();
///
///     let msg = peer_exchange_fix::build_peer_exchange_message(
///         &our_addrs,
///         &self.peer_ips,
///         &peer_observed_addrs,
///     );
///
///     if let Some(data) = peer_exchange_fix::serialize_exchange_message(&msg) {
///         if let Err(e) = self.network.swarm.behaviour_mut().gossipsub
///             .publish(topics::peers_topic(), data) {
///             debug!("Failed to publish peer exchange: {}", e);
///         }
///     }
/// }
/// ```
pub struct PeerExchangeFix;
