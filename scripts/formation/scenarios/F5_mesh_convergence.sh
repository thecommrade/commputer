#!/usr/bin/env bash
# F5 — full-mesh symmetric-quorum convergence (base sanity gate).
#
# Three cold nodes, every node seeded from every other: three competing
# producers with no asymmetry to break the tie. This is the topology that
# deadlocked live on 2026-07-25 (each node preferred its own first-arrived
# candidate, no quorum ever formed) until the lowest-hash tie-break landed.
# Node 1 is seed-less so it can bootstrap block 1; the others mesh into it.

SCENARIO_NAME="F5_mesh_convergence"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

boot_node 1 cold          # bootstrap leader (no seeds)
wait_rpc 1
boot_node 2 cold 1 3      # mesh: knows node1 and node3
boot_node 3 cold 1 2      # mesh: knows node1 and node2
wait_rpc 2; wait_rpc 3

log "waiting for the mesh to start finalizing..."
if ! wait_all_height 3 120 1 2 3; then
    fail "mesh never reached height 3 in 120s (symmetric deadlock?)"
    log "heights: $(heights_of 1 2 3)"
    scenario_result; exit $?
fi
pass "mesh bootstrapped past the symmetric-tie window"

assert_progress 40 3 1 2 3
assert_converged 2 90 1 2 3
assert_no_private_fork 1 2 3
assert_no_runaway 3 1 2 3
assert_no_panic 1 2 3

scenario_result
