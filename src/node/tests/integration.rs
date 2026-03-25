use commputer_network::transport::{CommpBehaviourEvent, CommpNetwork};
use commputer_network::topics;

use commputer_core::block::{Block, BlockHeader, BlockHash};
use commputer_core::identity::Address;

use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use std::time::Duration;

/// Build a minimal test block for gossip propagation.
fn test_block() -> Block {
    Block {
        header: BlockHeader {
            height: 1,
            parent_hash: BlockHash::GENESIS,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 1_700_000_000,
            producer: Address([0u8; 32]),
            epoch: 0,
            signature: vec![],
        },
        transactions: vec![],
        proof_summaries: vec![],
    }
}

/// Two nodes start, connect, and exchange a block via gossipsub.
#[tokio::test]
async fn two_nodes_gossip_block() {
    let mut node_a = CommpNetwork::new(19001).expect("node A should start");
    let mut node_b = CommpNetwork::new(19002).expect("node B should start");

    // Poll node B briefly to pick up its listen address.
    let b_addr = loop {
        match node_b.swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                // Only accept the TCP address (skip any others).
                if address.to_string().contains("/tcp/") {
                    break address;
                }
            }
            _ => {}
        }
    };

    // Dial from A -> B.
    node_a
        .dial(b_addr)
        .expect("node A should dial node B");

    // State machine for the test loop.
    let mut a_connected = false;
    let mut b_connected = false;
    let mut published = false;
    let mut received_block: Option<Block> = None;

    let block = test_block();
    let expected_hash = block.hash();

    let timeout = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(timeout);

    // After both connect, wait a few gossipsub heartbeats before publishing.
    let mut connected_at: Option<tokio::time::Instant> = None;
    // Retry publish every second if mesh isn't ready yet.
    let mut last_publish_attempt: Option<tokio::time::Instant> = None;

    loop {
        // Try to publish once mesh delay has elapsed.
        if !published && a_connected && b_connected {
            let now = tokio::time::Instant::now();
            let connected_time = connected_at.get_or_insert(now);
            let mesh_ready = now.duration_since(*connected_time) >= Duration::from_secs(3);
            let retry_ok = last_publish_attempt
                .map(|t| now.duration_since(t) >= Duration::from_secs(1))
                .unwrap_or(true);

            if mesh_ready && retry_ok {
                let data = serde_json::to_vec(&block).expect("serialize block");
                let topic = topics::block_topic();
                match node_a.swarm.behaviour_mut().gossipsub.publish(topic, data) {
                    Ok(_) => {
                        published = true;
                        eprintln!("Published block from Node A");
                    }
                    Err(e) => {
                        eprintln!("Publish failed ({e}), will retry...");
                        last_publish_attempt = Some(now);
                    }
                }
            }
        }

        // Success!
        if let Some(ref blk) = received_block {
            assert_eq!(blk.hash(), expected_hash, "received block hash should match");
            assert_eq!(blk.height(), 1, "received block height should be 1");
            eprintln!("SUCCESS: Node B received the block from Node A");
            return;
        }

        tokio::select! {
            _ = &mut timeout => {
                panic!(
                    "Test timed out. a_connected={a_connected}, b_connected={b_connected}, \
                     published={published}, received={}",
                    received_block.is_some()
                );
            }

            event = node_a.swarm.select_next_some() => {
                match event {
                    SwarmEvent::ConnectionEstablished { .. } => {
                        a_connected = true;
                    }
                    _ => {}
                }
            }

            event = node_b.swarm.select_next_some() => {
                match event {
                    SwarmEvent::ConnectionEstablished { .. } => {
                        b_connected = true;
                    }
                    SwarmEvent::Behaviour(CommpBehaviourEvent::Gossipsub(
                        libp2p::gossipsub::Event::Message { message, .. }
                    )) => {
                        if message.topic.as_str() == topics::TOPIC_BLOCKS {
                            if let Ok(blk) = serde_json::from_slice::<Block>(&message.data) {
                                received_block = Some(blk);
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Short tick to re-check publish logic and avoid blocking forever.
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

/// Simpler test: two nodes can discover each other and establish a connection.
#[tokio::test]
async fn two_nodes_connect() {
    let mut node_a = CommpNetwork::new(19003).expect("node A should start");
    let mut node_b = CommpNetwork::new(19004).expect("node B should start");

    // Get node B's listen address.
    let b_addr = loop {
        match node_b.swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                if address.to_string().contains("/tcp/") {
                    break address;
                }
            }
            _ => {}
        }
    };

    node_a.dial(b_addr).expect("dial should succeed");

    let mut a_connected = false;
    let mut b_connected = false;

    let timeout = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => {
                panic!(
                    "Connection timed out. a_connected={a_connected}, b_connected={b_connected}"
                );
            }

            event = node_a.swarm.select_next_some() => {
                if let SwarmEvent::ConnectionEstablished { peer_id, .. } = event {
                    eprintln!("Node A connected to {peer_id}");
                    a_connected = true;
                }
            }

            event = node_b.swarm.select_next_some() => {
                if let SwarmEvent::ConnectionEstablished { peer_id, .. } = event {
                    eprintln!("Node B connected to {peer_id}");
                    b_connected = true;
                }
            }
        }

        if a_connected && b_connected {
            eprintln!("SUCCESS: Both nodes connected to each other");
            return;
        }
    }
}
