#!/usr/bin/env bash
# F11 — candidate-map exhaustion halt (QC-021).
#
# Reproduces the QC-021 finding: one unstaked socket floods a node with 64
# EMPTY blocks at a FUTURE height, each self-signed by a throwaway ed25519 key
# with a FABRICATED parent hash. Those 64 distinct hashes fill the per-height
# candidate cap (MAX_CANDIDATES_PER_HEIGHT = 64, consensus_manager.rs:63); the
# cap then drops the ARRIVING candidate and preserves incumbents
# (consensus_manager.rs:395-400), so when the honest leader's real block for
# that height is produced it is evicted everywhere. The ballot only counts
# candidates whose parent_hash == tip_hash (consensus_manager.rs:546), and all
# 64 have a foreign parent, so nothing at that height is votable, nothing
# finalizes, and the tip never advances past it — a permanent one-height halt
# for ~64 self-signed frames from one socket.
#
# WHY LOG-BASED ASSERTIONS: a consensus-dead node still climbs in height via
# sync (apply_synced_block, event_loop.rs:4583-4643), so the height-based verbs
# (assert_progress/assert_converged) cannot see this halt. assert_finalizes
# scrapes the "Snowball finalized at height" log line instead.
#
# RED/GREEN CONTRACT: the PASS condition is "node1 keeps FINALIZING despite the
# flood." On today's unfixed binary the poisoned future height stalls
# finalization, so the post-flood assert_finalizes FAILS (RED — the bug is
# present, and that failure is the evidence). After the Stage-1 admission fix
# (the leader's tip-parented block is never evicted by foreign-parent
# candidates) finalization continues and the scenario PASSES (GREEN).
#
# Topology mirrors F5: node1 is the seed-less bootstrap leader AND the flood
# victim; node2/node3 mesh into it. The flood is published to node1 only; libp2p
# gossipsub's default forwarding re-broadcasts the frames to node1's mesh peers
# (QC-021 honest-caveat (a)), so the poison reaches all three.
#
# Exit-code contract (via scenario_result): 0 pass · 1 assertion failed · 2 infra
#
# NOTE (secondary vector, QC-001): the sybil_dialer also implements a
# `socket-flood` mode (open N distinct connections to inflate the consensus
# rung). That is a DIFFERENT finding (QC-001, self-heals on disconnect) and is
# left as a documented TODO below rather than gated here — this scenario stays
# focused on the QC-021 candidate-flood, the actual deliverable.

SCENARIO_NAME="F11_socket_flood"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

# The attacker binary is built alongside the node into target/formation by
# run_formation.sh (same --features formation-test --release profile). Reference
# it the same way formation_lib.sh references NODE_BIN.
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

# ---- BASELINE: a healthy mesh both PRODUCES and FINALIZES before the attack.
# If these fail the scenario is inconclusive (the mesh was not healthy to begin
# with), not a QC-021 result — but they must be GREEN on any binary.
assert_finalizes 1 30 3
assert_produces 1 30 1

# ---- THE QC-021 ATTACK: flood ONE explicit FUTURE height on ALL THREE nodes.
#
# Two things this must get right, learned from the first RED attempt:
#  1. FUTURE ENOUGH. The formation chain finalizes ~0.7 heights/s. With a small
#     offset the honest tip passes the flood height before the 64 frames arrive,
#     and add_candidate drops them as stale (height <= applied_tip,
#     consensus_manager.rs:384) — the map is never poisoned. `OFFSET` heights of
#     lead time (below MAX_HEIGHT_WINDOW=1024) lets all 64 land while the height
#     is still ahead of every node's tip.
#  2. NO UNPOISONED RESCUER. If only node1 were flooded, node2/node3 would
#     finalize the honest block at flood_height and node1 would sync past the
#     wall from them — a false GREEN. Flooding all three at the SAME explicit
#     height means none can finalize it, so none can rescue the others: the
#     chain-wide halt QC-021 caveat (a) describes, reproduced directly rather
#     than via gossip-forward luck. The floods run in PARALLEL so they all land
#     inside one ~12s window (tip advances only ~8 heights meanwhile).
OFFSET=25
TIP_BEFORE="$(get_height 1)"
[[ -n "${TIP_BEFORE}" ]] || infra "could not read node1 tip before flood"
FLOOD_HEIGHT=$(( TIP_BEFORE + OFFSET ))
log "tip=${TIP_BEFORE}; flooding FUTURE height ${FLOOD_HEIGHT} (offset ${OFFSET}) on all 3 nodes"

declare -a SYBIL_PIDS=()
for n in 1 2 3; do
    "${SYBIL_BIN}" \
        --target "/ip4/127.0.0.1/tcp/$(p2p_port "${n}")" \
        --target-rpc "http://127.0.0.1:$(rpc_port "${n}")" \
        --mode candidate-flood \
        --vector gossip \
        --count 64 \
        --flood-height "${FLOOD_HEIGHT}" \
        >>"${SYBIL_LOG}" 2>&1 &
    SYBIL_PIDS+=($!)
done

# Wait for all three floods to finish (each exits 0 when its 64 are sent), bounded.
sybil_deadline=$(( $(date +%s) + 90 ))
for pid in "${SYBIL_PIDS[@]}"; do
    while kill -0 "${pid}" 2>/dev/null && (( $(date +%s) < sybil_deadline )); do sleep 1; done
    kill -0 "${pid}" 2>/dev/null && kill -KILL "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null; rc=$?
    (( rc == 0 )) || log "WARNING: a sybil flood exited rc=${rc} — poison may be incomplete (see ${SYBIL_LOG})"
done
log "all floods done; sent $(grep -c 'gossip BlockCandidate' "${SYBIL_LOG}" 2>/dev/null) candidate frames total (see ${SYBIL_LOG})"

# ---- RED/GREEN GATE: does the honest tip get STUCK at the poisoned height?
# A halted chain climbs to flood_height-1 and stops dead — flood_height can
# never finalize because the honest block there is evicted by the 64
# foreign-parent candidates and nothing at that height is votable
# (query_votable_preference filters parent_hash==tip_hash, consensus_manager.rs:546).
# A healthy (fixed) chain sails straight past flood_height.
#
# Observed AFTER the tip reaches the wall, not over a fixed post-flood window: a
# node finalizes every height BELOW flood_height normally, so a fixed-window
# "finalized >= N" check passes on the unfixed binary too. The load-bearing test
# is that the tip cannot cross flood_height.
log "waiting for node1 to climb to the wall (${FLOOD_HEIGHT}-1)..."
if ! wait_height 1 $(( FLOOD_HEIGHT - 1 )) 120; then
    fail "node1 never reached ${FLOOD_HEIGHT}-1 within 120s (tip $(get_height 1)); flood may not have landed"
    scenario_result; exit $?
fi

h1="$(get_height 1)"
log "node1 at ${h1}; checking whether it can cross the poisoned height ${FLOOD_HEIGHT} (25s)"
sleep 25
h2="$(get_height 1)"
h2b="$(get_height 2)"; h3b="$(get_height 3)"
log "post-wall heights over 25s: node1 ${h1}->${h2}, node2 ${h2b}, node3 ${h3b} (poisoned ${FLOOD_HEIGHT})"
if (( ${h2:-0} > FLOOD_HEIGHT )); then
    pass "node1 crossed the poisoned height ${FLOOD_HEIGHT} (${h1} -> ${h2}) — not halted (GREEN, fix present)"
else
    fail "node1 STUCK at ${h2} (poisoned height ${FLOOD_HEIGHT}) — QC-021 halt reproduced (RED on unfixed binary)"
fi

assert_no_panic 1 2 3
scenario_result

# TODO(QC-001, secondary): a socket-flood scenario (open N distinct sockets to
# inflate the consensus rung, then assert the quorum requirement moves) would use
#   "${SYBIL_BIN}" --target "${TARGET_P2P}" --mode socket-flood --count 64 --hold-secs 120
# It is intentionally NOT gated here: QC-001 self-heals on disconnect
# (peer_ips.remove on ConnectionClosed, event_loop.rs:2161), so it needs a
# different assertion (a held-open rung check), and it is a distinct finding.
