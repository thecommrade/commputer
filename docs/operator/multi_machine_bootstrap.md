# Multi-machine testnet bootstrap — three-operator ceremony

> Audience: founder + two volunteer operators. The goal is to launch
> the first three-machine Commputer testnet from genesis, without
> seeds, in a coordinated 5-minute window so all three nodes converge
> on the same chain head from block 1.

## Roles

* **Operator A — Bootstrap leader.** Runs node1 with NO `--seeds`.
  Will produce the first block solo (`auto_register_validator` flow at
  `src/node/src/event_loop.rs:2268`). The protocol's leader-election
  gate in `handle_block_tick` only allows a node WITHOUT `--seeds` to
  produce the first block when fewer than 2 validators have been
  registered on-chain. The founder is the natural fit for this role.
* **Operator B — Validator-2.** Runs node2 with `--seeds <A's multiaddr>`.
* **Operator C — Validator-3.** Runs node3 with `--seeds <A's multiaddr>`.

The runbook assumes ALL three operators are on the same git tag of the
node binary, with identical `genesis.json`. Diverging on either is the
#1 way this ceremony fails.

## Cloud datacenter warning — read first

`src/validator/src/compliance_check.rs:291-352` flags AWS, GCP, Azure,
Hetzner, OVH, and DigitalOcean IP ranges as `NerfedIncidental`. **A
validator on any of these clouds will be silently bricked: the node
runs, the chain accepts its blocks, but its rewards are multiplied by
the multi-node nerf table** (1 validator: 1.0x; 2: 0.25x; 3: 0.0625x; 5+:
0.0). For testnet, where COMME has no value, the nerf is harmless to
the node but still misleading — operators see "Validator: yes" and
think everything works.

**Recommended:** all three operators on **bare-metal residential ISP**
or **a colocation rack with no cloud-marketplace ASN**. If you must use
a VPS for testnet-1, document it explicitly so the chain-health
analysis knows why rewards are zero.

## Before-the-ceremony checklist (each operator, T-24 hours)

1. **Build the binary:**
   ```bash
   cd commputer/src
   cargo build --release --bin commputer
   sudo install -m 0755 target/release/commputer /usr/local/bin/commputer
   commputer version    # confirm same git hash as the other two operators
   ```
2. **Generate node keypair, writing it to the path the node actually reads
   (`~/.commputer/peer_id`):**
   ```bash
   commputer-keygen --out ~/.commputer/peer_id
   # records the peer ID; you'll need it again at step 4.
   ```
   **CRITICAL:** the node loads its libp2p identity ONLY from
   `~/.commputer/peer_id`. If you generate the key anywhere else and don't
   copy it there before the first start, the node boots with a DIFFERENT
   random peer ID and the multiaddr you compute in step 4 will point at a
   peer that does not exist — every follower's seed dial to you will fail.
   Verified end-to-end: a node with this file pre-placed logs "Loaded
   persistent peer identity"; without it, "Generated and saved new peer
   identity" (a fresh random key).
3. **Open firewall:** TCP 9000 + UDP 9000 inbound (libp2p TCP + QUIC).
4. **Compute your multiaddr:**
   ```bash
   commputer-multiaddr-builder \
     --peer-id <peer-id-from-step-2> \
     --ip <your-public-ip> \
     --port 9000
   ```
   For Operator A only, ALSO compute the QUIC variant:
   ```bash
   commputer-multiaddr-builder \
     --peer-id <peer-id-from-step-2> \
     --ip <your-public-ip> \
     --port 9000 --proto quic
   ```
5. **Verify your multiaddr is reachable** from someone else's network
   (a phone hotspot is fine):
   ```bash
   commputer-verify-multiaddr "/ip4/.../tcp/9000/p2p/..."
   ```
   `parse: ok` and `reachable: true` are required.
6. **Confirm time sync:**
   ```bash
   chronyc tracking 2>/dev/null || timedatectl
   ```
   System Offset / Last sync should be < 1 second ago.
7. **Pre-stage `genesis.json`:**
   * The repo root has `genesis.json`. Compute its sha256:
     ```bash
     sha256sum genesis.json
     ```
   * Each operator computes this independently. The three hashes MUST
     match. If they don't, you're not on the same commit. Stop and
     align.
8. **Run the precheck script** (`src/staging/scripts/multi_machine_testnet/precheck_node.sh`)
   against your machine:
   ```bash
   ./precheck_node.sh /etc/commputer/node_key.bin <expected_genesis_sha256>
   ```
   It exits non-zero on any failed check. Fix and re-run before the
   ceremony.

## Pre-launch coordination (T-30 minutes)

All three operators in a real-time chat:

1. **Multiaddr exchange.** Operator A pastes their full TCP multiaddr
   AND their QUIC multiaddr (both forms) into chat. B and C copy these
   verbatim into their startup commands.
2. **Genesis hash agreement.** All three operators confirm their
   `sha256sum genesis.json` matches. If any differs, abort.
3. **Wallet pre-creation.** Each operator runs:
   ```bash
   commputer wallet create --testnet
   ```
   and writes the seed phrase down on paper. The wallet password is
   set via the `COMMPUTER_WALLET_PASSWORD` env var for non-interactive
   start; agree on whether each operator types it manually at startup
   or pre-stages an env-file.
4. **Time sync drift check.** Each operator pastes `date -u` output to
   chat. Drift between operators' clocks must be < 5 seconds. If any
   operator is more than 5s off, stop, install chrony, re-confirm.
5. **Genesis path agreement.** The node looks for `genesis.json` at
   `<data_dir>/genesis.json` (default `~/.commputer/testnet/`). All
   three operators copy the canonical `genesis.json` to that path.

## T-5 minutes: final checks

Each operator runs again on their own machine:

```bash
./precheck_node.sh /etc/commputer/node_key.bin <expected_genesis_sha256>
```

If it fails, abort and reschedule. **Do not partially launch.**

Operator A confirms in chat: "All checks green, ready to go at T0."
B and C confirm: "Standing by; will launch at T0+30s."

## T-1 minute

Each operator opens their `tail_chain_health.sh` window and the node's
log file in two split panes. Verify both are blank/idle.

## T0: Operator A launches

```bash
COMMPUTER_WALLET_PASSWORD="$WALLET_PASS" \
  commputer run --testnet \
    --port 9000 --rpc-port 9944 --rpc-bind 127.0.0.1 \
    --log-level info \
    --json-log
```

* Operator A confirms in chat: "Node1 launched, height advancing."
* Operator A waits ~10-15 seconds, watching the log:
  - "Genesis block hash: ..."
  - "P2P peer ID: 12D3Koo... (matches keygen output)"
  - "auto_register_validator submitted at height 1"
  - "Produced block candidate at height 1"
  - "Finalized and applied block at height 1"

If any of those don't appear within 30 seconds, ABORT and investigate.

## T+30s: Operators B and C launch simultaneously

Both run within ~5 seconds of each other:

```bash
COMMPUTER_WALLET_PASSWORD="$WALLET_PASS" \
  commputer run --testnet \
    --port 9000 --rpc-port 9944 --rpc-bind 127.0.0.1 \
    --seeds "<operator-A's-tcp-multiaddr>,<operator-A's-quic-multiaddr>" \
    --log-level info \
    --json-log
```

Each watches their log for:

* "Connected to N built-in seed nodes" -> N >= 1
* "Resumed chain at height ..." -> their height ticks up to A's height
* "auto_register_validator submitted at height M" -> their validator
  register tx lands

## T+2 minutes: verification

All three operators run `tail_chain_health.sh` against their LOCAL node.
The script polls `/status` every 10 seconds and prints whether all
three nodes are in lockstep.

Expected steady state after T+2 minutes:

* All three node heights within 1 of each other.
* All three peer counts >= 2.
* All three validator counts == 3.
* No `panicked at` lines in any log.
* No NerfedAdversarial in `/compliance` (NerfedIncidental on cloud is
  expected; see warning above).

If any check fails for more than 60 seconds, declare ceremony failure,
have all operators stop their nodes, archive logs, and post-mortem.

## Post-launch verification (T+30 minutes)

Each operator verifies:

1. **Chain progress.** `commputer status --testnet` shows height
   advancing roughly every 2 seconds.
2. **Validator registration.** `commputer validator-status <addr>` shows
   `Validator: yes` for their own address.
3. **Lockstep.** All three operators report `/status` height within 1
   block of each other. The `tail_chain_health.sh` script visualises
   this continuously.
4. **No restarts.** The node process has been up the whole time (no
   "Resumed chain at height" log line after the initial startup).
5. **Compliance.** `commputer compliance-check` shows `Compliant` for
   each (or `NerfedIncidental` if on cloud, expected).

If all five hold for 30+ minutes, the ceremony succeeded.

## Rollback / abort

If anything goes wrong before T+10 minutes:

1. Each operator hits Ctrl-C on their node (the panic hook flushes
   state).
2. Each operator wipes their data dir:
   ```bash
   rm -rf ~/.commputer/testnet/
   ```
3. WALLETS ARE PRESERVED at `~/.commputer/wallet/` — those are tied to
   seed phrases, not to chain state.
4. Post-mortem in chat: collect the last 200 lines of each node's log,
   note which operator's node misbehaved first, and reschedule.

If something goes wrong AFTER T+30 minutes (chain has been running for
a while):

* Don't wipe data dirs blindly. Identify which node fell behind.
* If the affected node's height is 100+ blocks behind, run
  `commputer verify-state --testnet` and `commputer rebuild-indexes`
  before deciding to resync from peers.
* Resync (delete `~/.commputer/testnet/`, restart with `--seeds`) is
  always available, but losing your local state means re-uploading
  blocks from peers — slow on real networks.

## Communication discipline

* All chat is timestamped.
* Multiaddrs are quoted verbatim (`code blocks`) — never paraphrased.
* Each operator confirms each step explicitly: "B: launched at HH:MM:SS,
  height = N". Silent operators block the ceremony.
* If anyone is unsure, they say "HOLD" and the founder pauses the
  ceremony. Better to delay 5 minutes than to launch a doomed chain
  state that requires a full wipe later.

## Operator quick-reference

What to type, in order, on the day:

1. `./precheck_node.sh /etc/commputer/node_key.bin <expected_sha>` (T-30m)
2. `./precheck_node.sh /etc/commputer/node_key.bin <expected_sha>` (T-5m)
3. Operator A: launch at T0.
4. Operators B+C: launch at T+30s.
5. All: open `./tail_chain_health.sh <RPC1> <RPC2> <RPC3>` panel.
6. Wait 30 minutes.
7. Confirm in chat: "Ceremony PASS. Testnet launched at HH:MM:SS UTC,
   chain head N."

## Appendix — what the ceremony tests

The "no-seeds bootstrap leader, others seed from leader" topology is
the same as `scripts/multinode_smoke.sh` — but on three physical
machines instead of three loopback ports. What changes between local
smoke and multi-machine ceremony:

* Real network: TCP MSS, MTU, packet loss, NAT.
* Real clocks: chrony drift, leap seconds.
* Real public IPs: NerfedIncidental from the cloud-IP detector
  (`src/validator/src/compliance_check.rs`).
* Real keypairs: persistent `peer_id` per operator, real ed25519 sigs.
* Real coordination cost: this runbook, not a script that does
  everything.

Anything that survives the local smoke but breaks the multi-machine
ceremony is a real-network bug. That's why we do this — to catch them
before mainnet.
