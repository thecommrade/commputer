//! Item 101: Peer discovery stress test.
//!
//! Simulates 5 nodes discovering each other via a simulated Kademlia-like
//! peer exchange protocol. Since we cannot spin up real libp2p nodes in a
//! unit test without network access, we simulate the discovery process using
//! the PeerStore and gossip-based peer exchange.

use std::collections::HashMap;
use commputer_network::peer::{PeerId, PeerInfo, PeerStore};

fn make_node_id(n: u8) -> PeerId {
    let mut id = [0u8; 32];
    id[0] = n;
    PeerId(id)
}

fn make_peer_info(n: u8) -> PeerInfo {
    PeerInfo {
        id: make_node_id(n),
        address: format!("10.0.0.{}", n),
        port: 9000 + n as u16,
        last_seen_ms: 1000,
        avg_rtt_ms: None,
        connected: true,
        bandwidth_score: 0,
        addresses: vec![],
    }
}

/// Simulate 5 nodes discovering each other through peer exchange.
/// Each node starts knowing only 1-2 neighbors, then through rounds of
/// peer exchange, all nodes discover all other nodes.
#[test]
fn five_nodes_discover_each_other() {
    // Create 5 node peer stores.
    let mut nodes: Vec<PeerStore> = (0..5).map(|_| PeerStore::new()).collect();

    // Initial topology: a chain 0-1-2-3-4
    // Node 0 knows Node 1
    nodes[0].add(make_peer_info(1));
    // Node 1 knows Node 0 and Node 2
    nodes[1].add(make_peer_info(0));
    nodes[1].add(make_peer_info(2));
    // Node 2 knows Node 1 and Node 3
    nodes[2].add(make_peer_info(1));
    nodes[2].add(make_peer_info(3));
    // Node 3 knows Node 2 and Node 4
    nodes[3].add(make_peer_info(2));
    nodes[3].add(make_peer_info(4));
    // Node 4 knows Node 3
    nodes[4].add(make_peer_info(3));

    // Simulate peer exchange rounds.
    // In each round, every node shares its known peers with all its known peers.
    // This is similar to how Kademlia gossip spreads peer information.
    for _round in 0..10 {
        // Collect all peers each node knows (snapshot before round).
        let snapshots: Vec<Vec<PeerInfo>> = nodes.iter()
            .map(|store| store.all_peers().into_iter().cloned().collect())
            .collect();

        // Each node shares its snapshot with all its known peers.
        for (node_idx, snapshot) in snapshots.iter().enumerate() {
            let known_ids: Vec<PeerId> = snapshot.iter().map(|p| p.id).collect();
            for peer_id in &known_ids {
                // Find which node index this peer corresponds to.
                let peer_node_idx = peer_id.0[0] as usize;
                if peer_node_idx < 5 {
                    // Share all of our known peers with this peer.
                    for shared_peer in snapshot {
                        if shared_peer.id != make_node_id(peer_node_idx as u8)
                            && shared_peer.id != make_node_id(node_idx as u8)
                        {
                            nodes[peer_node_idx].add(shared_peer.clone());
                        }
                    }
                    // Also add the sharing node itself.
                    nodes[peer_node_idx].add(make_peer_info(node_idx as u8));
                }
            }
        }
    }

    // Verify all nodes know all other nodes.
    for (i, node) in nodes.iter().enumerate() {
        for j in 0..5u8 {
            if j != i as u8 {
                assert!(
                    node.get(&make_node_id(j)).is_some(),
                    "Node {} should know Node {} after peer exchange rounds",
                    i, j,
                );
            }
        }
        // Each node should know exactly 4 other nodes.
        assert_eq!(
            node.len(), 4,
            "Node {} should know exactly 4 peers, but knows {}",
            i, node.len()
        );
    }
}

/// Test that peer exchange converges even with sparse initial connectivity.
#[test]
fn peer_exchange_convergence_sparse() {
    let mut nodes: Vec<PeerStore> = (0..5).map(|_| PeerStore::new()).collect();

    // Very sparse: Node 0 knows Node 2, Node 2 knows Node 4.
    nodes[0].add(make_peer_info(2));
    nodes[2].add(make_peer_info(0));
    nodes[2].add(make_peer_info(4));
    nodes[4].add(make_peer_info(2));
    // Nodes 1 and 3 know Node 2 (hub).
    nodes[1].add(make_peer_info(2));
    nodes[3].add(make_peer_info(2));
    nodes[2].add(make_peer_info(1));
    nodes[2].add(make_peer_info(3));

    // Run peer exchange rounds.
    for _round in 0..10 {
        let snapshots: Vec<Vec<PeerInfo>> = nodes.iter()
            .map(|store| store.all_peers().into_iter().cloned().collect())
            .collect();

        for (node_idx, snapshot) in snapshots.iter().enumerate() {
            let known_ids: Vec<PeerId> = snapshot.iter().map(|p| p.id).collect();
            for peer_id in &known_ids {
                let peer_node_idx = peer_id.0[0] as usize;
                if peer_node_idx < 5 {
                    for shared_peer in snapshot {
                        if shared_peer.id != make_node_id(peer_node_idx as u8)
                            && shared_peer.id != make_node_id(node_idx as u8)
                        {
                            nodes[peer_node_idx].add(shared_peer.clone());
                        }
                    }
                    nodes[peer_node_idx].add(make_peer_info(node_idx as u8));
                }
            }
        }
    }

    // All nodes should eventually discover all others.
    for (i, node) in nodes.iter().enumerate() {
        assert_eq!(
            node.len(), 4,
            "Node {} should know 4 peers after convergence, knows {}",
            i, node.len()
        );
    }
}

/// Test geographic diversity scoring during discovery.
#[test]
fn discovery_with_geo_diversity() {
    use commputer_network::peer::GeoScorer;

    let mut store = PeerStore::new();

    // Add peers from diverse subnets.
    let mut p1 = make_peer_info(1);
    p1.address = "10.0.1.1".to_string();
    store.add(p1);

    let mut p2 = make_peer_info(2);
    p2.address = "10.1.1.1".to_string();
    store.add(p2);

    let mut p3 = make_peer_info(3);
    p3.address = "172.16.1.1".to_string();
    store.add(p3);

    let score = GeoScorer::score(&store);
    assert!(score > 0.9, "diverse subnets should score high: {}", score);

    // Adding a 4th peer from existing 10.0 subnet (count would be 2, above avg=1)
    // first make one subnet overrepresented
    let mut p4 = make_peer_info(4);
    p4.address = "10.0.2.2".to_string();
    store.add(p4);
    // Now 10.0 has count=2, others have 1 each. avg = 4/3 = 1
    // Another 10.0 peer (count=2, > avg=1) does not improve diversity.
    assert!(!GeoScorer::improves_diversity("10.0.3.3", &store));
    // A peer from a completely new /16 does improve diversity.
    assert!(GeoScorer::improves_diversity("192.168.1.1", &store));
}
