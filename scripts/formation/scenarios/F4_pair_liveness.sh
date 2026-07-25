#!/usr/bin/env bash
# F4 — converged-pair liveness.
#
# Two nodes, converged, both Active, leadership alternating by sorted
# round-robin: the chain must keep climbing indefinitely. This reproduces the
# live "pair at 106 producing nothing" freeze, whose cause was
# last_block_seen_time starting at None: seconds_waiting computed 0 forever,
# so view-change never rotated and only the primary could ever produce.
# A freeze here is a regression of that fix.

SCENARIO_NAME="F4_pair_liveness"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

boot_node 1 cold
wait_rpc 1
boot_node 2 cold 1
wait_rpc 2

if ! wait_all_height 12 150 1 2; then
    fail "pair never reached height 12 (heights: $(heights_of 1 2))"
    scenario_result; exit $?
fi
assert_converged 2 60 1 2

# The freeze signature: both nodes alive, converged, and simply stop. Watch a
# long window — the live freeze survived minutes of waiting.
log "watching 120s for a sustained-production freeze..."
assert_progress 120 10 1 2

assert_converged 2 60 1 2
assert_no_private_fork 1 2
assert_no_panic 1 2

# Under a healthy leader the peer correctly DEFERS (a node does not propose
# when a candidate already exists at the next height and it has waited < 6s),
# so one-sided production is expected here — it is not a freeze. What must
# hold is FAILOVER: kill the producer and the survivor takes over.
log "production so far: node1=$(scrape 1 'Produced block candidate') node2=$(scrape 2 'Produced block candidate')"
TIP_BEFORE="$(get_height 2)"

# In a TWO-node network, killing one leaves the survivor with zero peers, and
# a zero-peer node must NOT produce — that is the solo-fork gate doing its job
# (an isolated node minting a private chain is the failure we designed out).
# So the assertion here is idle-not-fork. Genuine failover needs a third node
# to form quorum with, which seed_restart covers.
log "killing node1 — node2 is now alone and must IDLE, not mint"
kill_node 1 KILL
sleep 90
TIP_AFTER="$(get_height 2)"
if (( TIP_AFTER > TIP_BEFORE + 2 )); then
    fail "SOLO FORK: lone survivor climbed ${TIP_BEFORE} -> ${TIP_AFTER} with $(get_peers 2) peers"
else
    pass "lone survivor idled at ${TIP_AFTER} (peers=$(get_peers 2))"
fi
assert_height_monotonic 2 "${TIP_BEFORE}"

scenario_result
