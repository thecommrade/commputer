#!/usr/bin/env bash
# F1 — runaway producer under induced asymmetry (THE SOAK KILLER).
#
# Nodes accept-vote for proposal heights above their own applied tip
# ("rubber-stamping"), so a producer can reach quorum on votes from peers that
# never applied the blocks — one node finalized 107..123 alone while the other
# two held at 106, and no height-median could see it.
#
# The harness reproduces the asymmetry by repeatedly severing one node's link
# while the others produce, then asserts the invariant that matters: no node's
# height may exceed the height a MAJORITY actually agree on (fingerprint-
# identical) by more than a small tolerance.
#
# EXPECTED TO FAIL until vote-height discipline lands — this scenario is the
# executable definition of "Phase 2 is done".

SCENARIO_NAME="F1_runaway_detect"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

boot_node 1 cold
wait_rpc 1
boot_node 2 cold 1
boot_node 3 cold 1
wait_rpc 2; wait_rpc 3

if ! wait_all_height 12 240 1 2 3; then
    fail "chain never reached h12 (heights: $(heights_of 1 2 3))"
    scenario_result; exit $?
fi
assert_converged 2 60 1 2 3
log "baseline converged at $(heights_of 1 2 3)"

# Induce apply-asymmetry: node3 keeps losing and regaining its link, so it
# falls behind while 1 and 2 keep proposing. Under rubber-stamp voting the
# proposer's rounds still reach quorum.
log "inducing asymmetry: cycling node3's link for 4 rounds"
for round in 1 2 3 4; do
    kill_node 3 KILL
    sleep 20
    boot_node 3 warm 1
    wait_rpc 3
    sleep 10
    log "round ${round}: heights $(heights_of 1 2 3)"
    assert_no_runaway 4 1 2 3
done

log "settling for 60s, then the invariants must hold"
sleep 60
log "final heights: $(heights_of 1 2 3)"

assert_no_runaway 4 1 2 3
assert_no_private_fork 1 2 3
assert_converged 4 120 1 2 3
assert_no_panic 1 2 3

scenario_result
