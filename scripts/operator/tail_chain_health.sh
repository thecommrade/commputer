#!/usr/bin/env bash
# tail_chain_health.sh — post-launch lockstep monitor for the multi-machine
# testnet ceremony.
#
# Polls each node's RPC every 10 seconds for height, peer_count, and
# validator_status; reports lockstep PASS/FAIL across all three nodes.
# Designed to be run by all three operators side-by-side during the
# ceremony so divergence is caught within one polling cycle.
#
# Usage:
#   ./tail_chain_health.sh <rpc_url_1> <rpc_url_2> <rpc_url_3>
#   ./tail_chain_health.sh http://node1:9944 http://node2:9944 http://node3:9944
#
# Optional env vars:
#   POLL_INTERVAL  — seconds between polls (default 10)
#   LOCKSTEP_HEIGHT_TOLERANCE — max abs height delta tolerated (default 1)
#   API_KEY        — sent as X-API-Key header if set (auth-protected RPCs)
#
# Output format (one block per poll cycle):
#   [HH:MM:SS UTC] node1: h=N peers=K validator=yes  node2: h=N peers=K ... lockstep: OK
#   [HH:MM:SS UTC] node1: h=N peers=K validator=yes  node2: h=M peers=K ... lockstep: DRIFT (delta=M-N)
#
# Exits cleanly on Ctrl-C. Never exits non-zero by itself — the OPERATOR
# decides when to stop.

set -u
set -o pipefail

if (( $# < 1 )); then
    echo "Usage: $0 <rpc_url_1> [<rpc_url_2> ...]" >&2
    exit 2
fi

POLL_INTERVAL="${POLL_INTERVAL:-10}"
LOCKSTEP_HEIGHT_TOLERANCE="${LOCKSTEP_HEIGHT_TOLERANCE:-1}"

NODES=("$@")
NUM_NODES="${#NODES[@]}"

# Cleanup trap (so Ctrl-C doesn't leave the terminal in a weird state).
trap 'echo; echo "[tail] stopped at $(date -u +%H:%M:%S)"; exit 0' INT TERM

# Build curl args (optional auth header).
declare -a CURL_ARGS=(--silent --fail --max-time 4 --show-error)
if [[ -n "${API_KEY:-}" ]]; then
    CURL_ARGS+=(-H "X-API-Key: ${API_KEY}")
fi

# Pull a single field from /status. Uses jq if available; falls back to grep.
status_field() {
    local rpc_url="$1"
    local field="$2"
    local resp
    resp="$(curl "${CURL_ARGS[@]}" "${rpc_url}/status" 2>/dev/null || true)"
    if [[ -z "${resp}" ]]; then
        echo "?"
        return
    fi
    if command -v jq >/dev/null 2>&1; then
        echo "${resp}" | jq -r ".${field} // \"?\""
    else
        # Best-effort: extract field as JSON value.
        echo "${resp}" | grep -oE "\"${field}\"[[:space:]]*:[[:space:]]*[0-9]+" | head -1 | grep -oE '[0-9]+' || echo "?"
    fi
}

# Pull peer count via /peers (length of the array).
peer_count() {
    local rpc_url="$1"
    local resp
    resp="$(curl "${CURL_ARGS[@]}" "${rpc_url}/peers" 2>/dev/null || true)"
    if [[ -z "${resp}" ]]; then
        echo "?"
        return
    fi
    if command -v jq >/dev/null 2>&1; then
        echo "${resp}" | jq -r 'length // 0'
    else
        # Crude count of "peer_id" occurrences.
        echo "${resp}" | grep -oE '"peer_id"' | wc -l | awk '{print $1}'
    fi
}

# Validator presence — query /validators count.
validator_count() {
    local rpc_url="$1"
    local resp
    resp="$(curl "${CURL_ARGS[@]}" "${rpc_url}/validators" 2>/dev/null || true)"
    if [[ -z "${resp}" ]]; then
        echo "?"
        return
    fi
    if command -v jq >/dev/null 2>&1; then
        echo "${resp}" | jq -r '.count // 0'
    else
        echo "${resp}" | grep -oE '"count"[[:space:]]*:[[:space:]]*[0-9]+' | head -1 | grep -oE '[0-9]+' || echo "?"
    fi
}

# Quick reachability test.
echo "[tail] starting monitor; ${NUM_NODES} node(s); polling every ${POLL_INTERVAL}s"
for url in "${NODES[@]}"; do
    if curl "${CURL_ARGS[@]}" "${url}/status" >/dev/null 2>&1; then
        echo "[tail]   ${url}  (RPC reachable)"
    else
        echo "[tail]   ${url}  (RPC NOT REACHABLE — will retry on each poll)"
    fi
done
echo "[tail] press Ctrl-C to stop"
echo

# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------

while true; do
    declare -a HEIGHTS=()
    declare -a PEERS=()
    declare -a VALIDATOR_COUNTS=()

    for url in "${NODES[@]}"; do
        h="$(status_field "${url}" "height")"
        p="$(peer_count "${url}")"
        v="$(validator_count "${url}")"
        HEIGHTS+=("${h}")
        PEERS+=("${p}")
        VALIDATOR_COUNTS+=("${v}")
    done

    # Lockstep check: max - min over numeric heights must be <= tolerance.
    max_h=-1
    min_h=99999999999
    any_unknown=0
    for h in "${HEIGHTS[@]}"; do
        if [[ "${h}" == "?" ]]; then
            any_unknown=1
            continue
        fi
        if (( h > max_h )); then max_h=${h}; fi
        if (( h < min_h )); then min_h=${h}; fi
    done

    if (( any_unknown == 1 )); then
        lockstep="UNKNOWN (one or more RPCs unreachable)"
    elif (( max_h == -1 )); then
        lockstep="UNKNOWN (no responding node)"
    elif (( max_h - min_h <= LOCKSTEP_HEIGHT_TOLERANCE )); then
        lockstep="OK (delta=$((max_h - min_h)))"
    else
        lockstep="DRIFT (delta=$((max_h - min_h)))"
    fi

    # Format one line per node.
    timestamp="$(date -u +%H:%M:%S)"
    line="[${timestamp} UTC]"
    for i in "${!NODES[@]}"; do
        line+=" node$((i+1)): h=${HEIGHTS[$i]} peers=${PEERS[$i]} validators=${VALIDATOR_COUNTS[$i]}"
    done
    line+=" | lockstep: ${lockstep}"
    echo "${line}"

    sleep "${POLL_INTERVAL}"
done
