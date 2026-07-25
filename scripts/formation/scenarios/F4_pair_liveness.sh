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

# The view-change clock must be armed from boot; if a node reports zero
# production skips but the chain still froze, the gate is elsewhere.
log "leader rotation evidence: node1 produced=$(scrape 1 'Produced block candidate'), node2 produced=$(scrape 2 'Produced block candidate')"
if (( $(scrape 1 'Produced block candidate') == 0 )) || (( $(scrape 2 'Produced block candidate') == 0 )); then
    fail "one node never produced — leadership did not rotate"
fi

scenario_result
