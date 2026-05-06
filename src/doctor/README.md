# commputer-doctor

Pre-launch validator/linter for Commputer node operators.

Run this **before** `commputer-node` starts. Its job is to catch the silent-but-fatal misconfigurations that would otherwise let the node "boot" into a state where it produces nothing, gets nerfed, or rejects every block.

## What it checks

| Category | Check | Severity if bad |
|---|---|---|
| Config TOML | file readable, parses as TOML | Error |
| Config TOML | `network` is `mainnet`/`testnet` | Error |
| Config TOML | `chain_id` non-empty + matches `network` family | Error |
| Config TOML | `port` != `rpc_port` | Error |
| Config TOML | ports >= 1024 (privileged warn) | Warning |
| Config TOML | `rpc_bind` not exposed wide-open | Warning |
| Config TOML | `epoch_duration` >= 10s and sane for network | Error / Warning |
| Config TOML | `contribution_percent` in 1..=100 | Error |
| Config TOML | `log_level` is a known level | Error |
| Config TOML | seed list non-empty + each entry `host:port` | Warning / Error |
| Config TOML | `cors_origins` not `*` | Warning |
| Genesis | file readable + valid JSON | Error |
| Genesis | SHA-256 digest emitted for cross-check | Info |
| Genesis | `chain_id` matches operator config | Error |
| Genesis | `total_supply` > 0 | Error |
| Genesis | `emission_floor_rate` <= `emission_base_rate` | Error |
| Genesis | `epoch_duration_secs` aligns with config | Warning |
| Genesis | `channel_floors` valid + sum <= 1.0 | Error |
| Genesis | `protocol_version` matches binary (if present) | Error |
| Net | NTP drift < 5s | Warning |
| Net | P2P + RPC ports bindable now | Error |
| Net | Public IP not in flagged datacenter range | Warning |

That is **24 checks** in total, plus a SHA-256 integrity digest of the genesis bytes.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Clean. Safe to launch. |
| `1` | Warnings only. Operator should review; node MAY start. |
| `2` | At least one Error. Operator MUST fix; node MUST refuse to launch. |

`--strict` promotes warnings to exit code `2` so you can use the doctor as a hard gate in systemd/Kubernetes pre-start hooks.

## Usage

```
commputer-doctor [--config <path>] [--genesis <path>] [flags]
```

Defaults:
- `--config` = `~/.commputer/config.toml`
- `--genesis` = `./genesis.json`

Flags:
- `--check-public-ip <ip>` — classify a single IP and exit (uses the same CIDR table as `src/validator/src/compliance_check.rs:291-352`)
- `--binary-version <v>` — current binary version, compared against `protocol_version` in genesis (if encoded there)
- `--expected-chain-id <s>` — override the chain_id we compare genesis against
- `--skip-net` — skip NTP, port-bind, and public-IP probes (CI-friendly)
- `--strict` — treat any warning as fatal
- `--json` — machine-readable output for piping into other tooling

## Sample output

```
$ commputer-doctor --config ~/.commputer/config.toml --genesis ./genesis.json
================ commputer-doctor ================
[OK]   config.parse                  parsed /home/op/.commputer/config.toml
[OK]   config.network                network='testnet'
[OK]   config.chain_id               chain_id='commputer-testnet-1'
[OK]   config.ports                  p2p=9000 rpc=9944
[OK]   config.rpc_bind               RPC bound to localhost (safe default)
[OK]   config.epoch_duration         60s
[OK]   config.contribution_percent   100%
[OK]   config.log_level              'info'
[OK]   config.seeds                  1 seed(s) configured
[WARN] config.cors                   cors_origins='*' allows any origin to call your RPC
         -> narrow to specific origins for production
[OK]   genesis.sha256                3f...c1 (./genesis.json)
[OK]   genesis.chain_id              'commputer-testnet-1'
[OK]   genesis.chain_id.match        config and genesis chain_id agree
[OK]   genesis.total_supply          200000000000000000 base units
[OK]   genesis.emission              base=10000000000 floor=1000000000
[OK]   genesis.epoch_duration        3600s
[WARN] genesis.epoch_duration.match  operator epoch_duration=60 != genesis epoch_duration_secs=3600
         -> genesis is canonical; align your config to it
[OK]   genesis.channel_floors        5 channels, sum=1.0000
[OK]   net.port.p2p                  port 9000 (p2p) is free to bind
[OK]   net.port.rpc                  port 9944 (rpc) is free to bind
[OK]   net.ntp                       clock drift vs pool.ntp.org:123: -0.012s (OK)
[WARN] net.public_ip                 public IP 3.4.5.6 matches AWS datacenter range
         -> validators on commercial cloud are flagged NerfedIncidental ...
--------------------------------------------------
summary: 18 OK, 3 WARN, 0 FAIL
```

Exit code: `1` (warnings only).

## Pre-launch checklist

Before flipping `systemctl start commputer-node`:

1. **Ports** — `commputer-doctor` says both p2p and rpc are bindable. If not, an old `commputer-node` is still running.
2. **chain_id agreement** — operator config and genesis must declare the same `chain_id`.
3. **Public IP** — if the doctor flags your IP as datacenter, you will be silently nerfed (`NerfedIncidental`). Move to a residential / colo network or accept the reduced rewards.
4. **NTP** — drift < 5s. If you cannot reach NTP, fix that first; consensus relies on synchronized clocks.
5. **Genesis SHA-256** — compare the printed digest against the canonical value posted by the network operators. If they disagree, somebody's genesis is wrong.
6. **Epoch alignment** — operator `epoch_duration` should match genesis `epoch_duration_secs`. Genesis wins.
7. **RPC bind** — `127.0.0.1` unless you have a reverse proxy with auth. `0.0.0.0` exposes you to the open internet.
8. **Contribution percent** — if you set this below 25%, the doctor warns; you will earn less.

## Pinning into systemd

```ini
[Service]
ExecStartPre=/usr/local/bin/commputer-doctor --strict --config /etc/commputer/config.toml --genesis /etc/commputer/genesis.json
ExecStart=/usr/local/bin/commputer-node --config /etc/commputer/config.toml
```

`ExecStartPre` returning non-zero aborts `ExecStart`, so a misconfigured node never gets to run.

## Source-of-truth notes

- The cloud / datacenter CIDR table in `checks/cloud_ip.rs` is a **verbatim copy** of the prefixes in `src/validator/src/compliance_check.rs::is_datacenter_ip` (lines 291-352). When that list is updated, update this file in the same patch.
- The doctor deliberately does **not** depend on the node or validator crates. It is meant to ship as a tiny standalone binary that operators can run on a fresh box without compiling the entire workspace.
