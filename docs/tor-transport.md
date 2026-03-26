# Tor Transport Support -- Design Document

## Overview

This document describes how Commputer nodes can optionally communicate via
the Tor network to provide IP-address privacy for validators.

## Goals

1. Allow nodes to listen on a Tor hidden service (.onion address).
2. Allow nodes to dial other .onion peers.
3. Preserve all existing P2P functionality (gossipsub, Kademlia, etc.)
   over the Tor transport.
4. Make Tor optional -- nodes not using Tor still interoperate with
   Tor-enabled nodes via relay bridges.

## Architecture

### Transport Layer

Tor integration uses a SOCKS5 proxy provided by a local Tor daemon.
libp2p supports custom transports; we would add a `TorTransport` that:

- Connects outbound via the local SOCKS5 proxy (default 127.0.0.1:9050).
- Listens inbound by creating a Tor hidden service (via the Tor control
  port or `tor --HiddenServiceDir`).
- Wraps TCP streams, so Noise encryption still applies on top.

### Multiaddr Format

Tor addresses use the `/onion3/<base32-addr>:<port>` multiaddr component:

    /onion3/<56-char-base32>:9000

### Configuration

New CLI flags:

    --tor                 Enable Tor transport
    --tor-socks <addr>    SOCKS5 proxy address (default: 127.0.0.1:9050)
    --tor-control <addr>  Tor control port (default: 127.0.0.1:9051)
    --tor-only            Disable clearnet (Tor-exclusive mode)

### Peer Discovery

- Tor-only nodes cannot use Kademlia DHT over clearnet.
- Instead, they rely on seed nodes that have both clearnet and Tor
  addresses, acting as bridges.
- DNS seeds can publish TXT records with .onion addresses.

### Performance Considerations

- Tor adds 200-500ms latency per hop (3 hops typical).
- Block propagation will be slower for Tor-only nodes.
- Consensus participation (Snowball queries) may time out if latency
  exceeds thresholds -- the timeout should be configurable.
- Bandwidth is limited by the Tor network -- the BandwidthThrottler
  (item 117) should account for this.

### Security Considerations

- Tor provides IP privacy but not end-to-end encryption by itself.
  Our Noise layer provides the actual encryption.
- Guard node correlation attacks are mitigated by the Tor network.
- The node should avoid leaking its clearnet IP when in `--tor-only`
  mode (disable UPnP, don't bind on 0.0.0.0).

## Implementation Phases

1. **Phase 1**: Outbound-only Tor (dial .onion peers via SOCKS5).
2. **Phase 2**: Inbound Tor (create hidden service, accept connections).
3. **Phase 3**: Tor-only mode with bridge-based peer discovery.

## Dependencies

- Local Tor daemon (user-managed or bundled `arti` Rust Tor client).
- `libp2p-tor` crate or custom SOCKS5 transport adapter.
- `arti-client` crate for embedded Tor (optional, removes external
  Tor dependency).

## Status

Design phase. No code implemented yet.
