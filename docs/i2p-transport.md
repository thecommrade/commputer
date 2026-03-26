# I2P Transport Support -- Design Document

## Overview

This document describes how Commputer nodes can optionally communicate
via the I2P (Invisible Internet Project) network for enhanced privacy.

## Goals

1. Allow nodes to create an I2P tunnel and accept connections via a
   `.b32.i2p` address.
2. Allow nodes to dial other I2P-enabled peers.
3. Provide an alternative to Tor for users who prefer I2P's garlic
   routing model.
4. Make I2P optional and interoperable with clearnet nodes.

## Architecture

### I2P vs Tor

| Feature          | Tor                       | I2P                        |
|------------------|---------------------------|----------------------------|
| Routing          | Onion (circuit-based)     | Garlic (packet-based)      |
| Latency          | 200-500ms per hop         | 300-800ms per hop          |
| Inbound support  | Hidden services           | Native (every node routes) |
| Maturity         | Very mature               | Mature, smaller network    |
| Best for         | Client browsing           | Peer-to-peer services      |

I2P is well-suited for always-on services like blockchain nodes because
every I2P node is both a client and a router.

### Transport Layer

I2P integration uses the SAMv3 (Simple Anonymous Messaging) protocol
to create tunnels. A local I2P router (e.g., `i2pd`) exposes a SAM
bridge on 127.0.0.1:7656.

The `I2pTransport` would:

- Connect to the local SAM bridge to create a session.
- Generate a destination keypair (equivalent to a .b32.i2p address).
- Accept inbound connections on the I2P destination.
- Dial outbound to other .b32.i2p destinations.

### Multiaddr Format

I2P addresses would use a custom multiaddr protocol:

    /i2p/<base32-destination>

### Configuration

New CLI flags:

    --i2p                 Enable I2P transport
    --i2p-sam <addr>      SAM bridge address (default: 127.0.0.1:7656)
    --i2p-only            Disable clearnet (I2P-exclusive mode)

### Peer Discovery

- I2P nodes publish their `.b32.i2p` destination via gossipsub on the
  TOPIC_PEER_ADDRS topic.
- Seed nodes maintain both clearnet and I2P addresses.
- The PeerExchange protocol (item 112) includes I2P addresses in the
  `addresses` field of PeerInfo (item 120).

### Performance Considerations

- I2P has higher latency than clearnet (typically 500ms-2s RTT).
- Tunnel build time can add initial connection delay (5-30 seconds).
- Bandwidth through I2P tunnels is limited. The BandwidthThrottler
  should apply appropriate limits.
- Consensus timeouts should be extended for I2P-only nodes.

### Security Considerations

- I2P provides strong sender/receiver anonymity via garlic routing.
- Destination keys are persistent -- the node's I2P address is stable.
- The SAM bridge must only be accessible on localhost.
- In `--i2p-only` mode, all clearnet interfaces should be disabled.

## Implementation Phases

1. **Phase 1**: Outbound-only I2P (connect to .b32.i2p peers via SAM).
2. **Phase 2**: Inbound I2P (accept connections on generated destination).
3. **Phase 3**: I2P-only mode with bridge nodes for discovery.

## Dependencies

- Local I2P router: `i2pd` (C++) or `java-i2p`.
- Rust SAM client library (`i2p_sam` crate).
- Custom libp2p transport adapter wrapping the SAM protocol.

## Status

Design phase. No code implemented yet.
