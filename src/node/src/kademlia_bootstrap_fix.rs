// kademlia_bootstrap_fix.rs — Fix "No known peers" kademlia bootstrap failure
//
// WHAT IT DOES:
//   Fixes the "Failed to trigger bootstrap: No known peers" error that occurs
//   on every boot. Root cause: kademlia bootstrap() is called before any peer
//   has been added to its routing table.
//
// ROOT CAUSE:
//   The swarm is built with kademlia configured, but bootstrap() is called
//   immediately during setup before any connection is established. Kademlia
//   requires at least one peer in its routing table before it can bootstrap.
//
// FIX:
//   1. Don't call bootstrap() during swarm construction.
//   2. When the FIRST ConnectionEstablished event fires, add that peer to
//      kademlia's routing table explicitly.
//   3. Then trigger bootstrap.
//   4. Also add peers to kademlia when we learn their addresses via identify.
//
// WHERE IT SHOULD GO:
//   Called from handle_swarm_event() in src/node/src/event_loop.rs.
//
// WIRING INSTRUCTIONS:
//   1. Add `pub kademlia_bootstrapped: bool,` to EventLoop struct.
//   2. Initialize: `kademlia_bootstrapped: false,` in EventLoop::new().
//   3. In handle_swarm_event, find ConnectionEstablished handler:
//
//      FIND:
//      ```
//      SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
//          // ... existing code ...
//      }
//      ```
//
//      ADD at the end of the ConnectionEstablished handler:
//      ```
//      // Trigger kademlia bootstrap on first peer connection
//      kademlia_bootstrap_fix::trigger_bootstrap_on_first_peer(
//          &mut self.network.swarm,
//          peer_id,
//          &endpoint,
//          &mut self.kademlia_bootstrapped,
//      );
//      ```
//
//   4. In the identify response handler (when we learn peer addresses):
//      FIND: `CommpBehaviourEvent::Identify(identify::Event::Received { peer_id, info })`
//      ADD:
//      ```
//      // Add peer to kademlia routing table with their listen addresses
//      kademlia_bootstrap_fix::add_peer_to_kademlia(
//          &mut self.network.swarm,
//          peer_id,
//          &info.listen_addrs,
//      );
//      ```
//
//   5. In transport.rs, REMOVE or GUARD the `swarm.behaviour_mut().kademlia.bootstrap()`
//      call that runs at startup before any peers are connected.
//      FIND: `.bootstrap()` call during swarm setup
//      REPLACE with: a logged note that bootstrap will happen on first peer connect.
//
// EXISTING FILES THAT NEED CHANGES:
//   - src/node/src/event_loop.rs (ConnectionEstablished + identify handlers)
//   - src/network/src/transport.rs (remove premature bootstrap call)

use libp2p::{PeerId, Multiaddr};
use libp2p::core::ConnectedPoint;
use tracing::{info, debug};

/// Attempt to trigger kademlia bootstrap once a peer is connected.
///
/// This is called from the ConnectionEstablished handler.
/// Only triggers bootstrap on the FIRST successful peer connection.
///
/// # Arguments
/// * `swarm` — the libp2p swarm (needs access to kademlia behaviour)
/// * `peer_id` — the peer that just connected
/// * `endpoint` — the connection endpoint (has the peer's address)
/// * `bootstrapped` — flag to track whether bootstrap has been triggered
pub fn trigger_bootstrap_on_first_peer(
    peer_id: PeerId,
    peer_addr: Option<Multiaddr>,
    bootstrapped: &mut bool,
) -> Option<BootstrapAction> {
    if *bootstrapped {
        return None; // Already bootstrapped
    }

    info!(
        "[kademlia] First peer connected ({}), triggering bootstrap",
        peer_id
    );

    *bootstrapped = true;

    Some(BootstrapAction {
        add_to_routing_table: peer_id,
        addr: peer_addr,
        trigger_bootstrap: true,
    })
}

/// Action to perform on the kademlia behaviour after receiving a BootstrapAction.
/// The caller must apply this to the swarm.
#[derive(Debug)]
pub struct BootstrapAction {
    /// Peer to add to the routing table.
    pub add_to_routing_table: PeerId,
    /// Known address for that peer (from the connection endpoint).
    pub addr: Option<Multiaddr>,
    /// Whether to call kademlia.bootstrap() after adding the peer.
    pub trigger_bootstrap: bool,
}

/// Extract the address from a ConnectedPoint for adding to kademlia.
pub fn extract_addr_from_endpoint(endpoint: &ConnectedPoint) -> Option<Multiaddr> {
    match endpoint {
        ConnectedPoint::Dialer { address, .. } => Some(address.clone()),
        ConnectedPoint::Listener { send_back_addr, .. } => Some(send_back_addr.clone()),
    }
}

/// Add a peer's known listen addresses to kademlia.
/// Called when we receive an identify response with the peer's full listen addr list.
///
/// This is the more accurate version of adding to kademlia — uses the peer's
/// actual listen addresses (not the ephemeral source address).
///
/// Returns the list of addresses added.
pub fn prepare_kademlia_add(
    peer_id: PeerId,
    listen_addrs: &[Multiaddr],
) -> Vec<(PeerId, Multiaddr)> {
    let mut to_add = Vec::new();
    for addr in listen_addrs {
        // Only add non-local addresses to kademlia
        let addr_str = addr.to_string();
        if addr_str.contains("/127.0.0.1/")
            || addr_str.contains("/::1/")
            || addr_str.contains("/0.0.0.0/")
        {
            debug!("[kademlia] skipping local address: {}", addr);
            continue;
        }
        to_add.push((peer_id, addr.clone()));
        debug!("[kademlia] will add {} at {} to routing table", peer_id, addr);
    }
    to_add
}

/// Complete wiring code for the ConnectionEstablished handler.
///
/// Paste this at the END of the ConnectionEstablished arm in handle_swarm_event():
pub mod insertion_points {
    pub const IN_CONNECTION_ESTABLISHED: &str = r#"
// === INSERTED: kademlia bootstrap on first peer ===
{
    let peer_addr = kademlia_bootstrap_fix::extract_addr_from_endpoint(&endpoint);
    if let Some(action) = kademlia_bootstrap_fix::trigger_bootstrap_on_first_peer(
        peer_id, peer_addr, &mut self.kademlia_bootstrapped
    ) {
        // Add peer to kademlia routing table
        if let Some(addr) = action.addr {
            self.network.swarm.behaviour_mut().kademlia
                .add_address(&action.add_to_routing_table, addr);
        }
        // Now trigger bootstrap
        if action.trigger_bootstrap {
            match self.network.swarm.behaviour_mut().kademlia.bootstrap() {
                Ok(query_id) => info!("[kademlia] bootstrap started: {:?}", query_id),
                Err(e) => warn!("[kademlia] bootstrap failed: {:?}", e),
            }
        }
    }
}
// === END INSERTED ===
"#;

    /// In the identify::Event::Received handler:
    pub const IN_IDENTIFY_RECEIVED: &str = r#"
// === INSERTED: add peer's listen addrs to kademlia ===
{
    let to_add = kademlia_bootstrap_fix::prepare_kademlia_add(peer_id, &info.listen_addrs);
    for (pid, addr) in to_add {
        self.network.swarm.behaviour_mut().kademlia.add_address(&pid, addr);
    }
}
// === END INSERTED ===
"#;

    /// In transport.rs, find and REMOVE or comment out this line:
    pub const REMOVE_FROM_TRANSPORT: &str = r#"
// REMOVE THIS LINE from transport.rs setup code:
//   behaviour.kademlia.bootstrap();
// or:
//   swarm.behaviour_mut().kademlia.bootstrap().ok();
//
// REASON: bootstrap() fails silently when routing table is empty.
// Bootstrap is now triggered from the event loop on first peer connection.
// See kademlia_bootstrap_fix::trigger_bootstrap_on_first_peer().
"#;
}
