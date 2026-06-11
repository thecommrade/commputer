#!/usr/bin/env bash
# multinode_assert.sh — CI-grade PASS/FAIL wrapper around multinode_smoke.sh.
#
# The smoke harness proves the nodes COME UP and their RPC responds, but it does
# NOT assert that the chain actually made progress or that the nodes agree. This
# wrapper runs the smoke harness, then asserts on the per-node logs that real
# multi-node consensus happened:
#
#   1. every node reached height >= ASSERT_MIN_HEIGHT
#   2. the node heights agree within ASSERT_HEIGHT_TOL of each other
#   3. total "Snowball finalized" events across nodes >= ASSERT_MIN_FINALIZED
#   4. zero panics in any node log
#
# Exit codes:
#   0  — PASS (all assertions held)
#   1  — FAIL (an assertion failed; details printed)
#   2  — the smoke harness itself failed to run / a node died (propagated)
#
# All SMOKE_* env knobs from multinode_smoke.sh pass straight through. Assertion
# thresholds are calibrated to the observed local cadence (~7s/block on a debug
# build: a 90s run reaches height ~13). Override per environment.
#
# Knobs (with defaults):
#   ASSERT_MIN_HEIGHT     min height EVERY node must reach        (default 8)
#   ASSERT_HEIGHT_TOL     max allowed spread between node heights (default 2)
#   ASSERT_MIN_FINALIZED  min total "Snowball finalized" events   (default 12)
#
# Example:
#   SMOKE_NODES=3 SMOKE_DURATION=90 bash scripts/multinode_assert.sh

set -u
set -o pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
SMOKE="${SCRIPT_DIR}/multinode_smoke.sh"
LOG_DIR="${SMOKE_LOG_DIR:-${SCRIPT_DIR}/multinode_smoke_logs}"

ASSERT_MIN_HEIGHT="${ASSERT_MIN_HEIGHT:-8}"
ASSERT_HEIGHT_TOL="${ASSERT_HEIGHT_TOL:-2}"
ASSERT_MIN_FINALIZED="${ASSERT_MIN_FINALIZED:-12}"
SMOKE_NODES="${SMOKE_NODES:-3}"

if [[ ! -f "${SMOKE}" ]]; then
    echo "[assert] cannot find ${SMOKE}" >&2
    exit 2
fi

echo "[assert] Running smoke harness (${SMOKE_NODES} nodes)..."
# Export so the child sees the same node count / log dir.
export SMOKE_NODES SMOKE_LOG_DIR="${LOG_DIR}"
if ! bash "${SMOKE}"; then
    echo "[assert] FAIL: smoke harness exited non-zero (a node died or failed to come up)" >&2
    exit 2
fi

echo
echo "[assert] Smoke run finished. Checking assertions against ${LOG_DIR}/ ..."

fail=0
declare -a HEIGHTS=()

# ---- per-node: final height + panic scan -----------------------------------
# Each finalization logs: "Chain: height=N, circulating=..., ...".
for ((n = 1; n <= SMOKE_NODES; n++)); do
    log="${LOG_DIR}/node${n}.log"
    if [[ ! -f "${log}" ]]; then
        echo "[assert]   node${n}: FAIL — log not found at ${log}"
        fail=1
        continue
    fi

    # Final observed height (last "Chain: height=N" line).
    h="$(grep -oE 'Chain: height=[0-9]+' "${log}" 2>/dev/null | tail -1 | grep -oE '[0-9]+')"
    h="${h:-0}"
    HEIGHTS+=("${h}")

    # Panic scan (a real thread panic, not the word "panic" in an INFO line).
    panics="$(grep -ciE "thread '.*' panicked|panicked at|RUST_BACKTRACE" "${log}" 2>/dev/null || true)"

    status="ok"
    if (( h < ASSERT_MIN_HEIGHT )); then
        status="FAIL (height ${h} < min ${ASSERT_MIN_HEIGHT})"
        fail=1
    fi
    if (( panics > 0 )); then
        status="${status}; FAIL (${panics} panic line(s))"
        fail=1
    fi
    echo "[assert]   node${n}: height=${h} panics=${panics} -> ${status}"
done

# ---- cross-node agreement (height spread) ----------------------------------
if (( ${#HEIGHTS[@]} > 0 )); then
    max=0; min=999999999
    for h in "${HEIGHTS[@]}"; do
        (( h > max )) && max="${h}"
        (( h < min )) && min="${h}"
    done
    spread=$(( max - min ))
    if (( spread > ASSERT_HEIGHT_TOL )); then
        echo "[assert]   agreement: FAIL — height spread ${spread} > tol ${ASSERT_HEIGHT_TOL} (min=${min} max=${max})"
        fail=1
    else
        echo "[assert]   agreement: ok — spread ${spread} <= tol ${ASSERT_HEIGHT_TOL} (min=${min} max=${max})"
    fi
fi

# ---- finalization count (across all nodes) ---------------------------------
finalized=0
for ((n = 1; n <= SMOKE_NODES; n++)); do
    c="$(grep -c 'Snowball finalized' "${LOG_DIR}/node${n}.log" 2>/dev/null || echo 0)"
    finalized=$(( finalized + c ))
done
if (( finalized < ASSERT_MIN_FINALIZED )); then
    echo "[assert]   finalizations: FAIL — ${finalized} total < min ${ASSERT_MIN_FINALIZED}"
    fail=1
else
    echo "[assert]   finalizations: ok — ${finalized} total >= min ${ASSERT_MIN_FINALIZED}"
fi

echo
if (( fail == 0 )); then
    echo "[assert] PASS — multi-node consensus advanced and agreed."
    exit 0
else
    echo "[assert] FAIL — see assertions above; per-node logs in ${LOG_DIR}/"
    exit 1
fi
