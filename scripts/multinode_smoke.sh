#!/usr/bin/env bash
# multinode_smoke.sh — local 2-3 node smoke harness for Commputer
#
# Boots 2-3 commputer node processes locally on 127.0.0.1 with isolated data
# directories and disjoint P2P / RPC ports, lets them produce + finalize blocks
# cross-node, and tails each node's stdout/stderr into a configurable log dir.
# Useful as a quick pre-deploy sanity check that multi-node consensus works.
# Data dirs at /tmp/multinode-smoke-N/ are wiped on each start so each run
# begins from genesis.
#
# Topology: BOOTSTRAP-LEADER. Node 1 starts with NO --seeds (becomes the
# bootstrap leader and produces the first block solo, which includes the
# ValidatorRegister txs from the mempool). Nodes 2..N seed only from node 1.
# Required because the protocol's leader-election gate in handle_block_tick
# only allows a node WITHOUT --seeds to produce the first block when fewer
# than 2 validators have been registered on-chain.
#
# Exit codes:
#   0  — all nodes exited cleanly (caller sent SIGINT or SMOKE_DURATION elapsed)
#   2  — one or more nodes panicked or exited non-zero before shutdown
#   3  — build failed
#   4  — a node failed to come up (RPC /status never responded)
#
# Environment knobs:
#   SMOKE_NODES        number of nodes to start (2 or 3, default 3)
#   SMOKE_DURATION     seconds to let nodes run before tearing down
#                      (default 60; set to 0 to run until Ctrl-C)
#   SMOKE_BASE_P2P     base P2P port (default 19000; node N uses base+N)
#   SMOKE_BASE_RPC     base RPC port (default 19944; node N uses base+N)
#   SMOKE_TMPROOT      where to put per-node home dirs (default /tmp/multinode-smoke)
#   SMOKE_LOG_DIR      where to write logs (default <repo>/scripts/multinode_smoke_logs/)
#   SMOKE_LOG_LEVEL    RUST_LOG level (default info; debug is noisy)
#   FORCE_BUILD        if set, force a `cargo build` even if the binary exists
#
# Per-node HOME is overridden so each node gets its own ~/.commputer/ data dir
# without modifying src/node/src/config.rs. Genesis is auto-applied on first
# boot (see run_node in src/node/src/main.rs).

set -u
set -o pipefail

# ---------------------------------------------------------------------------
# Paths & defaults
# ---------------------------------------------------------------------------

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
# src/staging/ -> repo root is two levels up
REPO_ROOT="$( cd "${SCRIPT_DIR}/.." && pwd )"
SRC_DIR="${REPO_ROOT}/src"

SMOKE_NODES="${SMOKE_NODES:-3}"
SMOKE_DURATION="${SMOKE_DURATION:-60}"
SMOKE_BASE_P2P="${SMOKE_BASE_P2P:-19000}"
SMOKE_BASE_RPC="${SMOKE_BASE_RPC:-19944}"
SMOKE_TMPROOT="${SMOKE_TMPROOT:-/tmp/multinode-smoke}"
SMOKE_LOG_DIR="${SMOKE_LOG_DIR:-${SCRIPT_DIR}/multinode_smoke_logs}"
SMOKE_LOG_LEVEL="${SMOKE_LOG_LEVEL:-info}"

if [[ "${SMOKE_NODES}" -lt 2 || "${SMOKE_NODES}" -gt 5 ]]; then
    echo "[smoke] SMOKE_NODES must be in [2, 5], got ${SMOKE_NODES}" >&2
    exit 1
fi

NODE_BIN="${SRC_DIR}/target/debug/commputer"

mkdir -p "${SMOKE_LOG_DIR}"

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

if [[ ! -x "${NODE_BIN}" || -n "${FORCE_BUILD:-}" ]]; then
    echo "[smoke] Building commputer (this can take a few minutes)..."
    if ! ( cd "${SRC_DIR}" && cargo build -p commputer --bin commputer ); then
        echo "[smoke] BUILD FAILED" >&2
        exit 3
    fi
fi

if [[ ! -x "${NODE_BIN}" ]]; then
    echo "[smoke] Node binary not found at ${NODE_BIN}" >&2
    exit 3
fi

echo "[smoke] Using binary: ${NODE_BIN}"
"${NODE_BIN}" version || true

# ---------------------------------------------------------------------------
# Cleanup trap (kill children, do not delete log dir)
# ---------------------------------------------------------------------------

declare -a NODE_PIDS=()
declare -a NODE_NAMES=()

cleanup() {
    local rc=$?
    echo
    echo "[smoke] Shutting down nodes..."
    for i in "${!NODE_PIDS[@]}"; do
        local pid="${NODE_PIDS[$i]}"
        local name="${NODE_NAMES[$i]}"
        if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
            echo "[smoke]   killing ${name} (pid ${pid})"
            kill -TERM "${pid}" 2>/dev/null || true
        fi
    done
    # Give them a moment, then force.
    sleep 2
    for pid in "${NODE_PIDS[@]}"; do
        if kill -0 "${pid}" 2>/dev/null; then
            kill -KILL "${pid}" 2>/dev/null || true
        fi
    done
    wait 2>/dev/null || true
    return $rc
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# Wipe per-node home dirs and start nodes
# ---------------------------------------------------------------------------

# Build the seed list (every node gets every other node as a seed).
# Format is /ip4/127.0.0.1/tcp/<P2P_PORT>; libp2p resolves peer ID via identify.
declare -a P2P_PORTS=()
declare -a RPC_PORTS=()
for ((n = 1; n <= SMOKE_NODES; n++)); do
    P2P_PORTS+=("$((SMOKE_BASE_P2P + n))")
    RPC_PORTS+=("$((SMOKE_BASE_RPC + n))")
done

start_node() {
    local idx="$1"
    local name="node${idx}"
    local p2p_port="${P2P_PORTS[$((idx - 1))]}"
    local rpc_port="${RPC_PORTS[$((idx - 1))]}"
    local home_dir="${SMOKE_TMPROOT}-${idx}"

    # Bootstrap-leader topology: node 1 starts with NO --seeds (becomes the
    # bootstrap leader and produces the first block solo, including the
    # ValidatorRegister txs from the mempool). Nodes 2..N seed only from node 1.
    # This is required because the protocol's leader-election gate in
    # handle_block_tick only allows a node WITHOUT --seeds to produce the
    # first block when fewer than 2 validators have been registered on-chain.
    # Any node started with --seeds defers to the bootstrap leader.
    local seeds=""
    if [[ "${idx}" -gt 1 ]]; then
        local peer_port="${P2P_PORTS[0]}"
        seeds="/ip4/127.0.0.1/tcp/${peer_port}"
    fi

    echo "[smoke] Wiping ${home_dir}"
    rm -rf "${home_dir}"
    mkdir -p "${home_dir}"

    local log_file="${SMOKE_LOG_DIR}/${name}.log"
    : > "${log_file}"

    echo "[smoke] Starting ${name}: p2p=${p2p_port} rpc=${rpc_port} HOME=${home_dir}"
    if [[ -n "${seeds}" ]]; then
        echo "[smoke]   seeds=${seeds}"
    else
        echo "[smoke]   seeds=(none — bootstrap leader)"
    fi
    echo "[smoke]   log=${log_file}"

    # Build node argv. Omit --seeds entirely for the bootstrap leader so the
    # node knows it's seed-less (is_seed_connector=false → eligible to produce
    # the first block solo).
    local seeds_args=()
    if [[ -n "${seeds}" ]]; then
        seeds_args=(--seeds "${seeds}")
    fi

    HOME="${home_dir}" \
    COMMPUTER_WALLET_PASSWORD="smoke" \
    RUST_LOG="${SMOKE_LOG_LEVEL}" \
        "${NODE_BIN}" run \
            --testnet \
            --port "${p2p_port}" \
            --rpc-port "${rpc_port}" \
            --rpc-bind 127.0.0.1 \
            "${seeds_args[@]}" \
            --password "smoke" \
            --log-level "${SMOKE_LOG_LEVEL}" \
            >>"${log_file}" 2>&1 &
    local pid=$!
    NODE_PIDS+=("${pid}")
    NODE_NAMES+=("${name}")
}

for ((n = 1; n <= SMOKE_NODES; n++)); do
    start_node "${n}"
    # Stagger boots a bit so seeds in earlier-started nodes are reachable.
    sleep 1
done

# ---------------------------------------------------------------------------
# Wait for each RPC to come up
# ---------------------------------------------------------------------------

wait_for_rpc() {
    local idx="$1"
    local rpc_port="${RPC_PORTS[$((idx - 1))]}"
    local name="${NODE_NAMES[$((idx - 1))]}"
    local pid="${NODE_PIDS[$((idx - 1))]}"
    local deadline=$(( $(date +%s) + 30 ))
    while (( $(date +%s) < deadline )); do
        if ! kill -0 "${pid}" 2>/dev/null; then
            echo "[smoke] ${name} died before RPC came up — see ${SMOKE_LOG_DIR}/${name}.log"
            return 1
        fi
        if curl -fsS --max-time 1 "http://127.0.0.1:${rpc_port}/status" >/dev/null 2>&1; then
            echo "[smoke] ${name} RPC up on :${rpc_port}"
            return 0
        fi
        sleep 1
    done
    echo "[smoke] ${name} RPC never came up on :${rpc_port}"
    return 1
}

any_rpc_failed=0
for ((n = 1; n <= SMOKE_NODES; n++)); do
    if ! wait_for_rpc "${n}"; then
        any_rpc_failed=1
    fi
done

if [[ "${any_rpc_failed}" -ne 0 ]]; then
    echo "[smoke] One or more nodes failed health checks. Tailing logs:"
    for ((n = 1; n <= SMOKE_NODES; n++)); do
        echo "--- ${NODE_NAMES[$((n - 1))]} (last 40 lines) ---"
        tail -n 40 "${SMOKE_LOG_DIR}/${NODE_NAMES[$((n - 1))]}.log" || true
    done
    exit 4
fi

# ---------------------------------------------------------------------------
# Run for SMOKE_DURATION (or until killed), then orderly shutdown
# ---------------------------------------------------------------------------

echo "[smoke] All ${SMOKE_NODES} nodes are up."
echo "[smoke] Logs: ${SMOKE_LOG_DIR}/"
for ((n = 1; n <= SMOKE_NODES; n++)); do
    echo "[smoke]   ${NODE_NAMES[$((n - 1))]} -> http://127.0.0.1:${RPC_PORTS[$((n - 1))]}"
done

if [[ "${SMOKE_DURATION}" -le 0 ]]; then
    echo "[smoke] SMOKE_DURATION=0 — running until Ctrl-C"
    while :; do
        sleep 5
        # Surface early death.
        for i in "${!NODE_PIDS[@]}"; do
            if ! kill -0 "${NODE_PIDS[$i]}" 2>/dev/null; then
                echo "[smoke] ${NODE_NAMES[$i]} exited unexpectedly"
                exit 2
            fi
        done
    done
else
    echo "[smoke] Letting nodes run for ${SMOKE_DURATION}s..."
    deadline=$(( $(date +%s) + SMOKE_DURATION ))
    rc=0
    while (( $(date +%s) < deadline )); do
        sleep 2
        for i in "${!NODE_PIDS[@]}"; do
            if ! kill -0 "${NODE_PIDS[$i]}" 2>/dev/null; then
                echo "[smoke] ${NODE_NAMES[$i]} exited early"
                rc=2
                deadline=0
                break
            fi
        done
    done
    echo "[smoke] Run window elapsed."

    # Snapshot status from each RPC for the log.
    for ((n = 1; n <= SMOKE_NODES; n++)); do
        rpc_port="${RPC_PORTS[$((n - 1))]}"
        echo "--- ${NODE_NAMES[$((n - 1))]} status ---"
        curl -fsS --max-time 2 "http://127.0.0.1:${rpc_port}/status" || echo "(no response)"
        echo
        echo "--- ${NODE_NAMES[$((n - 1))]} peers ---"
        curl -fsS --max-time 2 "http://127.0.0.1:${rpc_port}/peers" || echo "(no response)"
        echo
    done

    exit "${rc}"
fi
