#!/usr/bin/env bash
# F13_attest_kill — the LIVENESS FLOOR of the attestation gate (QC-009 companion).
#
# The QC-009 fix will gate vote intake on VALIDATOR ATTESTATION: a
# ConsensusResponse::Vote is counted only when it comes from an attested member
# of the current validator set, so a stranger's socket (F13_vote_capture) can no
# longer drive finalization. The safety win is obvious. The RISK is LIVENESS: if
# the gate is too strict, a network with attestation misconfigured or briefly
# unavailable could stop finalizing entirely instead of degrading gracefully.
#
# This scenario pins that liveness floor. With attestation DISABLED via
# COMMPUTER_ATTEST_DISABLE=1 and NO adversary present, a healthy 3-node mesh must
# STILL finalize and converge — the honest validators are still the honest
# validators. It proves the attestation path FAILS OPEN for liveness among honest
# nodes (degrades, does not halt), which is the property that makes the safety
# gate deployable.
#
# LOAD-BEARING: this binary reads COMMPUTER_ATTEST_DISABLE (formation-test only,
# event_loop.rs attest_disabled) and gates vote intake on the attestation binding.
# With attestation disabled on all three, no peer binds, so the QC-009 vote gate
# has nothing bound and MUST fall back to counting unbound votes after GRACE_T —
# the chain has to keep finalizing and converge, proving the liveness floor
# DEGRADES rather than halts. If it stalls or diverges, the floor is wired wrong.
# In FAST_SET (run_formation.sh).
#
# Exit-code contract (via scenario_result): 0 pass · 1 assertion failed · 2 infra

SCENARIO_NAME="F13_attest_kill"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

# Disable attestation for every node booted below. boot_node runs the node with
# the surrounding environment inherited, so exporting here is sufficient.
#   NOTE: on the clamp-only binary this variable is NOT YET READ (no gate exists).
#   It is set now so the scenario is correct the day the gate lands; until then it
#   is inert and this run just proves a healthy mesh finalizes + converges.
export COMMPUTER_ATTEST_DISABLE=1
log "COMMPUTER_ATTEST_DISABLE=1 exported (inert on the clamp-only binary — no gate reads it yet)"

boot_node 1 cold          # seed-less bootstrap leader
wait_rpc 1
boot_node 2 cold 1 3      # mesh: knows node1 and node3
boot_node 3 cold 1 2      # mesh: knows node1 and node2
wait_rpc 2; wait_rpc 3

log "waiting for the mesh to bootstrap..."
if ! wait_all_height 5 120 1 2 3; then
    fail "mesh never reached height 5 in 120s (heights: $(heights_of 1 2 3))"
    scenario_result; exit $?
fi
pass "mesh bootstrapped ($(heights_of 1 2 3))"

# Grace window, then the liveness floor: with attestation disabled and no
# adversary, every honest node must keep FINALIZING (log-based — a consensus-dead
# node still climbs via sync, so height verbs alone would miss a finalization
# halt) and the mesh must CONVERGE on one chain.
log "grace window (15s) before asserting the attestation-off liveness floor..."
sleep 15

assert_finalizes 1 40 5
assert_finalizes 2 40 5
assert_finalizes 3 40 5
assert_converged 3 60 1 2 3
assert_no_panic 1 2 3

scenario_result
