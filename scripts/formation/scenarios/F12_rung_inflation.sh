#!/usr/bin/env bash
# F12 — Snowball rung inflation halt (QC-001).
#
# The Snowball rung/quorum params are sized from self.peer_ips.len() — the count
# of ANY established libp2p connection (consensus_manager.rs:292
# update_params_for_network_size, fed peer_ips.len() from the tick). An unstaked
# stranger that opens extra sockets raises peer_count and therefore the rung:
#   peer_count 2 -> (3,2,1) quorum 2   (the live 3-node value)
#   peer_count 5 -> (6,4,1) quorum 4
#   peer_count 6..=10 -> (5,4,8) quorum 4
# But only the real validators answer votes, so the reachable vote count stays at
# 2 peers + our self-vote = 3. Push quorum above 3 and quorum_choice is
# permanently None: the node finalizes nothing while the sockets are held. No
# stake, no identity — a handful of dials.
#
# Flooding 6 sockets per node -> peer_count 8 -> rung (5,4,8), quorum 4 > 3
# reachable. Flood ALL THREE so none can finalize and none can rescue the others.
#
# RED/GREEN CONTRACT: the PASS condition is "node1 keeps FINALIZING while the
# sockets are held." On today's unfixed binary the inflated rung stalls
# finalization -> the during-flood assert_finalizes FAILS (RED — the bug is
# present). After the Stage-2 clamp (RungInput = min(peer_count, distinct_eligible
# - 1); on the pin trio min(8, 3-1)=2 -> rung (3,2,1) unchanged) the sockets
# cannot raise the bar and finalization continues -> the scenario PASSES (GREEN).
#
# WHY LOG-BASED ASSERTIONS: a consensus-dead node still climbs in height via sync
# (apply_synced_block, event_loop.rs), so the height-based verbs cannot see this
# halt. assert_finalizes scrapes the "Snowball finalized at height" log line.
#
# Exit-code contract (via scenario_result): 0 pass · 1 assertion failed · 2 infra

SCENARIO_NAME="F12_rung_inflation"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

SYBIL_BIN="${FORMATION_SYBIL_BIN:-${SRC_DIR}/target/formation/release/sybil_dialer}"
[[ -x "${SYBIL_BIN}" ]] || infra "no sybil_dialer binary at ${SYBIL_BIN} (run_formation.sh builds it)"
SYBIL_LOG="${LOG_DIR}/${SCENARIO_NAME}-sybil.log"
: > "${SYBIL_LOG}"

boot_node 1 cold          # bootstrap leader + flood victim (no seeds)
wait_rpc 1
boot_node 2 cold 1 3      # mesh: knows node1 and node3
boot_node 3 cold 1 2      # mesh: knows node1 and node2
wait_rpc 2; wait_rpc 3

log "waiting for the mesh to bootstrap and finalize..."
if ! wait_all_height 5 120 1 2 3; then
    fail "mesh never reached height 5 in 120s (heights: $(heights_of 1 2 3))"
    scenario_result; exit $?
fi
pass "mesh bootstrapped ($(heights_of 1 2 3))"

# ---- BASELINE: a healthy mesh finalizes before the attack (GREEN on any binary).
assert_finalizes 1 30 3

# ---- THE QC-001 ATTACK: hold 6 extra sockets open on ALL THREE nodes.
# 6 sockets + 2 real peers = peer_count 8 -> rung (5,4,8), quorum 4 > 3 reachable.
SOCKETS=6
HOLD=120
declare -a SYBIL_PIDS=()
for n in 1 2 3; do
    "${SYBIL_BIN}" \
        --target "/ip4/127.0.0.1/tcp/$(p2p_port "${n}")" \
        --mode socket-flood \
        --count "${SOCKETS}" \
        --hold-secs "${HOLD}" \
        >>"${SYBIL_LOG}" 2>&1 &
    SYBIL_PIDS+=($!)
done
log "launched socket-flood (${SOCKETS} sockets/node, hold ${HOLD}s) against all 3 nodes"

# Let the connections establish and the 500ms tick recompute the rung.
log "settling 20s for peer_ips to inflate and the rung to recompute..."
sleep 20

# ---- RED/GREEN GATE: node1 must keep finalizing WHILE the sockets are held.
# Unfixed: rung (5,4,8) quorum 4 > 3 reachable -> no finalization -> FAILS (RED).
# Fixed:   clamp keeps rung (3,2,1) quorum 2 -> finalization continues -> PASSES.
# A healthy node finalizes ~1/block-interval; a rung-halted node finalizes 0.
log "during flood: node1 must still be finalizing (RED on today's unfixed binary)"
assert_finalizes 1 40 5

# Sanity: no crash under the flood.
assert_no_panic 1 2 3

# ---- Release the sockets and confirm the flood was the cause (informational):
# on the unfixed binary the chain should resume finalizing once peer_ips drains.
for pid in "${SYBIL_PIDS[@]}"; do kill "${pid}" 2>/dev/null || true; done
for pid in "${SYBIL_PIDS[@]}"; do wait "${pid}" 2>/dev/null || true; done
log "released all sockets; allowing peer_ips to drain"
sleep 15
if assert_finalizes 1 40 5; then
    log "chain resumed finalizing after the sockets were released (confirms rung inflation was the cause)"
fi

scenario_result
