#!/usr/bin/env bash
# F10 — mass simultaneous restart (cooldown-freeze regression).
#
# Every node re-broadcasts ValidatorRegister on boot. Before the
# first-registration-wins fix, those re-registers reset each validator's
# cooldown clock, so a simultaneous restart put ALL producers in anti-churn
# cooldown at once — and cooldown only expires as the height advances, which
# it cannot while every producer is muzzled. The live chain froze at h33.
#
# Also guards the destructive-recovery blast radius: warm restarts must resume
# from the stored tip, never re-derive from genesis.

SCENARIO_NAME="F10_mass_restart"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

boot_node 1 cold
wait_rpc 1
boot_node 2 cold 1
boot_node 3 cold 1
wait_rpc 2; wait_rpc 3

# Past the 10-block cooldown window and the bootstrap-registration exemption.
if ! wait_all_height 25 240 1 2 3; then
    fail "chain never reached h25 pre-restart (heights: $(heights_of 1 2 3))"
    scenario_result; exit $?
fi
assert_converged 2 60 1 2 3

PRE_MAX=0
for i in 1 2 3; do h="$(get_height $i)"; (( h > PRE_MAX )) && PRE_MAX=$h; done
log "pre-restart tip ${PRE_MAX} — killing all three within the same second"

kill_node 1 KILL; kill_node 2 KILL; kill_node 3 KILL
sleep 3

# WARM: same data dirs. The re-registers land here.
boot_node 1 warm
boot_node 2 warm 1
boot_node 3 warm 1
wait_rpc 1; wait_rpc 2; wait_rpc 3

# No node may have re-derived from genesis.
for i in 1 2 3; do assert_height_monotonic $i $(( PRE_MAX - 2 )); done

log "watching for the cooldown freeze (chain must advance past ${PRE_MAX})..."
if wait_all_height $(( PRE_MAX + 5 )) 180 1 2 3; then
    pass "chain advanced past the restart tip — no simultaneous-cooldown freeze"
else
    fail "COOLDOWN FREEZE: chain stuck at $(heights_of 1 2 3) after mass restart (pre=${PRE_MAX})"
    for i in 1 2 3; do
        log "node${i} cooldown skips: $(scrape $i 'validator cooldown')"
    done
fi

assert_converged 3 90 1 2 3
assert_no_private_fork 1 2 3
assert_no_panic 1 2 3

scenario_result
