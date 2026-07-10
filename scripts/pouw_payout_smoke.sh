#!/usr/bin/env bash
# pouw_payout_smoke.sh — LIVE PoUW pay-out smoke harness for Commputer.
#
# This is the ONE acceptance proof that cannot run inside `cargo test`: a REAL
# multi-node loopback network where a submitted compute job flows through the
# live executor + verifier actor loops and the real libp2p DA transport, settles
# on-chain, and ACTUALLY PAYS the executor (85%) and the committee verifiers
# (10%/k), burning 5% and consuming the submitter's budget. The in-process
# equivalent is src/node/tests/pouw_payout_e2e.rs; this harness proves the same
# money-path over the network the way an operator will actually run it.
#
# It is OPERATOR-RUN. It boots N+1 real `commputer` node processes, mines them up,
# bonds the validators, funds a submitter, POSTs a job, waits for it to settle
# (tens of seconds to a few minutes), and asserts the pay-out with a clear
# PASS/FAIL. See scripts/pouw_payout_smoke.README.md for the full runbook.
#
# NON-PROTECTED: this is a NEW file. It touches no protected source, no genesis,
# and no frozen crate; it drives the node purely through its CLI + JSON RPC.
#
# ─────────────────────────────────────────────────────────────────────────────
# WHY the deltas are clean despite block-reward mining income:
#   Every node earns ~15.855 COMME per block IT produces (credited to `balance`
#   AND, in lockstep, to `total_mined`). So for any account,
#       payout_delta = (balance_after - balance_before)
#                    - (total_mined_after - total_mined_before)
#   cancels ALL mining income exactly, leaving only the PoUW pay-out (+ negligible
#   per-tx fee noise). That is how a tiny (0.85 * budget) pay-out is measured on
#   top of a balance that is churning from mining.
#
# WHAT IT ASSERTS (all four ⇒ a Confirmed pay-out):
#   1. Exactly one validator's payout_delta ≈ worker_share  (0.85 * budget)   [executor]
#   2. ≥2 OTHER validators' payout_delta > 0, summing ≈ verifier_pool (0.10*budget) [committee]
#   3. Submitter's payout_delta ≈ -(budget + fee)  — budget CONSUMED, not refunded
#   4. Chain `burned` rose ≈ 0.05 * budget          — the Confirmed 5% burn signature
#
# Exit codes: 0 PASS · 1 FAIL (assertion) · 2 node/build/setup failure · 3 timeout
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail

# ---------------------------------------------------------------------------
# Paths & config knobs
# ---------------------------------------------------------------------------
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
REPO_ROOT="$( cd "${SCRIPT_DIR}/.." && pwd )"
SRC_DIR="${REPO_ROOT}/src"
NODE_BIN="${SRC_DIR}/target/debug/commputer"

# Topology / economics (all overridable).
SMOKE_NODES="${SMOKE_NODES:-4}"                 # bonded VALIDATOR nodes (executor + committee candidates)
SMOKE_BUDGET_COMME="${SMOKE_BUDGET_COMME:-10}"  # job budget in whole COMME (>=1; /submit_job floor is 1 COMME)
SMOKE_BOND_COMME="${SMOKE_BOND_COMME:-2}"       # per-validator bond (>> min_bond 0.00001 COMME; leaves budget headroom)
SMOKE_PASSWORD="${SMOKE_PASSWORD:-smoke}"
SMOKE_RPC_KEY="${SMOKE_RPC_KEY:-smoke-admin-key}"  # gates the /submit_job (admin) tier

SMOKE_BASE_P2P="${SMOKE_BASE_P2P:-19000}"
SMOKE_BASE_RPC="${SMOKE_BASE_RPC:-19944}"
SMOKE_TMPROOT="${SMOKE_TMPROOT:-/tmp/pouw-payout-smoke}"
SMOKE_LOG_DIR="${SMOKE_LOG_DIR:-${SCRIPT_DIR}/pouw_payout_smoke_logs}"
SMOKE_LOG_LEVEL="${SMOKE_LOG_LEVEL:-info}"

# Timeouts (seconds) and block budgets. Debug builds run ~5-8s/block.
WARMUP_MIN_HEIGHT="${WARMUP_MIN_HEIGHT:-3}"     # chain must reach this + agree before we proceed
FUND_TIMEOUT="${FUND_TIMEOUT:-420}"             # wait for every node (+submitter) to mine enough to bond/submit
BOND_SETTLE_BLOCKS="${BOND_SETTLE_BLOCKS:-6}"   # blocks to wait after bonding so the Bond txs apply (eligibility)
MAX_SETTLE_BLOCKS="${MAX_SETTLE_BLOCKS:-50}"    # submit_height + this ⇒ the lifecycle MUST have settled (claim10+phases30+margin)
SETTLE_TIMEOUT="${SETTLE_TIMEOUT:-900}"         # hard wall-clock cap on the settle wait
RPC_UP_TIMEOUT="${RPC_UP_TIMEOUT:-40}"

DRY_RUN="${DRY_RUN:-0}"                          # 1 = validate config + embedded constants + tool plumbing, start NO nodes
FORCE_BUILD="${FORCE_BUILD:-}"

# ---------------------------------------------------------------------------
# Consensus-anchored constants (genesis defaults; see src/core/src/genesis.rs,
# src/core/src/token.rs, src/staging/pouw/src/params.rs). Used for the asserts.
# ---------------------------------------------------------------------------
UNITS_PER_COMME=100000000        # token.rs UNITS_PER_COMME (1 COMME = 1e8 raw)
MINIMUM_FEE=100000               # transaction.rs MINIMUM_FEE (0.001 COMME)
WORKER_BPS=8500                  # GameParams worker share
VERIFIER_BPS=1000                # GameParams verifier pool
BURN_BPS=500                     # GameParams burn share
COMMITTEE_K=3                    # GameParams k (committee target; quorum = 2)

# Embedded default program + input (see README "How program_hex was produced").
# DOUBLER guest from src/node/tests/pouw_payout_e2e.rs, compiled with wat 1.251.0.
# program sha256 = 570471e71188e17bffaf66d6abbf85e9d73cca26d91df1a3e41dbe9a71a0d7c5
DEFAULT_PROGRAM_HEX="0061736d01000000010c0260017f017f60027f7f017e03030200010504010101010607017f014180080b071803066d656d6f7279020005616c6c6f6300000372756e00010a51021101017f23002101230020006a240020010b3d01027f20011000210202400340200320014f0d01200220036a4102200020036a2d00006c3a0000200341016a21030c000b0b2002ad4220862001ad840b004c046e616d650108010005616c6c6f63022102000200036c656e01037074720104000370747201036c656e02036f7574030169030f0101020004646f6e6501046c6f6f7007070100046e657874"
DEFAULT_INPUT_HEX="0102032807"   # [1,2,3,40,7]

SMOKE_PROGRAM_HEX="${SMOKE_PROGRAM_HEX:-$DEFAULT_PROGRAM_HEX}"
SMOKE_INPUT_HEX="${SMOKE_INPUT_HEX:-$DEFAULT_INPUT_HEX}"

# ---------------------------------------------------------------------------
# Logging helpers
# ---------------------------------------------------------------------------
log()  { echo "[payout] $*"; }
err()  { echo "[payout] ERROR: $*" >&2; }
die()  { err "$1"; exit "${2:-2}"; }

# ---------------------------------------------------------------------------
# Derived economics (raw units)
# ---------------------------------------------------------------------------
BUDGET_RAW=$(( SMOKE_BUDGET_COMME * UNITS_PER_COMME ))
BOND_RAW=$(( SMOKE_BOND_COMME * UNITS_PER_COMME ))
WORKER_SHARE=$(( BUDGET_RAW * WORKER_BPS / 10000 ))
VERIFIER_POOL=$(( BUDGET_RAW * VERIFIER_BPS / 10000 ))
BURN_EXPECTED=$(( BUDGET_RAW * BURN_BPS / 10000 ))
# A validator must be able to post e_bond = budget at claim AND keep its own bond,
# so fund every validator to at least bond + budget + margin before we proceed.
NODE_FUND_TARGET=$(( BOND_RAW + BUDGET_RAW + UNITS_PER_COMME ))       # + ~1 COMME margin
SUBMITTER_FUND_TARGET=$(( BUDGET_RAW + UNITS_PER_COMME ))             # budget + ~1 COMME (covers fee)

# Assertion tolerances (raw). Mining is subtracted exactly, so tolerances only
# absorb per-tx fee noise + rounding; kept a few % of budget with a fixed floor.
FEE_FLOOR=$(( 20 * MINIMUM_FEE ))                                     # ~0.02 COMME fee slack
EXEC_TOL=$(( BUDGET_RAW * 5 / 100 + FEE_FLOOR ))                      # executor share ±5%
VPOOL_TOL=$(( VERIFIER_POOL * 25 / 100 + FEE_FLOOR ))                 # verifier pool sum ±25%
VERIF_MIN=$(( VERIFIER_POOL / (COMMITTEE_K + 1) ))                    # a paid verifier clears this (robust to k=2 or 3)
SUB_TOL=$(( BUDGET_RAW * 2 / 100 + FEE_FLOOR ))                       # submitter drop ±2%
BURN_TOL=$(( BURN_EXPECTED * 30 / 100 + FEE_FLOOR ))                  # burn ±30% (rounding remainder folds into burn)

# ---------------------------------------------------------------------------
# Node bookkeeping (indices 1..N validators, N+1 = submitter)
# ---------------------------------------------------------------------------
SUBMITTER_IDX=$(( SMOKE_NODES + 1 ))
declare -a NODE_PIDS=()
declare -a NODE_NAMES=()
declare -a NODE_ADDR=()   # 1-indexed by node index; NODE_ADDR[i] = hex address
SUBMITTER_SEED=""
SUBMITTER_ADDR=""

p2p_port() { echo $(( SMOKE_BASE_P2P + $1 )); }
rpc_port() { echo $(( SMOKE_BASE_RPC + $1 )); }
home_dir() { echo "${SMOKE_TMPROOT}-$1"; }
NODE1_RPC=$(( SMOKE_BASE_RPC + 1 ))   # canonical read view + /submit_job target

# ---------------------------------------------------------------------------
# Cleanup trap — kill every child node on ANY exit.
# ---------------------------------------------------------------------------
cleanup() {
    local rc=$?
    echo
    log "Tearing down nodes..."
    local pid
    for pid in "${NODE_PIDS[@]:-}"; do
        [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null && kill -TERM "${pid}" 2>/dev/null || true
    done
    sleep 2
    for pid in "${NODE_PIDS[@]:-}"; do
        [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null && kill -KILL "${pid}" 2>/dev/null || true
    done
    wait 2>/dev/null || true
    return $rc
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# RPC helpers (public tier — no key needed except /submit_job)
# ---------------------------------------------------------------------------
# GET a URL, print body (empty on failure).
rpc_get() { curl -fsS --max-time 5 "$1" 2>/dev/null || true; }

# jq extraction with a default when the key is absent / body is an error.
jqf() { # jqf <json> <filter> <default>
    local out
    out="$(printf '%s' "$1" | jq -r "$2" 2>/dev/null || true)"
    if [[ -z "${out}" || "${out}" == "null" ]]; then printf '%s' "$3"; else printf '%s' "${out}"; fi
}

status_field() { # status_field <rpc_port> <jq-key> <default>
    local body; body="$(rpc_get "http://127.0.0.1:$1/status")"
    jqf "${body}" ".$2" "$3"
}
get_height() { status_field "$1" height 0; }
get_burned() { status_field "$1" burned 0; }

# Read an account's balance / total_mined (raw) from a given RPC (0 if not on chain).
acct_field() { # acct_field <rpc_port> <addr_hex> <field>
    local body; body="$(rpc_get "http://127.0.0.1:$1/account/$2")"
    jqf "${body}" ".$3" 0
}
get_balance() { acct_field "$1" "$2" balance; }
get_mined()   { acct_field "$1" "$2" total_mined; }

# ---------------------------------------------------------------------------
# Preflight: tools, binary, config sanity
# ---------------------------------------------------------------------------
preflight() {
    command -v curl >/dev/null 2>&1 || die "curl is required" 2
    command -v jq   >/dev/null 2>&1 || die "jq is required (parses RPC JSON)" 2
    command -v cargo >/dev/null 2>&1 || die "cargo is required to build the node" 2

    [[ "${SMOKE_NODES}" -ge 3 ]] || die "SMOKE_NODES must be >= 3 (executor + committee); 4+ recommended" 2
    [[ "${SMOKE_BUDGET_COMME}" -ge 1 ]] || die "SMOKE_BUDGET_COMME must be >= 1 (/submit_job floor is 1 COMME)" 2

    if [[ "${SMOKE_NODES}" -lt 4 ]]; then
        log "WARNING: SMOKE_NODES=${SMOKE_NODES}. The executor is excluded from its own committee, so"
        log "         only $(( SMOKE_NODES - 1 )) verifier candidates remain (k=${COMMITTEE_K}, quorum=2). With 3 nodes a"
        log "         SINGLE straggler ⇒ NoQuorum ⇒ refund ⇒ FAIL. Use 4+ for a robust pay-out."
    fi

    # Validate the embedded program hex decodes and is non-empty.
    [[ "${SMOKE_PROGRAM_HEX}" =~ ^([0-9a-fA-F]{2})+$ ]] || die "SMOKE_PROGRAM_HEX is not valid hex" 2
    [[ "${SMOKE_INPUT_HEX}"   =~ ^([0-9a-fA-F]{2})*$ ]] || die "SMOKE_INPUT_HEX is not valid hex" 2

    mkdir -p "${SMOKE_LOG_DIR}"
}

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
build_node() {
    if [[ ! -x "${NODE_BIN}" || -n "${FORCE_BUILD}" ]]; then
        log "Building commputer (this can take a few minutes)..."
        ( cd "${SRC_DIR}" && cargo build -p commputer --bin commputer ) \
            || die "build failed" 2
    fi
    [[ -x "${NODE_BIN}" ]] || die "node binary not found at ${NODE_BIN}" 2
    log "Using binary: ${NODE_BIN}"
}

# ---------------------------------------------------------------------------
# Wallet pre-creation — deterministic address (+ seed for the submitter) with NO
# running node (so no RocksDB lock) and NO log parsing. The node loads this exact
# wallet on boot (main.rs: wallet_file.exists() ⇒ Keystore::load).
# ---------------------------------------------------------------------------
precreate_wallet() { # precreate_wallet <idx>  -> sets NODE_ADDR[idx]; sets SUBMITTER_* for the submitter idx
    local idx="$1" home; home="$(home_dir "${idx}")"
    rm -rf "${home}"; mkdir -p "${home}"
    local out
    out="$( HOME="${home}" COMMPUTER_WALLET_PASSWORD="${SMOKE_PASSWORD}" \
            "${NODE_BIN}" wallet create --testnet 2>&1 )" \
        || die "wallet create failed for node ${idx}:\n${out}" 2
    # "  Full:    <64 hex>"
    local addr; addr="$(printf '%s\n' "${out}" | grep -oiE '[0-9a-f]{64}' | head -1)"
    [[ -n "${addr}" ]] || die "could not parse address for node ${idx}" 2
    NODE_ADDR["${idx}"]="${addr}"
    if [[ "${idx}" -eq "${SUBMITTER_IDX}" ]]; then
        SUBMITTER_ADDR="${addr}"
        # Seed = the numbered word list "    NN. word" ⇒ single space-joined phrase.
        SUBMITTER_SEED="$(printf '%s\n' "${out}" \
            | grep -E '^[[:space:]]*[0-9]+\.[[:space:]]' \
            | sed -E 's/^[[:space:]]*[0-9]+\.[[:space:]]+//' \
            | tr '\n' ' ' | sed -E 's/[[:space:]]+$//')"
        local wc; wc="$(printf '%s' "${SUBMITTER_SEED}" | wc -w)"
        [[ "${wc}" -ge 12 ]] || die "could not parse submitter seed phrase (got ${wc} words)" 2
    fi
}

# ---------------------------------------------------------------------------
# Start one node. Validators (1..N) + submitter (N+1). Bootstrap-leader topology:
# node 1 has NO --seeds; everyone else seeds node 1.
# ---------------------------------------------------------------------------
start_node() { # start_node <idx>
    local idx="$1" name="node${1}"
    [[ "${idx}" -eq "${SUBMITTER_IDX}" ]] && name="submitter"
    local pp; pp="$(p2p_port "${idx}")"
    local rp; rp="$(rpc_port "${idx}")"
    local home; home="$(home_dir "${idx}")"
    local log_file="${SMOKE_LOG_DIR}/${name}.log"; : > "${log_file}"

    local seeds_args=()
    if [[ "${idx}" -ne 1 ]]; then
        seeds_args=(--seeds "/ip4/127.0.0.1/tcp/$(p2p_port 1)")
    fi

    log "Starting ${name}: p2p=${pp} rpc=${rp} addr=${NODE_ADDR[$idx]:0:12}... log=${log_file}"
    HOME="${home}" \
    COMMPUTER_WALLET_PASSWORD="${SMOKE_PASSWORD}" \
    RUST_LOG="${SMOKE_LOG_LEVEL}" \
        "${NODE_BIN}" run \
            --testnet \
            --port "${pp}" \
            --rpc-port "${rp}" \
            --rpc-bind 127.0.0.1 \
            --rpc-key "${SMOKE_RPC_KEY}" \
            --password "${SMOKE_PASSWORD}" \
            --log-level "${SMOKE_LOG_LEVEL}" \
            "${seeds_args[@]}" \
            >>"${log_file}" 2>&1 &
    NODE_PIDS+=("$!")
    NODE_NAMES+=("${name}")
}

wait_rpc_up() { # wait_rpc_up <idx>
    local idx="$1" rp; rp="$(rpc_port "${idx}")"
    local deadline=$(( $(date +%s) + RPC_UP_TIMEOUT ))
    while (( $(date +%s) < deadline )); do
        if curl -fsS --max-time 1 "http://127.0.0.1:${rp}/status" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}

# ---------------------------------------------------------------------------
# Wait for the chain to produce + agree (reuse the multinode_assert idea via RPC).
# ---------------------------------------------------------------------------
wait_chain_progress() {
    local total=$(( SMOKE_NODES + 1 ))
    local deadline=$(( $(date +%s) + FUND_TIMEOUT ))
    log "Waiting for chain to reach height >= ${WARMUP_MIN_HEIGHT} on all ${total} nodes..."
    while (( $(date +%s) < deadline )); do
        local ok=1 minh=999999999 maxh=0 i h
        for (( i=1; i<=total; i++ )); do
            h="$(get_height "$(rpc_port "${i}")")"
            (( h < minh )) && minh="${h}"
            (( h > maxh )) && maxh="${h}"
            (( h < WARMUP_MIN_HEIGHT )) && ok=0
        done
        if (( ok == 1 )); then
            log "Chain producing: heights min=${minh} max=${maxh} (spread $(( maxh - minh )))"
            return 0
        fi
        sleep 3
    done
    return 1
}

# ---------------------------------------------------------------------------
# Fund gate — wait until every validator has >= NODE_FUND_TARGET and the
# submitter has >= SUBMITTER_FUND_TARGET (self-funded via mining). Read all
# balances from node1's consistent view.
# ---------------------------------------------------------------------------
wait_funded() {
    local deadline=$(( $(date +%s) + FUND_TIMEOUT ))
    log "Waiting for nodes to mine funds (validators >= ${NODE_FUND_TARGET} raw, submitter >= ${SUBMITTER_FUND_TARGET} raw)..."
    while (( $(date +%s) < deadline )); do
        local all=1 i bal
        for (( i=1; i<=SMOKE_NODES; i++ )); do
            bal="$(get_balance "${NODE1_RPC}" "${NODE_ADDR[$i]}")"
            (( bal < NODE_FUND_TARGET )) && all=0
        done
        bal="$(get_balance "${NODE1_RPC}" "${SUBMITTER_ADDR}")"
        (( bal < SUBMITTER_FUND_TARGET )) && all=0
        if (( all == 1 )); then
            log "All nodes funded."
            return 0
        fi
        sleep 4
    done
    return 1
}

# ---------------------------------------------------------------------------
# Bond every validator (>= min_bond, generously). cmd_bond signs with the node's
# local wallet + broadcasts to /tx; it reads the nonce over RPC (never touches the
# node's locked DB). Non-zero exit ⇒ the tx was rejected.
# ---------------------------------------------------------------------------
bond_validators() {
    local i home rp
    for (( i=1; i<=SMOKE_NODES; i++ )); do
        home="$(home_dir "${i}")"; rp="$(rpc_port "${i}")"
        log "Bonding node${i} ${SMOKE_BOND_COMME} COMME..."
        HOME="${home}" COMMPUTER_WALLET_PASSWORD="${SMOKE_PASSWORD}" \
            "${NODE_BIN}" bond "${SMOKE_BOND_COMME}" --rpc-port "${rp}" >>"${SMOKE_LOG_DIR}/bond.log" 2>&1 \
            || die "bond rejected for node${i} (see ${SMOKE_LOG_DIR}/bond.log)" 2
    done
    # Wait for the Bond txs to be included + applied so the nodes become eligible.
    local start_h; start_h="$(get_height "${NODE1_RPC}")"
    local target=$(( start_h + BOND_SETTLE_BLOCKS ))
    log "Waiting ${BOND_SETTLE_BLOCKS} blocks (height ${start_h} -> ${target}) for bonds to apply..."
    local deadline=$(( $(date +%s) + 180 ))
    while (( $(date +%s) < deadline )); do
        (( $(get_height "${NODE1_RPC}") >= target )) && { log "Bonds should be applied."; return 0; }
        sleep 3
    done
    return 1
}

# ---------------------------------------------------------------------------
# Snapshot every account's (balance,total_mined) from node1 + chain burned.
# Writes into the named arrays passed by the caller.
# ---------------------------------------------------------------------------
declare -a BAL_BEFORE=() MINED_BEFORE=() BAL_AFTER=() MINED_AFTER=()
BURN_BEFORE=0 BURN_AFTER=0 SUB_BAL_BEFORE=0 SUB_MINED_BEFORE=0 SUB_BAL_AFTER=0 SUB_MINED_AFTER=0

snapshot_before() {
    local i
    for (( i=1; i<=SMOKE_NODES; i++ )); do
        BAL_BEFORE[$i]="$(get_balance "${NODE1_RPC}" "${NODE_ADDR[$i]}")"
        MINED_BEFORE[$i]="$(get_mined "${NODE1_RPC}" "${NODE_ADDR[$i]}")"
    done
    SUB_BAL_BEFORE="$(get_balance "${NODE1_RPC}" "${SUBMITTER_ADDR}")"
    SUB_MINED_BEFORE="$(get_mined "${NODE1_RPC}" "${SUBMITTER_ADDR}")"
    BURN_BEFORE="$(get_burned "${NODE1_RPC}")"
}
snapshot_after() {
    local i
    for (( i=1; i<=SMOKE_NODES; i++ )); do
        BAL_AFTER[$i]="$(get_balance "${NODE1_RPC}" "${NODE_ADDR[$i]}")"
        MINED_AFTER[$i]="$(get_mined "${NODE1_RPC}" "${NODE_ADDR[$i]}")"
    done
    SUB_BAL_AFTER="$(get_balance "${NODE1_RPC}" "${SUBMITTER_ADDR}")"
    SUB_MINED_AFTER="$(get_mined "${NODE1_RPC}" "${SUBMITTER_ADDR}")"
    BURN_AFTER="$(get_burned "${NODE1_RPC}")"
}

# ---------------------------------------------------------------------------
# Submit the job (KEYED /submit_job tier ⇒ X-API-Key header). Returns 0 on accept.
# ---------------------------------------------------------------------------
SUBMIT_HEIGHT=0 DA_ROOT="" TX_HASH=""
submit_job() {
    SUBMIT_HEIGHT="$(get_height "${NODE1_RPC}")"
    local body resp
    body="$(jq -cn \
        --arg p "${SMOKE_PROGRAM_HEX}" --arg in "${SMOKE_INPUT_HEX}" \
        --argjson b "${BUDGET_RAW}" --arg s "${SUBMITTER_SEED}" \
        '{program_hex:$p, input_hex:$in, budget:$b, submitter_seed:$s}')"
    log "POST /submit_job -> node1 (:${NODE1_RPC})  budget=${BUDGET_RAW} raw  submit_height=${SUBMIT_HEIGHT}"
    # NOTE: no -f here — on a 4xx we WANT the JSON error body (captured + logged),
    # not an empty curl failure.
    resp="$(curl -sS --max-time 30 -X POST \
        -H "Content-Type: application/json" \
        -H "X-API-Key: ${SMOKE_RPC_KEY}" \
        --data "${body}" \
        "http://127.0.0.1:${NODE1_RPC}/submit_job" 2>>"${SMOKE_LOG_DIR}/submit.log" || true)"
    printf '%s\n' "${resp}" >>"${SMOKE_LOG_DIR}/submit.log"
    local accepted; accepted="$(jqf "${resp}" '.accepted' false)"
    [[ "${accepted}" == "true" ]] || { err "submit_job not accepted: ${resp}"; return 1; }
    DA_ROOT="$(jqf "${resp}" '.da_root' '')"
    TX_HASH="$(jqf "${resp}" '.tx_hash' '')"
    log "Accepted. da_root=${DA_ROOT:0:16}... tx_hash=${TX_HASH:0:16}..."
    return 0
}

# ---------------------------------------------------------------------------
# Wait for the job to settle. Early-exit once `burned` jumps (Confirmed 5% burn);
# otherwise bound by SUBMIT_HEIGHT + MAX_SETTLE_BLOCKS and SETTLE_TIMEOUT.
# ---------------------------------------------------------------------------
wait_settled() {
    local target=$(( SUBMIT_HEIGHT + MAX_SETTLE_BLOCKS ))
    local deadline=$(( $(date +%s) + SETTLE_TIMEOUT ))
    local burn_trigger=$(( BURN_EXPECTED / 2 ))
    log "Waiting for settlement (burn jump, or height >= ${target}, timeout ${SETTLE_TIMEOUT}s)..."
    while (( $(date +%s) < deadline )); do
        local h b
        h="$(get_height "${NODE1_RPC}")"
        b="$(get_burned "${NODE1_RPC}")"
        if (( b - BURN_BEFORE >= burn_trigger )); then
            log "Burn jumped (Δburned=$(( b - BURN_BEFORE )) raw) at height ${h} — settlement detected."
            sleep 4   # let the settlement block's balances propagate to node1's snapshot
            return 0
        fi
        if (( h >= target )); then
            log "Reached height ${h} >= ${target} without a burn jump (likely a refund/no-payout)."
            return 0   # proceed to asserts; they will FAIL honestly with the deltas
        fi
        sleep 5
    done
    err "settle wait timed out after ${SETTLE_TIMEOUT}s (height $(get_height "${NODE1_RPC}"), submit ${SUBMIT_HEIGHT})"
    return 3
}

# ---------------------------------------------------------------------------
# Assertions
# ---------------------------------------------------------------------------
abs() { local v="$1"; (( v < 0 )) && v=$(( -v )); echo "${v}"; }
near() { # near <value> <target> <tol>  -> 0 if |value-target| <= tol
    local d; d="$(abs $(( $1 - $2 )))"; (( d <= $3 )); }
comme() { # pretty raw->COMME
    local raw="$1" sign=""
    (( raw < 0 )) && { sign="-"; raw=$(( -raw )); }
    printf '%s%d.%08d' "${sign}" "$(( raw / UNITS_PER_COMME ))" "$(( raw % UNITS_PER_COMME ))"
}

run_assertions() {
    echo
    log "──────────────── PAY-OUT LEDGER (payout_delta = Δbalance − Δtotal_mined) ────────────────"
    log "budget=${BUDGET_RAW} ($(comme ${BUDGET_RAW}) COMME)  worker_share=$(comme ${WORKER_SHARE})  verifier_pool=$(comme ${VERIFIER_POOL})  burn=$(comme ${BURN_EXPECTED})"

    local i dpay exec_count=0 exec_idx=0 verif_count=0 verif_sum=0
    for (( i=1; i<=SMOKE_NODES; i++ )); do
        dpay=$(( (BAL_AFTER[i] - BAL_BEFORE[i]) - (MINED_AFTER[i] - MINED_BEFORE[i]) ))
        local role="-"
        if near "${dpay}" "${WORKER_SHARE}" "${EXEC_TOL}"; then
            role="EXECUTOR (85%)"; exec_count=$(( exec_count + 1 )); exec_idx="${i}"
        elif (( dpay >= VERIF_MIN )); then
            role="verifier"; verif_count=$(( verif_count + 1 )); verif_sum=$(( verif_sum + dpay ))
        fi
        log "  node${i} ${NODE_ADDR[$i]:0:12}..  payout_delta=$(comme ${dpay}) COMME  ${role}"
    done

    local sub_dpay=$(( (SUB_BAL_AFTER - SUB_BAL_BEFORE) - (SUB_MINED_AFTER - SUB_MINED_BEFORE) ))
    local dburn=$(( BURN_AFTER - BURN_BEFORE ))
    log "  submitter ${SUBMITTER_ADDR:0:12}..  payout_delta=$(comme ${sub_dpay}) COMME (expect ~ -$(comme ${BUDGET_RAW}))"
    log "  chain burned Δ = $(comme ${dburn}) COMME (expect ~ $(comme ${BURN_EXPECTED}))"
    log "──────────────────────────────────────────────────────────────────────────────────────"

    # ---- the four assertions ----
    local fail=0
    if (( exec_count == 1 )); then
        log "PASS  [1/4] executor: node${exec_idx} received worker_share ≈ 0.85*budget"
    elif (( exec_count > 1 )); then
        err "FAIL  [1/4] executor: ${exec_count} nodes match worker_share (expected exactly 1)"; fail=1
    else
        err "FAIL  [1/4] executor: no node received ≈ worker_share ($(comme ${WORKER_SHARE}) COMME ± $(comme ${EXEC_TOL}))"; fail=1
    fi

    if (( verif_count >= 2 )) && near "${verif_sum}" "${VERIFIER_POOL}" "${VPOOL_TOL}"; then
        log "PASS  [2/4] committee: ${verif_count} verifiers paid, sum=$(comme ${verif_sum}) ≈ verifier_pool"
    else
        err "FAIL  [2/4] committee: ${verif_count} verifiers (need >=2), sum=$(comme ${verif_sum}) vs pool $(comme ${VERIFIER_POOL}) ± $(comme ${VPOOL_TOL})"; fail=1
    fi

    if near "${sub_dpay}" $(( -BUDGET_RAW - MINIMUM_FEE )) "${SUB_TOL}"; then
        log "PASS  [3/4] submitter: budget CONSUMED (dropped ≈ budget, NOT refunded)"
    else
        err "FAIL  [3/4] submitter: drop $(comme ${sub_dpay}) != -budget (a refund ⇒ NoQuorum/Timeout/Disputed, not a pay-out)"; fail=1
    fi

    if near "${dburn}" "${BURN_EXPECTED}" "${BURN_TOL}"; then
        log "PASS  [4/4] burn: chain burned ≈ 0.05*budget (Confirmed signature)"
    else
        err "FAIL  [4/4] burn: Δburned=$(comme ${dburn}) != ≈0.05*budget $(comme ${BURN_EXPECTED}) ± $(comme ${BURN_TOL})"; fail=1
    fi

    echo
    if (( fail == 0 )); then
        log "================  PASS — the job PAID the executor + committee on a live network  ================"
        return 0
    else
        err "================  FAIL — see the four checks + the ledger above  ================"
        err "Logs: ${SMOKE_LOG_DIR}/  (node*.log, submitter.log, submit.log, bond.log)"
        return 1
    fi
}

# ---------------------------------------------------------------------------
# DRY RUN — validate config, embedded constants, jq/curl plumbing WITHOUT nodes.
# ---------------------------------------------------------------------------
dry_run() {
    log "DRY RUN — no nodes will be started."
    preflight
    log "config: SMOKE_NODES=${SMOKE_NODES} budget=${SMOKE_BUDGET_COMME} COMME bond=${SMOKE_BOND_COMME} COMME rpc_key=set"
    log "economics(raw): budget=${BUDGET_RAW} worker=${WORKER_SHARE} verifier_pool=${VERIFIER_POOL} burn=${BURN_EXPECTED}"
    log "tolerances(raw): exec=${EXEC_TOL} vpool=${VPOOL_TOL} verif_min=${VERIF_MIN} sub=${SUB_TOL} burn=${BURN_TOL}"
    log "fund targets(raw): node=${NODE_FUND_TARGET} submitter=${SUBMITTER_FUND_TARGET}"
    local plen; plen=$(( ${#SMOKE_PROGRAM_HEX} / 2 ))
    log "program_hex: ${plen} bytes, input_hex: $(( ${#SMOKE_INPUT_HEX} / 2 )) bytes"
    # Exercise the exact jq body-builder used by submit_job.
    local body
    body="$(jq -cn --arg p "${SMOKE_PROGRAM_HEX}" --arg in "${SMOKE_INPUT_HEX}" \
        --argjson b "${BUDGET_RAW}" --arg s "twelve word test seed phrase only used for dry run body build check here now" \
        '{program_hex:$p, input_hex:$in, budget:$b, submitter_seed:$s}')" \
        || die "jq body-builder failed" 2
    printf '%s' "${body}" | jq -e '.budget and .program_hex and .input_hex and .submitter_seed' >/dev/null \
        || die "submit_job body missing a required field" 2
    log "submit_job body builds OK ($(printf '%s' "${body}" | wc -c) bytes)."
    # Exercise the arithmetic/format helpers.
    near 100 100 0 && log "near(): OK"
    log "comme(${WORKER_SHARE}) = $(comme ${WORKER_SHARE}) COMME"
    log "comme(-${BUDGET_RAW}) = $(comme $(( -BUDGET_RAW ))) COMME"
    log "DRY RUN OK — arg parsing, embedded program, jq/curl plumbing all valid."
}

# ===========================================================================
# MAIN
# ===========================================================================
main() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        dry_run
        exit 0
    fi

    preflight
    build_node

    log "Pre-creating ${SMOKE_NODES} validator wallets + 1 submitter wallet..."
    local i
    for (( i=1; i<=SMOKE_NODES; i++ )); do precreate_wallet "${i}"; done
    precreate_wallet "${SUBMITTER_IDX}"
    log "Submitter ${SUBMITTER_ADDR:0:12}.. (unbonded, self-funding, excluded from the game)."

    log "Starting ${SMOKE_NODES} validators + submitter (bootstrap-leader topology)..."
    for (( i=1; i<=SMOKE_NODES; i++ )); do start_node "${i}"; sleep 1; done
    start_node "${SUBMITTER_IDX}"; sleep 1

    for (( i=1; i<=SUBMITTER_IDX; i++ )); do
        wait_rpc_up "${i}" || die "node ${i} RPC never came up (see ${SMOKE_LOG_DIR}/)" 2
    done
    log "All ${SUBMITTER_IDX} node RPCs are up."

    wait_chain_progress || die "chain never reached height ${WARMUP_MIN_HEIGHT} — see logs" 2
    wait_funded         || die "nodes never mined enough to bond/submit within ${FUND_TIMEOUT}s" 2
    bond_validators     || die "bonds never applied" 2

    snapshot_before
    log "BEFORE captured (burned=${BURN_BEFORE} raw)."

    submit_job || die "job submission failed (see ${SMOKE_LOG_DIR}/submit.log)" 2

    wait_settled || die "job did not settle in time" 3
    snapshot_after
    log "AFTER captured (burned=${BURN_AFTER} raw)."

    run_assertions
}

main "$@"
