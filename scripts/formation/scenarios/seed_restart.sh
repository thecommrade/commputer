#!/usr/bin/env bash
# seed restart while the other two hold each other.
#
# The original formation wedge: validators kept each other as peers, so the
# zero-peer-only reconnection sweep never re-dialed the returned seed and the
# star never re-knit. Now the keepalive must re-establish the link regardless
# of peer count. Also exercises the seed's own restart path — a store whose
# blocks arrived via sync used to lack block 0 and panic on resume.

SCENARIO_NAME="seed_restart"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

boot_node 1 cold           # the seed (seed-less bootstrap leader)
wait_rpc 1
# The validators know the seed AND each other. Booting them seed-only would
# leave both at zero peers the moment the seed dies — which is what the LIVE
# topology did, and why that outage wedged: with no peers a node cannot form
# quorum at all, so the scenario would be testing the topology rather than the
# failover it means to exercise.
boot_node 2 cold 1 3
boot_node 3 cold 1 2
wait_rpc 2; wait_rpc 3

if ! wait_all_height 20 240 1 2 3; then
    fail "chain never reached h20 (heights: $(heights_of 1 2 3))"
    scenario_result; exit $?
fi
assert_converged 2 60 1 2 3

SEED_TIP="$(get_height 1)"
log "seed at ${SEED_TIP} — taking it down for 40s"
kill_node 1 KILL

# The pair must keep finalizing without the seed (6s view-change promotes a
# fallback leader).
log "pair must keep producing during the outage..."
assert_progress 40 3 2 3

log "restarting the seed WARM (resume path)"
boot_node 1 warm
wait_rpc 1
assert_no_panic 1
assert_height_monotonic 1 $(( SEED_TIP - 2 ))

# The keepalive must re-link the seed even though 2 and 3 already have a peer.
log "waiting for the seed to re-link and catch up..."
if assert_converged 3 150 1 2 3; then
    pass "seed re-knit into the mesh after restart"
else
    log "seed keepalive dials: $(scrape 1 'Seed keepalive|Dialed default DNS seed')"
    log "node2 keepalive dials: $(scrape 2 'Seed keepalive')"
fi

assert_no_private_fork 1 2 3
assert_no_panic 1 2 3

scenario_result
