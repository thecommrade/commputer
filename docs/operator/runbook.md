# Commputer Validator Operator Runbook

> **Status:** testnet-1 / pre-launch. Treat any "TODO" markers as items the
> founder must finalize before this guide is shipped to operators.

---

## 1. Executive summary

Commputer is a Layer-1 blockchain whose native token is **$COMME** (total
supply 2,000,000,000 with 8 decimals; see `src/core/src/token.rs`). Unlike most
chains it deliberately **penalizes scale**: validators colocated on the same
machine, the same /24 or /16 subnet, the same ASN, or known datacenter IP
ranges are flagged `NerfedIncidental` and earn drastically reduced rewards
(see `src/validator/src/compliance_check.rs:447` and the **Anti-scale rules**
section below). The intended operator profile is therefore a **single home
machine on a residential ISP**, not a rack of cloud instances.

Consensus is Snowball (sample=3, quorum=2, threshold=5). Block time is **2 s**
(`src/core/src/genesis.rs:40`, `testnet.toml`). Testnet epoch is **60 s**;
mainnet epoch is **3600 s** (`commputer.toml`, `genesis.json`). Block reward
starts at ~15.855 COMME and halves every 63,072,000 blocks
(`src/core/src/token.rs:14`).

What you do as an operator:

- Build the `commputer` binary from source.
- Generate a wallet (24-word BIP39 seed phrase).
- Start the node pointed at testnet seeds.
- The node will broadcast a `ValidatorRegister` transaction on first run
  automatically (`src/node/src/event_loop.rs:2268`).
- Keep the process up. Watch logs and `/health`.

Time commitment: ~30-60 min for first build & sync, then mostly autonomous.
Plan to be reachable for chat-based incident response during testnet.

---

## 2. Prerequisites

### 2.1 Hardware (recommended for testnet)

These are practical recommendations derived from what the node actually does
(RocksDB-backed `ChainState`, gossip + Kademlia DHT, multi-channel proof of
processing across CPU/GPU/RAM/storage/bandwidth, ~2 s blocks):

| Resource | Minimum | Recommended |
|---|---|---|
| CPU | 4 physical cores, x86_64 or aarch64 | 8 cores (AVX2 helpful for proofs) |
| RAM | 8 GB | 16 GB (RocksDB cache + libp2p buffers) |
| Disk | 50 GB SSD | 200 GB NVMe SSD (chain grows with epoch summaries) |
| Network | 25 Mbps symmetric | 100 Mbps symmetric, low-latency |
| Public IP | Optional but preferred | Yes (dedicated, residential ISP) |

Spinning rust (HDD) is **not recommended** — RocksDB compaction will starve
under it.

### 2.2 OS

- **Linux x86_64 / aarch64** is canonical. Tested on Debian bookworm
  (`Dockerfile:18`). Ubuntu 22.04+ also fine.
- **macOS (Apple Silicon or Intel)** — should build and run from source. Treat
  as best-effort; the systemd unit at `deploy/commputer.service` won't apply.
- **Windows** — not officially supported. WSL2 with an Ubuntu image works.
- **BSD** — not tested; `rocksdb` and `libp2p` should compile, but no support
  is offered.

### 2.3 Network

- **Open inbound: TCP 9000 and UDP 9000** (libp2p TCP + QUIC-v1 — see
  `src/network/src/transport.rs:278-283`). Both are required: the node listens
  on both transports simultaneously.
- **RPC port 9944** — by default binds to `127.0.0.1` only
  (`src/node/src/main.rs:79`, `commputer.toml:21`). Do **not** expose this to
  the internet unless you understand the tradeoffs and set `--rpc-key`.
- **Outbound: any** — the node dials seeds and discovered peers on TCP 9000 /
  UDP 9000 (or whatever ports they advertise).
- **NAT:** the node detects NAT type at startup (`main.rs:1089`). If you are
  behind symmetric NAT you may want `--relay` mode on a public node, or
  upstream port-forwarding for 9000/tcp + 9000/udp.
- **NTP:** required. The node runs a pre-flight NTP check on startup
  (`main.rs:923-938`). If your clock drifts by more than a few seconds your
  blocks will be rejected. Run `chrony` or `systemd-timesyncd`.

---

## 3. Build from source

The chain ships **no binary releases yet**. Build from source.

### 3.1 Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

The workspace targets edition 2024 (`src/Cargo.toml:17`). There is **no
`rust-toolchain.toml`** pinned in the repo, so `rustup` will give you `stable`.
The Dockerfile pins `rust:1.82-slim` (`Dockerfile:2`); use **rustc 1.82 or
newer**. The `Cargo.toml` contains no `rust-version` field — verify your
locally-built binary actually compiles before deploying.

### 3.2 System dependencies

```bash
# Debian / Ubuntu
sudo apt-get update
sudo apt-get install -y pkg-config libclang-dev clang librocksdb-dev \
                        build-essential ca-certificates git
```

```bash
# macOS (Homebrew)
brew install rocksdb pkg-config
```

### 3.3 Clone & build

```bash
git clone https://github.com/thecommrade/commputer.git
cd commputer/src
cargo build --release --bin commputer
```

Expected build time: **8-25 minutes** on the recommended hardware (LTO is on,
`codegen-units = 1` — see `src/Cargo.toml:86-90`). The release profile strips
symbols.

The binary lands at `src/target/release/commputer`. Either run it from there
or install:

```bash
sudo install -m 0755 src/target/release/commputer /usr/local/bin/commputer
commputer version
```

You should see something like:

```
commputer 0.1.0 (git: <hash>)
  Built:     <date>
  Protocol:  /commputer/0.1.0
  Chain ID:  commputer-testnet-1
  Supply:    2,000,000,000 COMME
  Consensus: Snowball (sample=3, quorum=2, threshold=5)
```

If `Chain ID` is **not** `commputer-testnet-1`, you have the wrong build —
stop and rebuild.

---

## 4. First-run setup

### 4.1 Directory layout

The node creates `~/.commputer/` automatically on first run
(`src/node/src/config.rs:103-108`). Layout:

```
~/.commputer/
├── config.toml          # optional — overrides CLI defaults
├── peer_id              # libp2p peer identity (auto-generated, persistent)
├── wallet/
│   └── wallet-testnet.json   # encrypted keystore (BIP39-derived)
└── testnet/             # RocksDB chain data (or mainnet/ on mainnet)
```

The wallet directory **survives** a `rm -rf ~/.commputer/testnet` (data wipe);
your seed phrase is the source of truth.

### 4.2 Wallet generation

You can let the node create a wallet on first `commputer run`, or do it
explicitly first:

```bash
commputer wallet create --testnet
```

You will be prompted for a password (used to encrypt the keystore at
`~/.commputer/wallet/wallet-testnet.json`) and shown a **24-word BIP39 seed
phrase**. **Write it down on paper.** It is the only way to recover the
wallet.

To recover an existing wallet:

```bash
commputer wallet recover --testnet
# enter 24-word phrase, set new password
```

To inspect:

```bash
commputer wallet show --testnet     # address, balance, tier, validator status
commputer wallet export --testnet   # re-print the seed phrase (requires password)
commputer wallet list --testnet     # list named wallets
```

### 4.3 Optional: `~/.commputer/config.toml`

CLI flags override the file. A sensible testnet config:

```toml
# ~/.commputer/config.toml
network = "testnet"
chain_id = "commputer-testnet-1"
seeds = ["seed.commputer.xyz:9000"]   # replaced by founder before launch
port = 9000
rpc_port = 9944
rpc_bind = "127.0.0.1"
epoch_duration = 60
contribution_percent = 100
log_level = "info"
cors_origins = "*"
```

The exact field names come from `src/node/src/config.rs:14-28`.

### 4.4 Headless / non-interactive

Two ways to avoid the password prompt at startup:

```bash
# 1. CLI flag
commputer run --testnet --password "$WALLET_PASS"

# 2. Environment variable (preferred for systemd)
COMMPUTER_WALLET_PASSWORD="$WALLET_PASS" commputer run --testnet
```

The env var is read by `read_password()` (`src/node/src/main.rs:337`).

---

## 5. Joining testnet

### 5.1 Genesis

The node creates the genesis block deterministically from
`src/core/src/genesis.rs` (`default_genesis()`), or, if present, from
`<data_dir>/genesis.json`. Every honest node produces the **same** genesis
block hash — the `auto_register_validator` flow includes the first 8 bytes of
this hash in the libp2p `identify` agent_version
(`src/node/src/main.rs:1051-1056`) so peers reject anyone on a different
chain.

The default `genesis.json` shipped at the repository root pins:

```json
{
  "chain_id": "commputer-testnet-1",
  "total_supply": 200000000000000000,
  "epoch_duration_secs": 3600,
  "emission_base_rate": 10000000000,
  "emission_floor_rate": 1000000000,
  "channel_floors": {
    "Processing": 0.20, "Gpu": 0.20, "Storage": 0.20,
    "Ram": 0.20, "Bandwidth": 0.20
  }
}
```

> **TODO before testnet:** publish the canonical genesis SHA-256 on
> `commputer.xyz` so operators can verify their copy. Until then, build from
> the same git commit as the seeds.

If your peers reject your blocks with a chain-mismatch error, your genesis
hash is wrong — wipe `~/.commputer/testnet/` and rebuild from the canonical
git tag.

### 5.2 Seed nodes

`src/network/src/transport.rs:316` currently contains:

```rust
pub const SEED_NODES: &[&str] = &[
    // Format: /ip4/<IP>/tcp/<PORT>/p2p/<PEER_ID>
    // Or QUIC: /ip4/<IP>/udp/<PORT>/quic-v1/p2p/<PEER_ID>
];
```

> **TODO before testnet: founder fills SEED_NODES at
> `src/network/src/transport.rs:316`.** Until that lands, every operator has
> to be given seed multiaddrs out-of-band and pass them with `--seeds`.

The default config falls back to `seed.commputer.xyz:9000`
(`src/node/src/config.rs:9`). Verify that DNS record is live before relying on
it.

To pass seeds explicitly:

```bash
commputer run --testnet \
  --seeds "/ip4/A.B.C.D/tcp/9000/p2p/12D3Koo...,/ip4/E.F.G.H/udp/9000/quic-v1/p2p/12D3Koo..." \
  --dns-seeds "seed1.commputer.xyz,seed2.commputer.xyz"
```

If you supply `--seeds`, the node marks itself as a "seed connector" and will
**not** produce blocks until at least one seed connects, to prevent forks
(`src/node/src/main.rs:1149-1153`).

### 5.3 Starting the node

Foreground, with verbose logging:

```bash
commputer run --testnet --log-level info
```

Common flags (all defined in `src/node/src/main.rs:62-110`):

| Flag | Default | Purpose |
|---|---|---|
| `--testnet` | `true` | testnet mode |
| `--mainnet` | `false` | overrides `--testnet` |
| `--port` | `9000` | libp2p TCP+QUIC listen port |
| `--rpc-port` | `9944` | HTTP/JSON RPC server |
| `--rpc-bind` | `127.0.0.1` | bind address (use `0.0.0.0` for remote RPC) |
| `--rpc-key` | none | shared-secret API key for RPC auth |
| `--contribution-percent` | `100` | 1-100; advertised hardware contribution |
| `--relay` | `false` | run as a libp2p relay (no mining) |
| `--seeds` | `[]` | comma-separated multiaddrs |
| `--dns-seeds` | `[]` | comma-separated DNS seed domains |
| `--password` | none | wallet password (alt: `COMMPUTER_WALLET_PASSWORD` env) |
| `--wallet` | `default` | named wallet selection |
| `--dashboard` | `false` | terminal TUI |
| `--json-log` | `false` | structured JSON logs (use this for systemd) |
| `--cors-origins` | `*` | CORS allowed origins |
| `--log-level` | `info` | trace / debug / info / warn / error |

Or use the convenience alias:

```bash
commputer mine             # equivalent to: run --testnet, contribution 100%
```

### 5.4 systemd unit

A reference unit ships at `deploy/commputer.service`:

```ini
[Service]
Type=simple
User=commputer
Group=commputer
ExecStart=/usr/local/bin/commputer run --testnet --port 9000 --rpc-port 9944
Restart=on-failure
RestartSec=10
LimitNOFILE=65535
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/var/lib/commputer
```

Adjust `User`/`ReadWritePaths`. If you run as a system user the wallet/data
directories must live under `/var/lib/commputer`, not `/root/.commputer`.

Set `COMMPUTER_WALLET_PASSWORD` via a `EnvironmentFile=` referencing a
`chmod 600` file. Never put the password directly in the unit file.

---

## 6. Becoming a validator

### 6.1 What happens automatically

When you run `commputer run` (or `commputer mine`), the event loop calls
`auto_register_validator(contribution_percent)`
(`src/node/src/main.rs:1147`, body in `src/node/src/event_loop.rs:2268-2329`).
That function:

1. Computes a SHA-256 of your hardware fingerprint.
2. Builds a `Transaction { kind: TxKind::ValidatorRegister { hardware_fingerprint_hash, contribution_percent }, fee: 0, ... }`
   (registration is fee-exempt, see `event_loop.rs:2303`).
3. Signs it with your wallet key.
4. Publishes it to the gossipsub `tx` topic.
5. Adds it to the local mempool so it lands in the next block.

You don't need to issue a separate `register` command.

### 6.2 Verifying registration

Once your tx is included in a block, your account flips `is_validator = true`:

```bash
commputer balance <your-address-hex>
# Look for: Validator: yes

commputer validator-status <your-address-hex>
```

Or query RPC directly:

```bash
curl -s http://127.0.0.1:9944/account/<address-hex> | jq
curl -s http://127.0.0.1:9944/validators | jq
```

### 6.3 Anti-scale rules — read this carefully

`ComplianceChecker::check()` in `src/validator/src/compliance_check.rs:447`
runs every epoch. It compares your reported IP against every other validator
and returns one of:

- `Compliant` — full reward
- `NerfedIncidental` — same IP / same /24 / same /16 / same ASN / known
  datacenter IP. Reward is multiplied by the `multi_node_multiplier` table:
  1 → 1.0, 2 → 0.25, 3 → 0.0625, 4 → 0.015625, 5+ → 0.0
  (`compliance_check.rs:655-663`).
- `NerfedAdversarial` — duplicate hardware fingerprint, or >3 validators
  behind the same IP (VPN/proxy). Treated more harshly than incidental.

Concretely:

- **Do not run two validators on the same machine.** Same fingerprint →
  `NerfedAdversarial` for both (`compliance_check.rs:117-136`).
- **Do not run on AWS / GCP / Azure / Hetzner / OVH / DigitalOcean.** Their IP
  ranges are hardcoded (`compliance_check.rs:292-352`); you will be flagged
  `NerfedIncidental` for being in a datacenter regardless of how many
  neighbours you have.
- **Do not co-locate with another operator on the same /16.** That includes
  most municipal-fiber CGNAT pools.
- **Do not use a VPN.** Four or more validators behind the same egress IP →
  `NerfedAdversarial` (`compliance_check.rs:466-468`).

Compliance is restored automatically once the offending peer deregisters or
moves IP (`compliance_check.rs:597-604`).

> Compliance status appears on `/compliance` (RPC) and via `commputer
> compliance-check`.

### 6.4 Faucet (testnet only)

The node hosts a `POST /faucet` endpoint (`src/node/src/rpc.rs:1190`) that
dispenses **10 COMME per address per epoch**
(`src/node/src/faucet.rs:7,55-70`). Use this only for smoke-testing transfers;
mining rewards are the real source of testnet COMME.

---

## 7. Monitoring & operations

### 7.1 Logs

Default tracing format — set verbosity with `--log-level`:

```bash
commputer run --testnet --log-level debug
```

For systemd / log shippers, use `--json-log` for structured output
(`main.rs:106-107`). The standard tracing env-filter applies, e.g.
`RUST_LOG=commputer_network=debug,info`.

Key log lines to watch:

- `Genesis block hash: ...`
- `P2P peer ID: 12D3Koo...`
- `NAT type: <Open | Cone | Symmetric | Unknown>`
- `Connected to N built-in seed nodes`
- `Registered as validator at N% contribution`
- `Resumed chain at height ...` on restart

### 7.2 RPC endpoints

Full router at `src/node/src/rpc.rs:1158-1192`. The endpoints an operator
actually polls:

| Endpoint | Use |
|---|---|
| `GET /` | HTML block explorer |
| `GET /status` | height, supply, accounts, epoch, pending_txs |
| `GET /health` | enhanced health (uptime, sync status) |
| `GET /metrics` | JSON node metrics |
| `GET /metrics/prometheus` | Prometheus text exposition |
| `GET /peers` | connected peers (peer_id, ip, validator_address) |
| `GET /validators` | active validator set |
| `GET /balance/{address}` | account balance + tier |
| `GET /nonce/{address}` | next nonce |
| `GET /block/{height}` | block by height |
| `GET /receipt/{tx_hash}` | tx receipt |
| `GET /mempool` | pending txs |
| `GET /compliance` | network-wide compliance dashboard |
| `GET /anti-scale` | anti-scale dashboard |
| `GET /capacity` | aggregate hardware capacity |
| `GET /supply` | emitted/burned/circulating |
| `GET /storage/metrics` | RocksDB metrics |
| `GET /network` | network health |
| `GET /network/info` | network info |
| `GET /network/quality` | per-peer quality |
| `GET /proofs/status` | per-channel proof status |
| `GET /proofs/leaderboard` | leaderboard |
| `GET /ws` | WebSocket — real-time block / tx events |
| `POST /tx` | submit signed tx |
| `POST /faucet` | claim testnet COMME |

> **Prometheus note:** `/metrics/prometheus` exists in code today
> (`src/node/src/rpc.rs:486-487`) but additional Prometheus metric coverage
> is staged work — track `src/staging/` for additions.

### 7.3 CLI quick-checks

```bash
commputer status --testnet                  # local chain status
commputer peers                             # via RPC
commputer mining-stats                      # via RPC
commputer network-info                      # via RPC
commputer validator-status <addr-hex>       # via RPC
commputer compliance-check                  # via RPC
commputer sys-check                         # local hardware vs requirements
```

### 7.4 Suggested Prometheus scrape

```yaml
- job_name: commputer
  scrape_interval: 15s
  static_configs:
    - targets: ['127.0.0.1:9944']
  metrics_path: /metrics/prometheus
```

If you `--rpc-bind 0.0.0.0` for remote scraping, also set `--rpc-key` and
firewall the port.

---

## 8. Upgrade procedure

The chain has no on-chain governance for upgrades yet; coordinate via
operator chat.

1. **Watch for the announcement.** Founder will publish a target git tag.
2. **Build the new binary in a separate path** so the running node keeps
   serving:

   ```bash
   git fetch --tags
   git checkout v0.x.y
   cd src
   cargo build --release --bin commputer
   ```

3. **Stop the node** (let it flush state cleanly):

   ```bash
   sudo systemctl stop commputer
   # or Ctrl-C if foreground; the panic hook flushes
   #   src/node/src/main.rs:1211-1230
   ```

4. **Replace the binary**:

   ```bash
   sudo install -m 0755 src/target/release/commputer /usr/local/bin/commputer
   commputer version   # confirm new git hash
   ```

5. **Start the node** and watch logs for `Resumed chain at height N`. Height
   should match what was logged at shutdown.

6. **Sanity-check** for ~5 minutes: `/health`, `/peers`, mempool drains.

The CLI ships a `commputer update` command (`src/node/src/main.rs:1165-1202`)
which only checks the GitHub releases API and prints the version delta — **it
does not replace the binary today**. Treat it as a "is there a newer release?"
notifier.

If the upgrade is a **hard fork** (genesis or protocol change), wipe
`~/.commputer/testnet/` after stopping. Your wallet under
`~/.commputer/wallet/` is unaffected.

---

## 9. Backup & disaster recovery

### 9.1 What to back up

| Path | Why |
|---|---|
| `~/.commputer/wallet/` | The keystore — **irreplaceable** without seed |
| Your 24-word seed phrase | **The** disaster recovery primitive (offline) |
| `~/.commputer/peer_id` | Stable libp2p identity for your node |
| `~/.commputer/testnet/` (optional) | Fast recovery; resyncable from peers |

### 9.2 Built-in backup

```bash
commputer backup commputer-backup.tar.gz --testnet
# defined at src/node/src/main.rs:163-169 (Feature 187)
```

### 9.3 Restore

```bash
commputer restore commputer-backup.tar.gz --testnet
```

### 9.4 Verify chain integrity

```bash
commputer verify-chain --testnet     # full re-verification of all blocks
commputer verify-state --testnet     # merkle tree integrity
commputer rebuild-indexes --testnet  # rebuild secondary indexes
```

### 9.5 Wallet loss

If the keystore file is destroyed but you have your seed phrase:

```bash
rm -f ~/.commputer/wallet/wallet-testnet.json
commputer wallet recover --testnet
```

If you lose **both** the keystore and the seed phrase, the wallet — and any
$COMME in it — is gone. There is no central reset mechanism.

---

## 10. Troubleshooting (top 5)

### 10.1 "No connected peers" / `Connected to 0 built-in seed nodes`

**Diagnosis:** `SEED_NODES` is empty (`src/network/src/transport.rs:316`) or
DNS for `seed.commputer.xyz` is not resolving, **or** UDP 9000 is being
dropped by your ISP / NAT.

**Fix:**
- Pass seeds explicitly: `--seeds "/ip4/.../tcp/9000/p2p/..."`.
- Confirm `nc -vz seed.commputer.xyz 9000` returns success.
- Test UDP: `nmap -sU -p 9000 seed.commputer.xyz`.
- Confirm your firewall lets UDP 9000 in *and* out (QUIC).
- Check `commputer peers` after 60 s.

### 10.2 `NTP check FAILED` on startup

**Diagnosis:** clock skew > tolerated bound (see `main.rs:923-938`). Your
blocks will be rejected.

**Fix:** install and enable a time daemon:

```bash
sudo apt-get install -y chrony
sudo systemctl enable --now chrony
chronyc tracking
```

Re-run `commputer run`.

### 10.3 `Validator: yes` but rewards are 0 / very low

**Diagnosis:** you are flagged `NerfedIncidental` or `NerfedAdversarial`. See
section 6.3.

**Fix:**

```bash
commputer compliance-check
curl -s http://127.0.0.1:9944/compliance | jq
curl -s http://127.0.0.1:9944/anti-scale | jq
```

If the response shows your IP shares a /24 or /16 with another validator,
either you or they must move. If your IP is in a datacenter range, you must
relocate to a residential ISP — there is no whitelist override at this stage.

### 10.4 `Wallet password` prompt on a headless box

**Diagnosis:** the node always loads or creates a wallet at startup
(`main.rs:970-1035`).

**Fix:** set `COMMPUTER_WALLET_PASSWORD` in a 0600 env-file referenced by
your systemd unit, or pass `--password` (less secure — visible in `ps`).

### 10.5 `panic: ...` crash followed by clean exit

**Diagnosis:** the custom panic hook (`main.rs:1211-1230`) logs the location
and exits with code 1. The chain state should be flushed by destructors, but
this is not guaranteed.

**Fix:**
1. Capture the full panic line (file:line:column + message) and report it.
2. Run `commputer verify-state --testnet`. If errors, run
   `commputer rebuild-indexes --testnet`.
3. If verification still fails: stop the node, move
   `~/.commputer/testnet/` aside, restart — the node will resync from peers
   (this can take time depending on chain length).

### 10.6 Bonus: build fails with `error: linking with cc failed: ld: -lrocksdb`

**Diagnosis:** `librocksdb-dev` not installed, or your distro ships a too-old
RocksDB. Workspace pins `rocksdb = "0.24"` (`src/Cargo.toml:42`).

**Fix on Debian/Ubuntu:**

```bash
sudo apt-get install -y librocksdb-dev clang libclang-dev
```

If your distro's RocksDB is too old, build the bundled crate by setting
`ROCKSDB_STATIC=1` before `cargo build`.

---

## 11. Security checklist

Run through this before exposing the node:

- [ ] Seed phrase written down on paper, stored offline, **not** photographed.
- [ ] Wallet keystore password is unique to this machine (not reused).
- [ ] `--rpc-bind` left at `127.0.0.1` unless you specifically need remote
      RPC. If remote, `--rpc-key` is set and the port is firewalled.
- [ ] systemd unit's `EnvironmentFile` for `COMMPUTER_WALLET_PASSWORD` is
      `chmod 600`, owned by the service user.
- [ ] `~/.commputer/wallet/` is `chmod 700` and owned by the service user.
- [ ] Outbound only on TCP/UDP 9000 inbound. Everything else firewalled.
- [ ] Time sync daemon (`chrony` / `systemd-timesyncd`) is running.
- [ ] Disk free monitoring is set up — RocksDB will not gracefully recover
      from a full disk.
- [ ] Log shipping does not capture wallet password env-file contents.
- [ ] You are running on **residential ISP**, on **a single machine**, with
      **a single validator identity** — anything else gets nerfed (section 6.3).
- [ ] You can be reached on the operator chat in case of an emergency
      coordinated action (e.g. hard fork rollback).
- [ ] You have read the consensus explainer (`docs/guides/consensus_explainer.md`)
      and the anti-scale doc (`docs/ANTI_SCALE.md`).

---

## Appendix A. Useful commands cheat-sheet

```bash
# Build & install
cd src && cargo build --release --bin commputer
sudo install -m 0755 src/target/release/commputer /usr/local/bin/

# Wallet
commputer wallet create --testnet
commputer wallet recover --testnet
commputer wallet show --testnet
commputer wallet export --testnet
commputer wallet list --testnet

# Run
commputer run --testnet
commputer mine                                # alias for run --testnet
COMMPUTER_WALLET_PASSWORD=xxx commputer run --testnet

# Inspect
commputer version
commputer status --testnet
commputer sys-check
commputer peers
commputer mining-stats
commputer network-info
commputer validator-status <addr-hex>
commputer compliance-check
commputer balance <addr-hex>

# Maintenance
commputer backup commputer-backup.tar.gz --testnet
commputer restore commputer-backup.tar.gz --testnet
commputer verify-chain --testnet
commputer verify-state --testnet
commputer rebuild-indexes --testnet
commputer export-chain chain-export.json --testnet

# Tx
commputer send <to-hex> <amount-COMME> --testnet
commputer burst --cpu 4 --ram 8gb --duration 1h
```

---

## Footer — unverified assumptions

The following statements in this runbook were not verifiable from the source
read at authoring time. The **founder** must confirm or correct each before
this guide is shipped to operators.

1. **`SEED_NODES` empty at launch.** `src/network/src/transport.rs:316`
   currently has zero entries. The runbook tells operators to use `--seeds`
   manually until the founder fills this in. **Action:** populate
   `SEED_NODES` and recut a release before public testnet, or document
   the canonical out-of-band seed list.
2. **`seed.commputer.xyz` DNS record.** `src/node/src/config.rs:9` references
   this hostname. Whether it actually resolves at testnet launch is not
   verified. **Action:** confirm DNS is provisioned (A + AAAA), or remove
   the default.
3. **Genesis hash publication.** The runbook tells operators to verify
   `genesis.json` matches a published SHA-256. No such hash is published
   anywhere in the repo today. **Action:** publish on `commputer.xyz` and
   link from this doc.
4. **MSRV.** No `rust-version` field in any workspace `Cargo.toml`. The
   Dockerfile pins `rust:1.82-slim`. The runbook recommends rustc ≥ 1.82
   based on that. **Action:** add `rust-version = "1.82"` to
   `[workspace.package]` to make this enforceable.
5. **Hardware recommendations.** RAM/CPU/disk numbers are derived from
   architectural reasoning (RocksDB cache, libp2p buffers, multi-channel PoW
   prover, ~2 s blocks) — they have not been load-tested at full network
   scale. **Action:** revise after the first stress run with N=10 external
   validators.
6. **`ValidatorRegister` fee = 0.** The runbook states this based on
   `event_loop.rs:2303`. If the validator-register fee is ever raised the
   guide must be updated.
7. **`commputer update` semantics.** The runbook describes `commputer update`
   as "notification-only, does not replace binary" based on
   `main.rs:1165-1202`. If a future version performs an actual self-replace,
   the upgrade procedure changes substantially.
8. **NAT type detection accuracy.** `network.detect_nat_type()` is called at
   startup but its full behaviour was not audited line-by-line. **Action:**
   verify the four-state output (`Open`/`Cone`/`Symmetric`/`Unknown`) and
   document remediation per state.
9. **Anti-scale rules — exact thresholds.** Documented from
   `compliance_check.rs:447-507`. The runbook does not exhaustively list the
   `multi_node_multiplier` table beyond the first five values; verify there
   are no edge cases at very large N before publishing.
10. **`--rpc-key` enforcement.** The CLI flag exists at
    `main.rs:82-83` but the actual auth middleware in `rpc.rs` was not
    audited. **Action:** confirm rejection of unauthenticated requests on
    `0.0.0.0`-bound RPC before recommending public exposure.
11. **Faucet rate-limit window.** Stated as "1 claim per address per epoch"
    based on `faucet.rs:55-70`. Whether the production testnet keeps the same
    `FAUCET_AMOUNT = 10 COMME` is a parameter the founder may wish to tune.
12. **`librocksdb7.8` runtime dep.** The `Dockerfile:22` pins this Debian
    package. Different host distros may need a different package name; the
    runbook recommends `librocksdb-dev` for build only. **Action:** confirm
    runtime linkage on each supported distro.

---

*End of runbook.*
