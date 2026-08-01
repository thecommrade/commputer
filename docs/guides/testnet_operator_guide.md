# Testnet Operator Guide

This guide covers everything you need to run a Commputer node on the testnet: hardware requirements, installation, configuration, running the node, and monitoring.

---

## Hardware Requirements

### Minimum (Solo Desktop — Recommended)

Commputer is intentionally designed to reward solo desktops. The anti-scale multiplier gives full rewards to a single node.

| Component | Minimum | Recommended |
|-----------|---------|-------------|
| CPU | 2 cores, x86-64 | 4 cores |
| RAM | 8 GB | 16 GB |
| Disk | 20 GB SSD | 100 GB SSD |
| Network | 10 Mbps symmetric | 50 Mbps symmetric |
| OS | Linux (64-bit) | Ubuntu 22.04 LTS or Arch Linux |

### What "Solo Desktop" Means

The anti-scale multiplier penalizes operators who run multiple nodes from the same wallet:
- 1 node: full reward (1.0×)
- 2 nodes: 0.5× per node
- 3 nodes: 0.25× per node
- N nodes: 0.5^(N-1)× per node

Running from a home desktop on residential internet is the optimal strategy.

### Why Not a VPS?

VPS providers put hundreds of nodes in the same /16 subnet. The eclipse attack detector will flag this, and the anti-scale system detects correlated infrastructure. The system is designed to make home nodes the economically optimal choice.

---

## Installation

### Prerequisites

- **Rust** (for building from source): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **NTP enabled** (see `ntp_requirement_doc.md`)
- **Port 9000 open** (see `port_configuration_guide.md`)

### Build from Source

```bash
# Clone the repository
git clone https://github.com/thecommrade/commputer.git
cd commputer/src          # the cargo workspace root is src/, not the repo root

# Build in release mode
cargo build --release -p commputer

# The binary is at:
./target/release/commputer
```

### Verify the Build

```bash
./target/release/commputer --version
# Expected: commputer 0.x.y (commit: abcdef0)
```

---

## Configuration

### Create a Data Directory

```bash
mkdir -p ~/.commputer
```

### Generate or Import a Wallet

```bash
# Generate a new wallet
commputer wallet create

# This outputs:
#   Address: comme:deadbeef...
#   Mnemonic: word1 word2 word3 ... word24
#   IMPORTANT: Write down your mnemonic. It cannot be recovered.

# Or import an existing wallet
commputer wallet import --mnemonic "word1 word2 ..."
```

### Create commputer.toml

```toml
# ~/.commputer/commputer.toml

[node]
name = "MyNode"           # Friendly name (shown in peer lists, not authenticated)
data_dir = "~/.commputer/data"

[network]
port = 9000
seeds = [
    "seed1.testnet.commputer.xyz:9000",
    "seed2.testnet.commputer.xyz:9000",
    "seed3.testnet.commputer.xyz:9000",
]

[rpc]
port = 9944
bind = "127.0.0.1"        # Only accessible locally

[validator]
enabled = true            # Set to false if you only want to observe the network
```

### Pre-flight Check

Before starting for the first time, run the config validator:

```bash
commputer check-config
```

Expected output:
```
[OK] P2P port 9000: Port 9000 is available
[OK] RPC port 9944: RPC port 9944 is available
[OK] Port conflict: P2P and RPC ports are different
[OK] Disk space: Data directory exists: /home/you/.commputer/data
[OK] Seed node1.testnet.commputer.xyz:9000: reachable
[OK] Seed node2.testnet.commputer.xyz:9000: reachable
[OK] NTP sync: Time synchronization service appears to be running
```

If you see any `[ERROR]` lines, fix them before proceeding. `[WARN]` lines are non-fatal.

---

## Running the Node

### Start the Node

```bash
commputer node
```

### Useful Flags

```bash
# Enable verbose logging
commputer node --log-level debug

# Use compact one-line log format
commputer node --log-format compact

# Run without registering as a validator (observer mode)
commputer node --no-validate

# Specify a custom config file
commputer node --config /path/to/commputer.toml
```

### Running as a Background Service (systemd)

Create `/etc/systemd/system/commputer.service`:

```ini
[Unit]
Description=Commputer Node
After=network-online.target
Wants=network-online.target

[Service]
User=your_username
WorkingDirectory=/home/your_username
ExecStart=/home/your_username/.cargo/bin/commputer node
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

Enable and start:
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now commputer
sudo journalctl -u commputer -f   # follow logs
```

---

## Monitoring

### Node Status

```bash
commputer status
```

Example output:
```
Node:       MyNode
Address:    comme:deadbeef01234567...
State:      Active
Height:     1234
Peers:      8 connected
Validator:  active (registered at block 100)
Balance:    50.000000 COMME
Grace:      365.0 days
```

### Peer List

```bash
commputer peers
```

### Peer Topology

```bash
commputer peers --topology
```

### Chain Health

```bash
commputer health
```

Checks:
- Block finality lag
- Vote participation rate
- Recent reorg history
- Network partition risk
- Eclipse attack risk

### Logs

With systemd:
```bash
journalctl -u commputer -f
```

Log line format (compact mode):
```
[1234] BLOCK: hash=abcdef012345 producer=comme:dead txs=5 via=snowball t=2.1s
```

---

## Registering as a Validator

Once your node is synced and running, register to earn block rewards:

```bash
commputer validator register
```

This submits a `ValidatorRegister` transaction to the chain. Registration costs a small fee.

You must maintain:
- **Grace balance > 0** — The grace period acts as a stake. When you are offline, grace drains. When online, it refills 2:1. If grace reaches 0, you are removed from the validator set.
- **Consistent uptime** — The CRS (Contribution Rate Score) includes an uptime component.

To check your validator status:
```bash
commputer validator status
```

To deregister:
```bash
commputer validator deregister
```

---

## Troubleshooting

### Node stuck at Syncing state

Check that:
1. Port 9000 is open and reachable from the internet
2. Seed nodes are listed in your config
3. You have at least 3 peers (`commputer peers`)

### "Too few peers" warning

Add more seed nodes to `commputer.toml` or wait for the Kademlia DHT to find more peers. Initial peer discovery can take a few minutes.

### High finality lag

This may indicate:
1. Your clock is drifting (check NTP sync)
2. The network has low validator participation
3. You are on a network partition (check `commputer health`)

### Grace balance draining fast

Grace drains when you are offline. If your node crashes and restarts slowly, grace drains faster than it refills. Ensure your node has reliable uptime (use systemd with `Restart=on-failure`).

### "Eclipse risk" alerts

If `commputer health` shows eclipse risk, your peers are too concentrated in one subnet. This can happen if you are connected only to seeds in the same data center. The node will attempt to diversify connections automatically.

---

## Getting Help

- GitHub Issues: https://github.com/thecommrade/commputer/issues
- Website: https://commputer.xyz
- Operator guide: https://commputer.xyz/operator.html
