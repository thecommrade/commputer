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
SURVIVOR_BEFORE="$(scrape 2 'Produced block candidate')"
TIP_BEFORE="$(get_height 2)"
log "killing node1 — node2 must take over via view change"
kill_node 1 KILL

if wait_height 2 $(( TIP_BEFORE + 3 )) 120; then
    pass "survivor took over: height ${TIP_BEFORE} -> $(get_height 2), produced $(( $(scrape 2 'Produced block candidate') - SURVIVOR_BEFORE )) blocks"
else
    fail "FAILOVER DEAD: node2 stuck at $(get_height 2) for 120s after the leader died"
    log "node2 skip reasons:"
    grep -aoE "Skipping block production — [^\"]{0,60}" "$(log_file 2)" | sed 's/height [0-9]*/height N/;s/[0-9]\+s/Ns/' | sort | uniq -c | sort -rn | head -4
fi
assert_height_monotonic 2 "${TIP_BEFORE}"

scenario_result
