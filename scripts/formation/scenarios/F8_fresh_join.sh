#!/usr/bin/env bash
# F8 — a fresh node joins a chain that is already running.
#
# The join path a real operator takes, and the one that broke live on
# 2026-07-25: a wiped validator connected, learned the network's height, began
# downloading — and was then marked "sync_complete" by the solo-node latch,
# which fired despite the node having a peer. It sat at height 0 for ten
# minutes reporting `synced: true` while its peers were at 1265, because every
# sync re-engage was undone by the same latch.

SCENARIO_NAME="F8_fresh_join"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

boot_node 1 cold
wait_rpc 1
boot_node 2 cold 1
wait_rpc 2

# Build a chain tall enough that the joiner must genuinely sync, and well past
# the 30s window in which the solo latch is armed.
if ! wait_all_height 40 300 1 2; then
    fail "chain never reached h40 (heights: $(heights_of 1 2))"
    scenario_result; exit $?
fi
TIP="$(get_height 1)"
log "chain at ${TIP} — joining a cold node"

boot_node 3 cold 1 2
wait_rpc 3

# The joiner must climb. A node that latches solo sits at 0 forever while
# cheerfully reporting itself synced, so assert real height movement.
if wait_height 3 $(( TIP / 2 )) 180; then
    pass "joiner reached $(get_height 3) (chain was ${TIP} at join)"
else
    fail "JOIN FAILED: node3 stuck at $(get_height 3) after 180s (chain ${TIP}+)"
    log "node3 health: $(curl -fsS --max-time 2 "http://127.0.0.1:$(rpc_port 3)/health" 2>/dev/null | jq -c '{peers,synced}')"
    log "node3 solo-latch hits: $(scrape 3 'No network blocks found after 30s')"
    log "node3 sync lines:"; grep -a "\[sync\]" "$(log_file 3)" | tail -3 | cut -c1-150
fi

# It must catch up to the tip and hold the same chain, not a private one.
assert_converged 5 240 1 2 3
assert_no_private_fork 1 2 3
assert_no_panic 1 2 3
# The latch must not have fired at all: node3 had a peer the whole time.
assert_log_count_lt 3 "No network blocks found after 30s" 1 "solo-latch fires while peered"

scenario_result
