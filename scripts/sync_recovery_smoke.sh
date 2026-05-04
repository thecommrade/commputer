#!/usr/bin/env bash
# sync_recovery_smoke.sh — validate that a killed node can rejoin its old chain.
#
# Different from multinode_smoke.sh's lazy-join (which tests a fresh node
# joining a running chain). This tests crash recovery: a node with partial
# block history is killed, then restarted with the SAME data dir while the
# rest of the network keeps producing blocks. The restarted node should pick
# up from its stored tip and sync the gap.
#
# Topology: 3 nodes. Node 1 is bootstrap leader (no --seeds). Nodes 2 and 3
# seed from node 1. Node 2 gets killed mid-run and restarted.
#
# Usage:
#   bash scripts/sync_recovery_smoke.sh
#
# Knobs (env):
#   PHASE1   seconds to run all 3 nodes before killing node 2 (default 60)
#   GAP      seconds to wait after kill before restart (default 30)
#   PHASE2   seconds to run after restart, while watching node 2 catch up (default 60)

set -u
set -o pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
REPO_ROOT="$( cd "${SCRIPT_DIR}/.." && pwd )"
SRC_DIR="${REPO_ROOT}/src"
NODE_BIN="${SRC_DIR}/target/debug/commputer"

PHASE1="${PHASE1:-60}"
GAP="${GAP:-30}"
PHASE2="${PHASE2:-60}"

LOG_DIR="${SCRIPT_DIR}/sync_recovery_logs"
mkdir -p "${LOG_DIR}"

if [[ ! -x "${NODE_BIN}" ]]; then
    echo "[recov] Building commputer..."
    ( cd "${SRC_DIR}" && cargo build -p commputer --bin commputer ) || {
        echo "[recov] BUILD FAILED" >&2
        exit 3
    }
fi

# Per-node setup: ports, home dirs, log files
declare -A P2P=([1]=19101 [2]=19102 [3]=19103)
declare -A RPC=([1]=19981 [2]=19982 [3]=19983)
declare -A HOME_DIR=([1]=/tmp/sync-recov-1 [2]=/tmp/sync-recov-2 [3]=/tmp/sync-recov-3)
declare -A PID

NODE1_SEED="/ip4/127.0.0.1/tcp/${P2P[1]}"

start_node() {
    local idx="$1"
    local fresh="$2"  # "fresh" or "resume"
    local name="node${idx}"
    local log="${LOG_DIR}/${name}.log"

    if [[ "${fresh}" == "fresh" ]]; then
        echo "[recov] Wiping ${HOME_DIR[$idx]} (fresh start)"
        rm -rf "${HOME_DIR[$idx]}"
        : > "${log}"  # truncate log on fresh start
    else
        echo "[recov] Preserving ${HOME_DIR[$idx]} (resume)"
        # Append to existing log so we have continuity
    fi
    mkdir -p "${HOME_DIR[$idx]}"

    local seeds_args=()
    if [[ "${idx}" -gt 1 ]]; then
        seeds_args=(--seeds "${NODE1_SEED}")
    fi

    HOME="${HOME_DIR[$idx]}" \
    COMMPUTER_WALLET_PASSWORD="recov" \
    RUST_LOG="info" \
        "${NODE_BIN}" run \
            --testnet \
            --port "${P2P[$idx]}" \
            --rpc-port "${RPC[$idx]}" \
            --rpc-bind 127.0.0.1 \
            "${seeds_args[@]}" \
            --password "recov" \
            --log-level "info" \
            >>"${log}" 2>&1 &
    PID[$idx]=$!
    echo "[recov] Started ${name} pid=${PID[$idx]} p2p=${P2P[$idx]} rpc=${RPC[$idx]} home=${HOME_DIR[$idx]}"
}

shutdown_all() {
    echo "[recov] Shutting down all nodes..."
    for idx in 1 2 3; do
        if [[ -n "${PID[$idx]:-}" ]] && kill -0 "${PID[$idx]}" 2>/dev/null; then
            echo "[recov]   kill node${idx} pid=${PID[$idx]}"
            kill "${PID[$idx]}" 2>/dev/null || true
        fi
    done
    sleep 2
    for idx in 1 2 3; do
        if [[ -n "${PID[$idx]:-}" ]] && kill -0 "${PID[$idx]}" 2>/dev/null; then
            kill -9 "${PID[$idx]}" 2>/dev/null || true
        fi
    done
}
trap shutdown_all EXIT INT TERM

height_of() {
    local idx="$1"
    curl -sf --max-time 2 "http://127.0.0.1:${RPC[$idx]}/status" 2>/dev/null \
        | grep -oE '"height":[0-9]+' | head -1 | cut -d: -f2
}

# ---------------------------------------------------------------------------
# Phase 0: start all 3 nodes fresh
# ---------------------------------------------------------------------------
echo
echo "[recov] === Phase 0: cold start ==="
start_node 1 fresh
sleep 1
start_node 2 fresh
sleep 1
start_node 3 fresh

echo "[recov] Waiting 5s for RPC servers..."
sleep 5

# ---------------------------------------------------------------------------
# Phase 1: produce blocks for PHASE1 seconds
# ---------------------------------------------------------------------------
echo
echo "[recov] === Phase 1: produce blocks for ${PHASE1}s ==="
sleep "${PHASE1}"
for i in 1 2 3; do
    echo "[recov]   node${i} height: $(height_of $i)"
done

# ---------------------------------------------------------------------------
# Phase 2: kill node 2, run nodes 1+3 alone for GAP seconds
# ---------------------------------------------------------------------------
echo
echo "[recov] === Phase 2: kill node2 ==="
NODE2_HEIGHT_AT_KILL=$(height_of 2)
echo "[recov]   node2 height at kill: ${NODE2_HEIGHT_AT_KILL}"
kill "${PID[2]}" 2>/dev/null || true
PID[2]=""
sleep 2

echo "[recov] Running nodes 1+3 alone for ${GAP}s (chain advances without node2)..."
sleep "${GAP}"
echo "[recov]   node1 height: $(height_of 1)"
echo "[recov]   node3 height: $(height_of 3)"

# ---------------------------------------------------------------------------
# Phase 3: restart node 2 with same data dir, verify it catches up
# ---------------------------------------------------------------------------
echo
echo "[recov] === Phase 3: restart node2 with same data dir ==="
start_node 2 resume
echo "[recov] Waiting 5s for RPC..."
sleep 5
echo "[recov]   node2 height immediately after restart: $(height_of 2)"

echo "[recov] Letting all 3 run for ${PHASE2}s — node2 should sync the gap..."
sleep "${PHASE2}"
echo
echo "[recov] === Final heights ==="
for i in 1 2 3; do
    echo "[recov]   node${i}: $(height_of $i)"
done

echo
echo "[recov] Done. See ${LOG_DIR}/ for full logs. Look for:"
echo "[recov]   grep 'Sync: applied block' ${LOG_DIR}/node2.log  # gap-sync evidence"
echo "[recov]   grep 'crash\\|recover\\|Resumed' ${LOG_DIR}/node2.log  # crash recovery messages"
