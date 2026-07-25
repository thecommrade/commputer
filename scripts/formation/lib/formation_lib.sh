#!/usr/bin/env bash
# formation_lib.sh — primitives for the Commputer formation-test harness.
#
# Sourced by every scenario under scripts/formation/scenarios/. Provides node
# lifecycle (cold/warm boot, kill, restart), RPC probes (height, fingerprint,
# peers), log scraping, and the assertion verbs.
#
# NETWORK ISOLATION (load-bearing): a local node dials the compiled-in public
# seed (seed.commputer.xyz) at boot AND on every keepalive tick, and a cold
# local node derives the SAME genesis hash and chain-id as the public alpha —
# so an unisolated harness run would JOIN THE LIVE NETWORK. run_formation.sh
# therefore re-execs the whole harness inside an unprivileged network
# namespace (`unshare -r -n`, loopback only). Scenarios must never be run
# outside that wrapper; assert_isolated() enforces it at scenario start.
#
# Exit-code contract (matches multinode_assert.sh):
#   0 pass · 1 assertion failed · 2 infrastructure failure

set -u
set -o pipefail

FORMATION_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FORMATION_ROOT="$(cd "${FORMATION_LIB_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${FORMATION_ROOT}/../.." && pwd)"
SRC_DIR="${REPO_ROOT}/src"

# Default to the FORMATION build (target/formation, built with --features
# formation-test), never the deploy binary at target/release. Scenarios and
# ad-hoc diagnostics both source this file directly, and pointing at
# target/release silently tests a stale, pin-enabled binary — which looks
# exactly like "only the bootstrap node ever produces".
NODE_BIN="${FORMATION_NODE_BIN:-${SRC_DIR}/target/formation/release/commputer}"
BASE_P2P="${FORMATION_BASE_P2P:-19100}"   # offset from multinode_smoke's 19000
BASE_RPC="${FORMATION_BASE_RPC:-19144}"   # offset from multinode_smoke's 19944
TMPROOT="${FORMATION_TMPROOT:-/tmp/formation}"
LOG_DIR="${FORMATION_LOG_DIR:-${FORMATION_ROOT}/logs}"
LOG_LEVEL="${FORMATION_LOG_LEVEL:-info}"
WALLET_PASS="formation"

SCENARIO_NAME="${SCENARIO_NAME:-unnamed}"
declare -A NODE_PID=()
declare -A NODE_SEEDS=()
FAILURES=0

mkdir -p "${LOG_DIR}"

# --------------------------------------------------------------------------
# output helpers
# --------------------------------------------------------------------------
_ts() { date +%H:%M:%S; }
log()  { echo "[$(_ts)][${SCENARIO_NAME}] $*"; }
pass() { echo "[$(_ts)][${SCENARIO_NAME}] PASS: $*"; }
fail() { echo "[$(_ts)][${SCENARIO_NAME}] FAIL: $*" >&2; FAILURES=$((FAILURES + 1)); }
infra() { echo "[$(_ts)][${SCENARIO_NAME}] INFRA: $*" >&2; teardown_all; exit 2; }

p2p_port() { echo $((BASE_P2P + $1)); }
rpc_port() { echo $((BASE_RPC + $1)); }
home_dir() { echo "${TMPROOT}-${SCENARIO_NAME}-$1"; }
log_file() { echo "${LOG_DIR}/${SCENARIO_NAME}-node$1.log"; }

# --------------------------------------------------------------------------
# isolation guard
# --------------------------------------------------------------------------

# Refuse to run outside the network namespace: an unisolated run would dial the
# live public seed and merge test nodes into the real chain.
assert_isolated() {
    # A missing or wrong binary is the difference between testing the change
    # you just made and testing last week's; fail loudly rather than silently.
    [[ -x "${NODE_BIN}" ]] || {
        echo "REFUSING TO RUN: no harness binary at ${NODE_BIN}" >&2
        echo "Build it: cd src && CARGO_TARGET_DIR=target/formation cargo build --release -p commputer --bin commputer --features formation-test" >&2
        exit 2
    }
    log "binary: ${NODE_BIN} (built $(date -r "${NODE_BIN}" '+%H:%M:%S'))"
    if [[ "${FORMATION_ISOLATED:-0}" != "1" ]]; then
        echo "REFUSING TO RUN: not inside the formation network namespace." >&2
        echo "Run via scripts/formation/run_formation.sh (it re-execs under 'unshare -r -n')." >&2
        exit 2
    fi
    # Belt and braces: prove the public seed is genuinely unreachable.
    if curl -s --max-time 2 "http://174.138.35.16:9000" >/dev/null 2>&1; then
        echo "REFUSING TO RUN: public seed IP is reachable from inside the namespace." >&2
        exit 2
    fi
}

# --------------------------------------------------------------------------
# node lifecycle
# --------------------------------------------------------------------------

# boot_node <idx> <mode:cold|warm> [seed_idx ...]
# cold wipes the data dir (fresh genesis); warm preserves it (replays the tip).
# With no seed indices the node is seed-less: is_seed_connector=false, so it may
# bootstrap-produce block 1 (see handle_block_tick's bootstrap branch).
boot_node() {
    local idx="$1"; shift
    local mode="$1"; shift
    local home; home="$(home_dir "${idx}")"
    local logf; logf="$(log_file "${idx}")"

    case "${mode}" in
        cold) rm -rf "${home}"; mkdir -p "${home}"; : > "${logf}" ;;
        warm) [[ -d "${home}" ]] || infra "warm boot of node${idx} with no data dir" ;;
        *) infra "boot_node: mode must be cold|warm, got '${mode}'" ;;
    esac

    local seeds_csv=""
    for s in "$@"; do
        local sp; sp="$(p2p_port "${s}")"
        [[ -n "${seeds_csv}" ]] && seeds_csv+=","
        seeds_csv+="/ip4/127.0.0.1/tcp/${sp}"
    done
    NODE_SEEDS[$idx]="${seeds_csv}"

    local seeds_args=()
    [[ -n "${seeds_csv}" ]] && seeds_args=(--seeds "${seeds_csv}")

    HOME="${home}" \
    COMMPUTER_WALLET_PASSWORD="${WALLET_PASS}" \
    RUST_LOG="${LOG_LEVEL}" \
        "${NODE_BIN}" run \
            --testnet \
            --port "$(p2p_port "${idx}")" \
            --rpc-port "$(rpc_port "${idx}")" \
            --rpc-bind 127.0.0.1 \
            "${seeds_args[@]}" \
            --password "${WALLET_PASS}" \
            --log-level "${LOG_LEVEL}" \
            >>"${logf}" 2>&1 &
    NODE_PID[$idx]=$!
    # Detach from job control so teardown does not spray "Killed" job messages
    # over the scenario output; liveness is tracked with kill -0, not wait.
    disown "${NODE_PID[$idx]}" 2>/dev/null || true
    log "node${idx} ${mode} boot pid=${NODE_PID[$idx]} p2p=$(p2p_port "${idx}") rpc=$(rpc_port "${idx}") seeds=${seeds_csv:-none}"
}

# restart_node <idx> <mode> — reuses the seed set the node was booted with.
restart_node() {
    local idx="$1"; local mode="${2:-warm}"
    local seeds_csv="${NODE_SEEDS[$idx]:-}"
    local -a seed_idxs=()
    if [[ -n "${seeds_csv}" ]]; then
        IFS=',' read -ra parts <<< "${seeds_csv}"
        for p in "${parts[@]}"; do
            local port="${p##*/}"
            seed_idxs+=( $((port - BASE_P2P)) )
        done
    fi
    boot_node "${idx}" "${mode}" "${seed_idxs[@]}"
}

# kill_node <idx> [TERM|KILL]
kill_node() {
    local idx="$1"; local sig="${2:-KILL}"
    local pid="${NODE_PID[$idx]:-}"
    [[ -z "${pid}" ]] && return 0
    kill -"${sig}" "${pid}" 2>/dev/null || true
    local deadline=$(( $(date +%s) + 10 ))
    while kill -0 "${pid}" 2>/dev/null && (( $(date +%s) < deadline )); do sleep 0.2; done
    kill -KILL "${pid}" 2>/dev/null || true
    unset 'NODE_PID[$idx]'
    log "node${idx} killed (${sig})"
}

node_alive() { local pid="${NODE_PID[$1]:-}"; [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; }

teardown_all() {
    for idx in "${!NODE_PID[@]}"; do kill_node "${idx}" KILL; done
    wait 2>/dev/null || true
}
trap teardown_all EXIT INT TERM

# wait_rpc <idx> [timeout_s] — poll until /status answers.
wait_rpc() {
    local idx="$1"; local timeout="${2:-45}"
    local port; port="$(rpc_port "${idx}")"
    local deadline=$(( $(date +%s) + timeout ))
    while (( $(date +%s) < deadline )); do
        node_alive "${idx}" || { infra "node${idx} died before RPC came up (see $(log_file "${idx}"))"; }
        if curl -fsS --max-time 1 "http://127.0.0.1:${port}/status" >/dev/null 2>&1; then
            log "node${idx} RPC up"
            return 0
        fi
        sleep 0.5
    done
    infra "node${idx} RPC never came up on :${port}"
}

# --------------------------------------------------------------------------
# probes
# --------------------------------------------------------------------------

get_height() {
    local idx="$1"
    curl -fsS --max-time 2 "http://127.0.0.1:$(rpc_port "${idx}")/status" 2>/dev/null \
        | jq -r '.height // empty' 2>/dev/null
}

get_peers() {
    local idx="$1"
    curl -fsS --max-time 2 "http://127.0.0.1:$(rpc_port "${idx}")/health" 2>/dev/null \
        | jq -r '.peers // empty' 2>/dev/null
}

# get_fingerprint <idx> <height> — the FORK ORACLE. Height agreement alone is
# what let a rubber-stamped divergence through: nodes can report identical
# heights while holding different chains. state_root+parent_hash are both
# hash-covered, so identical fingerprints at a height mean identical chains.
# NOTE: /block/{h} serves from a bounded recent-block cache — old heights
# return "Block not found". Fingerprint only near the tip.
get_fingerprint() {
    local idx="$1"; local h="$2"
    local body
    for _ in 1 2 3; do
        body="$(curl -fsS --max-time 2 "http://127.0.0.1:$(rpc_port "${idx}")/block/${h}" 2>/dev/null)"
        if [[ -n "${body}" ]] && ! grep -q '"error"' <<<"${body}"; then
            jq -cS '[.header.state_root, .header.parent_hash]' <<<"${body}" 2>/dev/null | sha256sum | cut -c1-16
            return 0
        fi
        sleep 0.4
    done
    echo "MISSING"
}

# scrape <idx> <extended-regex> — occurrence count in a node's log.
# grep -c prints 0 AND exits 1 on no-match, so the count must not be ORed with
# a fallback echo (that emits "0\n0" and breaks every arithmetic comparison).
scrape() {
    local f; f="$(log_file "$1")"
    [[ -r "${f}" ]] || { echo 0; return 0; }
    local n; n="$(grep -acE "$2" "${f}" 2>/dev/null)" || true
    echo "${n:-0}"
}

# --------------------------------------------------------------------------
# waits (deadline + retry, never a bare sleep on a condition)
# --------------------------------------------------------------------------

# wait_height <idx> <target> <timeout_s>
wait_height() {
    local idx="$1"; local target="$2"; local timeout="$3"
    local deadline=$(( $(date +%s) + timeout ))
    while (( $(date +%s) < deadline )); do
        local h; h="$(get_height "${idx}")"
        [[ -n "${h}" ]] && (( h >= target )) && return 0
        sleep 1
    done
    return 1
}

# wait_all_height <target> <timeout_s> <idx...>
wait_all_height() {
    local target="$1"; shift
    local timeout="$1"; shift
    local deadline=$(( $(date +%s) + timeout ))
    while (( $(date +%s) < deadline )); do
        local ok=1
        for idx in "$@"; do
            local h; h="$(get_height "${idx}")"
            [[ -z "${h}" ]] || (( h < target )) && { ok=0; break; }
        done
        (( ok )) && return 0
        sleep 1
    done
    return 1
}

heights_of() { for idx in "$@"; do printf '%s ' "$(get_height "${idx}" || echo '?')"; done; }

# --------------------------------------------------------------------------
# assertions
# --------------------------------------------------------------------------

# assert_progress <window_s> <min_gain> <idx...> — every node's height must
# climb by at least min_gain within the window.
assert_progress() {
    local window="$1"; shift
    local min_gain="$1"; shift
    local -A start=()
    for idx in "$@"; do start[$idx]="$(get_height "${idx}" || echo 0)"; done
    sleep "${window}"
    local ok=1
    for idx in "$@"; do
        local now; now="$(get_height "${idx}" || echo 0)"
        local gain=$(( ${now:-0} - ${start[$idx]:-0} ))
        if (( gain < min_gain )); then
            fail "node${idx} gained ${gain} blocks in ${window}s (want >= ${min_gain}): ${start[$idx]} -> ${now}"
            ok=0
        fi
    done
    (( ok )) && pass "all nodes progressed >= ${min_gain} in ${window}s ($(heights_of "$@"))"
    return $(( ! ok ))
}

# assert_converged <tolerance> <timeout_s> <idx...> — heights within tolerance
# AND identical fingerprints at a common recent height.
assert_converged() {
    local tol="$1"; shift
    local timeout="$1"; shift
    local deadline=$(( $(date +%s) + timeout ))
    local nodes=("$@")
    while (( $(date +%s) < deadline )); do
        local min=999999999 max=0 ok=1
        for idx in "${nodes[@]}"; do
            local h; h="$(get_height "${idx}")"
            [[ -z "${h}" ]] && { ok=0; break; }
            (( h < min )) && min=$h
            (( h > max )) && max=$h
        done
        if (( ok )) && (( max - min <= tol )) && (( min > 0 )); then
            # Fingerprint at a height every node holds, one back from the min
            # tip to dodge the apply/serve race.
            local probe=$(( min > 1 ? min - 1 : 1 ))
            local ref="" agree=1
            for idx in "${nodes[@]}"; do
                local fp; fp="$(get_fingerprint "${idx}" "${probe}")"
                [[ "${fp}" == "MISSING" ]] && { agree=0; break; }
                [[ -z "${ref}" ]] && ref="${fp}"
                [[ "${fp}" != "${ref}" ]] && { agree=0; break; }
            done
            if (( agree )); then
                pass "converged: spread $((max - min)) <= ${tol}, identical chain at h${probe} ($(heights_of "${nodes[@]}"))"
                return 0
            fi
        fi
        sleep 2
    done
    fail "did not converge within ${timeout}s (heights: $(heights_of "${nodes[@]}"))"
    return 1
}

# assert_no_private_fork <idx...> — pairwise fingerprint agreement at the
# common tip. Distinct chains at the same height = a fork.
assert_no_private_fork() {
    local nodes=("$@")
    local min=999999999
    for idx in "${nodes[@]}"; do
        local h; h="$(get_height "${idx}")"; [[ -z "${h}" ]] && continue
        (( h < min )) && min=$h
    done
    (( min == 999999999 || min < 2 )) && { log "no common height to compare — skipping fork check"; return 0; }
    local probe=$(( min - 1 ))
    local ref="" refidx=""
    for idx in "${nodes[@]}"; do
        local fp; fp="$(get_fingerprint "${idx}" "${probe}")"
        [[ "${fp}" == "MISSING" ]] && continue
        if [[ -z "${ref}" ]]; then ref="${fp}"; refidx="${idx}"; continue; fi
        if [[ "${fp}" != "${ref}" ]]; then
            fail "PRIVATE FORK at h${probe}: node${refidx}=${ref} vs node${idx}=${fp}"
            return 1
        fi
    done
    pass "no private fork at h${probe} (fingerprint ${ref})"
    return 0
}

# assert_no_runaway <tolerance> <idx...> — no node's height may exceed the
# applied-consensus height (the highest height a MAJORITY agree on) by more
# than tolerance. This is the soak-killer detector: a producer finalizing on
# rubber-stamp votes climbs while the appliers stand still.
assert_no_runaway() {
    local tol="$1"; shift
    local nodes=("$@")
    local -a hs=()
    for idx in "${nodes[@]}"; do hs+=( "$(get_height "${idx}" || echo 0)" ); done
    local sorted; sorted=$(printf '%s\n' "${hs[@]}" | sort -n)
    local n=${#nodes[@]}
    local majority_idx=$(( (n - 1) / 2 ))          # lower median = majority floor
    local consensus_h; consensus_h=$(sed -n "$((majority_idx + 1))p" <<<"${sorted}")
    local ok=1
    for i in "${!nodes[@]}"; do
        local over=$(( hs[i] - consensus_h ))
        if (( over > tol )); then
            fail "RUNAWAY: node${nodes[i]} at ${hs[i]} is ${over} above consensus height ${consensus_h} (tol ${tol})"
            ok=0
        fi
    done
    (( ok )) && pass "no runaway (consensus h=${consensus_h}, heights: ${hs[*]})"
    return $(( ! ok ))
}

# assert_height_monotonic <idx> <floor> — height must never drop below a known
# floor (guards the destructive-recovery blast radius).
assert_height_monotonic() {
    local idx="$1"; local floor="$2"
    local h; h="$(get_height "${idx}" || echo 0)"
    if (( ${h:-0} < floor )); then
        fail "node${idx} height ${h} dropped below floor ${floor} (destructive truncation)"
        return 1
    fi
    pass "node${idx} height ${h} >= floor ${floor}"
    return 0
}

# assert_no_panic <idx...>
assert_no_panic() {
    local ok=1
    for idx in "$@"; do
        local c; c="$(scrape "${idx}" "thread '.*' panicked|panicked at|RUST_BACKTRACE")"
        if (( c > 0 )); then fail "node${idx} PANICKED (${c} hits) — see $(log_file "${idx}")"; ok=0; fi
    done
    (( ok )) && pass "no panics"
    return $(( ! ok ))
}

# assert_log_count_lt <idx> <regex> <max> <label>
assert_log_count_lt() {
    local idx="$1"; local re="$2"; local max="$3"; local label="$4"
    local c; c="$(scrape "${idx}" "${re}")"
    if (( c >= max )); then fail "node${idx} ${label}: ${c} occurrences (want < ${max})"; return 1; fi
    pass "node${idx} ${label}: ${c} < ${max}"
    return 0
}

# scenario_result — final verdict; scenarios end with this.
scenario_result() {
    if (( FAILURES == 0 )); then
        log "SCENARIO PASSED"
        return 0
    fi
    log "SCENARIO FAILED (${FAILURES} assertion failure(s))"
    return 1
}
