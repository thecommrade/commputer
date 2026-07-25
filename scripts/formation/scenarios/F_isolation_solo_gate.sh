#!/usr/bin/env bash
# Zero-peer isolation — the solo-fork gate.
#
# An isolated node used to self-vote its own candidates to finalization every
# stall timeout, minting a private fork (observed four times live in one
# night). The gate now refuses solo finalization above the genesis-bootstrap
# window: an isolated node must IDLE, and must rejoin cleanly afterwards.

SCENARIO_NAME="F_isolation_solo_gate"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

boot_node 1 cold
wait_rpc 1
boot_node 2 cold 1
boot_node 3 cold 1
wait_rpc 2; wait_rpc 3

if ! wait_all_height 15 240 1 2 3; then
    fail "chain never reached h15 (heights: $(heights_of 1 2 3))"
    scenario_result; exit $?
fi
assert_converged 2 60 1 2 3

ISO_TIP="$(get_height 3)"
log "isolating node3 at height ${ISO_TIP} (restart pointed at a dead port)"
kill_node 3 KILL
# Warm restart with a seed that nothing is listening on: 0 peers, same data.
NODE_SEEDS[3]="/ip4/127.0.0.1/tcp/$((BASE_P2P + 99))"
HOME="$(home_dir 3)" \
COMMPUTER_WALLET_PASSWORD="${WALLET_PASS}" \
RUST_LOG="${LOG_LEVEL}" \
    "${NODE_BIN}" run --testnet \
        --port "$(p2p_port 3)" --rpc-port "$(rpc_port 3)" --rpc-bind 127.0.0.1 \
        --seeds "/ip4/127.0.0.1/tcp/$((BASE_P2P + 99))" \
        --password "${WALLET_PASS}" --log-level "${LOG_LEVEL}" \
        >>"$(log_file 3)" 2>&1 &
NODE_PID[3]=$!
wait_rpc 3

log "letting the isolated node sit for 120s while the pair produces..."
sleep 120

ISO_NOW="$(get_height 3)"
PEERS3="$(get_peers 3)"
log "isolated node3: height ${ISO_TIP} -> ${ISO_NOW}, peers=${PEERS3:-0}"

# A couple of blocks of in-flight slack is tolerable; sustained climbing at
# zero peers means it is minting a private chain.
if (( ISO_NOW > ISO_TIP + 2 )); then
    fail "SOLO FORK: isolated node climbed ${ISO_TIP} -> ${ISO_NOW} with ${PEERS3:-0} peers"
else
    pass "isolated node idled at ${ISO_NOW} (no solo minting)"
fi

# It must also not have wiped itself while alone.
assert_height_monotonic 3 $(( ISO_TIP - 2 ))
assert_progress 30 3 1 2      # the majority pair keeps going regardless
assert_no_panic 1 2 3

scenario_result
