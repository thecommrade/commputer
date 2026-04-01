# Testnet Runbook — Commputer

*For: 3-node testnet on local network (seed + 2 validators)*
*Last updated: 2026-03-31*

---

## Pre-Flight Checklist

Before starting any node, verify:

- [ ] **NTP synced**: `timedatectl status` shows `NTP service: active` and `synchronized: yes`
- [ ] **Port available**: `ss -tlnp | grep 9002` returns empty (port not in use)
- [ ] **Disk space**: `df -h ~/.commputer` shows at least 10GB free
- [ ] **Executable**: `~/commputer-bin --version` runs without error
- [ ] **No stale process**: `pgrep -a commputer` returns nothing
- [ ] **Clock skew**: `date -u` matches other nodes within 5 seconds

---

## Start Sequence

**IMPORTANT:** Start the seed node FIRST. Other nodes need it to bootstrap discovery.

### Step 1: Start the Seed Node

```bash
# On the seed machine
./commputer --config seed.toml
```

Expected output (within 5 seconds):
```
INFO P2P encryption: Noise protocol active
INFO Event loop started at height X. Listening for peers...
INFO Listening on /ip4/0.0.0.0/tcp/30303
INFO Listening on /ip4/0.0.0.0/udp/30303/quic-v1
```

Wait until you see the seed is listening before starting other nodes.

### Step 2: Start the Other Nodes

```bash
# On each validator machine (wait 5-10 seconds between each)
./commputer --config commputer.toml
```

Expected output (within 30 seconds):
```
INFO Connected to peer 12D3KooW...
INFO Initial sync complete at height X (network at Y)
INFO node_state: Syncing -> Active
INFO Height: X | Peers: 2 | Balance: Y COMME | Epoch: Z
```

### Step 3: Verify All Nodes Are Connected

On each node, the status line (every 60 seconds) should show `Peers: 2+`.

---

## Monitoring Commands

### Health check (quick)
```bash
# Check node is running
pgrep -a commputer

# Check recent logs for errors
journalctl -u commputer -n 50 --no-pager | grep -E "ERROR|WARN|height"

# RPC status
curl -s http://localhost:8080/status | jq .height
```

### What to grep for — healthy node
```
"node_state: Syncing -> Active"   # startup OK
"Height: \d+ | Peers: [2-9]"     # height advancing, peers connected
"Block .* finalized"              # producing/receiving blocks normally
"Initial sync complete"           # sync finished
```

### What to grep for — unhealthy node
```
"WARN"                             # any warning
"All peers exhausted"              # sync stuck
"Failed to trigger bootstrap"      # kademlia issue (known bug, see known_issues_doc.md)
"Only .* peer"                     # peer discovery issue
"No network blocks after 30s"      # solo node (OK only for first node)
"node_state: Active -> Stale"      # fell behind — should self-recover
```

### Block production check
```bash
# Check if blocks are advancing (run twice, 10 seconds apart)
curl -s http://localhost:8080/status | jq .height
sleep 10
curl -s http://localhost:8080/status | jq .height
# Height should increase by ~5 (1 block/2s * 10s)
```

---

## Common Failure Modes and Recovery

### Failure: Node not advancing height
**Symptoms:** Status line shows same height for >30 seconds.

**Check:**
```bash
journalctl -u commputer -n 100 | grep -E "sync|Syncing|Active|peers"
```

**Recovery:**
1. Check peer count: if 0 peers, restart with `systemctl restart commputer`
2. If peer count >0 but stuck: check the other nodes are producing (they may be stuck too)
3. If all nodes stuck: restart all nodes in sequence (seed first)

### Failure: "Address already in use" on startup
**Recovery:**
```bash
ss -tlnp | grep 30303   # find blocking PID
kill <PID>
sleep 30                  # wait for TIME_WAIT
./commputer --config commputer.toml
```

### Failure: "Failed to trigger bootstrap: No known peers"
**This is a known bug (see Issue 2 in known_issues_doc.md).** Node will still work once it connects to the seed via the manual seeds config. No action needed.

### Failure: Node shows only 1 peer when 3 are running
**This is a known bug (Issue 1).** All nodes connect to the seed but don't discover each other via DHT. Peer exchange fix is in staging. No action needed for testnet — consensus still works with 2/3 nodes connected via the seed as relay.

### Failure: Clock skew warning
```
WARN timestamp: block rejected, clock skew X ms
```
**Recovery:**
```bash
sudo timedatectl set-ntp true
sudo systemctl restart systemd-timesyncd
```

---

## How to Add a New Node to a Running Testnet

1. Set up `commputer.toml` with the seed address in `seeds`.
2. Set `data_dir` to a fresh empty directory.
3. Start the node: `./commputer --config commputer.toml`
4. The node will start in `Syncing` state and download all blocks.
5. Wait for: `Initial sync complete at height X`
6. Node is now `Active` and participating.

The existing nodes do not need to be restarted.

---

## How to Restart a Single Node Without Disrupting Others

1. Send SIGTERM (graceful shutdown):
   ```bash
   systemctl stop commputer
   # or: kill -TERM <PID>
   ```
2. Wait for: `State saved. Goodbye.` in logs (up to 10 seconds)
3. Start the node again:
   ```bash
   systemctl start commputer
   ```
4. The node will re-sync the blocks missed while down (usually fast).

**If SIGTERM doesn't work** (hung process):
```bash
kill -KILL <PID>  # force kill
# Node may need to re-sync from last saved height
```

The other nodes continue producing blocks normally while one node is restarting.
