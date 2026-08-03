#!/usr/bin/env bash
# F13 — Snowball vote-capture / finalization fork (QC-009).
#
# Reproduces QC-009 against the Stage-2 clamp binary. The clamp pins the rung at
# (3,2,1) — quorum 2, decision_threshold(beta) 1, sample k=3 — regardless of how
# many sockets connect (consensus_manager.rs RungInput::derive, wired at
# event_loop.rs:4172). Vote intake is UNAUTHENTICATED: a ConsensusResponse::Vote
# from ANY connected PeerId is counted by record_peer_response
# (event_loop.rs:2089 -> consensus_manager.rs:725).
#
# THE ATTACK (sybil_dialer --mode vote-capture):
#   * ~40 held sockets per node, each a distinct PeerId (a distinct VOTER). Each
#     enters the node's peer_ips unconditionally (event_loop.rs:1630); 40+2 <
#     MAX_PEERS=50 so none are evicted.
#   * The node (proposer) sends its honest block for tip+1 to every peer as a
#     ConsensusRequest::BlockProposal (event_loop.rs:4104/4195). Each socket reads
#     the height + REAL parent + timestamp from that honest block and crafts an
#     EMPTY tip-parented block B signed by a FIXED throwaway wallet — so B is
#     byte-identical across every socket and its producer (ATTACK_PRODUCER) is
#     stable. Each socket then:
#       - VOTES for B on the inbound channel (ConsensusResponse::Vote{pref=B.hash})
#         — the crux the node counts at event_loop.rs:2089;
#       - SEEDS B back (BlockProposal add_candidate at event_loop.rs:1984 + a
#         best-effort BlockCandidate gossip) so the node HOLDS B as a votable,
#         tip-parented candidate (consensus_manager.rs:794 retains only held,
#         tip-parented candidates).
#   * ~40 distinct-PeerId votes for B dominate the k=3 sample -> B reaches quorum
#     2 and finalizes in one beta=1 round (snowball.rs:130-162). B is then applied
#     (apply_block_validated has no producer/leader gate and does not verify
#     state_root — state.rs:1272-1330): the node logs
#     "Finalized and applied block <B.hash()> at height <h>". A self-produced tip
#     was finalized — finalization capture / fork.
#
# RED/GREEN CONTRACT — PRODUCER-MEMBERSHIP, NOT a finalize count. A captured node
# STILL logs "Snowball finalized" (its own honest blocks below the capture, then
# the attacker's), so a finalize count is a WORTHLESS discriminator here. The PASS
# condition is: NO node ever applies a block produced by ATTACK_PRODUCER. The
# discriminator scrapes each node log for "Finalized and applied block <hash>"
# where <hash> is one of the attacker block hashes the sybil printed — every such
# hash is produced by ATTACK_PRODUCER by construction, so a match is a capture.
#   * On the clamp-only binary (no attestation gate): B finalizes -> at least one
#     node applies an ATTACK_PRODUCER block -> FAIL (RED — QC-009 reproduced).
#   * After attestation lands (a vote is counted only from an attested validator):
#     the sybil votes are ignored, B never finalizes -> GREEN. That fix is NOT in
#     this lane, so this scenario is EXPECTED RED here.
#
# TWO FALSE-GREEN GUARDS the grounding pass flagged:
#   (i)  Confirm the sybil sockets are actually COUNTED as peers before judging
#        (else MAX_PEERS / subnet eviction dropped them, no vote landed, and a
#        GREEN would be INCONCLUSIVE, not a fix). We assert node1's peer count
#        climbs well past the honest baseline.
#   (ii) NEVER use a bare finalize-count as the discriminator (see above).
#
# WHY LOG-BASED: a node whose honest peers forked away still climbs via sync, and
# height/convergence verbs cannot see "which producer's block was applied". The
# apply log line names the block; the attacker's hashes name the producer.
#
# Exit-code contract (via scenario_result): 0 pass · 1 assertion failed · 2 infra

SCENARIO_NAME="F13_vote_capture"
source "$(dirname "${BASH_SOURCE[0]}")/../lib/formation_lib.sh"
assert_isolated

SYBIL_BIN="${FORMATION_SYBIL_BIN:-${SRC_DIR}/target/formation/release/sybil_dialer}"
[[ -x "${SYBIL_BIN}" ]] || infra "no sybil_dialer binary at ${SYBIL_BIN} (run_formation.sh builds it)"
SYBIL_LOG="${LOG_DIR}/${SCENARIO_NAME}-sybil.log"
: > "${SYBIL_LOG}"

SOCKETS="${F13_SOCKETS:-40}"     # 40 + 2 honest = 42 < MAX_PEERS(50): no eviction
HOLD="${F13_HOLD:-120}"

boot_node 1 cold          # bootstrap leader + capture victim (no seeds)
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

BASE_PEERS="$(get_peers 1)"; BASE_PEERS="${BASE_PEERS:-0}"
log "baseline node1 peer count: ${BASE_PEERS}"

# ---- THE QC-009 ATTACK: ~40 vote-capture sockets on ALL THREE nodes, in
# parallel. Each process holds its sockets and answers proposals for HOLD seconds.
declare -a SYBIL_PIDS=()
for n in 1 2 3; do
    "${SYBIL_BIN}" \
        --target "/ip4/127.0.0.1/tcp/$(p2p_port "${n}")" \
        --target-rpc "http://127.0.0.1:$(rpc_port "${n}")" \
        --mode vote-capture \
        --count "${SOCKETS}" \
        --hold-secs "${HOLD}" \
        >>"${SYBIL_LOG}" 2>&1 &
    SYBIL_PIDS+=($!)
done
log "launched vote-capture (${SOCKETS} sockets/node, hold ${HOLD}s) against all 3 nodes"

# Let the sockets connect and the first captures land.
log "settling 30s for sockets to connect and captures to begin..."
sleep 30

# ---- GUARD (i): the sybil sockets MUST be counted as peers, or nothing this
# scenario asserts is meaningful. If they were evicted, a later "no capture" is
# INCONCLUSIVE (infra), not a GREEN result.
NOW_PEERS="$(get_peers 1)"; NOW_PEERS="${NOW_PEERS:-0}"
log "node1 peer count during flood: ${NOW_PEERS} (baseline ${BASE_PEERS}, launched ${SOCKETS}/node)"
if (( NOW_PEERS < 20 )); then
    infra "sybil sockets NOT counted as peers (node1 peers=${NOW_PEERS}, want >= 20): eviction/subnet — result would be inconclusive, not GREEN"
fi
pass "sybil sockets counted as peers (node1 peers=${NOW_PEERS} >> baseline ${BASE_PEERS})"

# Let captures accumulate across many heights.
log "holding 60s more for vote-capture to finalize attacker blocks..."
sleep 60

# ---- DISCRIMINATOR: producer-membership. Collect every attacker block hash the
# sybil printed (each is produced by ATTACK_PRODUCER), then check whether any
# node's apply log finalized one of them.
ATTACK_PRODUCER="$(grep -oE 'ATTACK_PRODUCER=[0-9a-f]+' "${SYBIL_LOG}" 2>/dev/null | head -1 | cut -d= -f2)"
[[ -n "${ATTACK_PRODUCER}" ]] || infra "sybil never printed ATTACK_PRODUCER (see ${SYBIL_LOG}) — attacker did not start"
log "ATTACK_PRODUCER=${ATTACK_PRODUCER}"

ATTACK_HASH_FILE="${LOG_DIR}/${SCENARIO_NAME}-attack-hashes.txt"
grep -oE 'ATTACK_BLOCK height=[0-9]+ hash=[0-9a-f]+' "${SYBIL_LOG}" 2>/dev/null \
    | grep -oE 'hash=[0-9a-f]+' | cut -d= -f2 | sort -u > "${ATTACK_HASH_FILE}"
N_ATTACK="$(wc -l < "${ATTACK_HASH_FILE}" | tr -d ' ')"
log "sybil crafted ${N_ATTACK} distinct attacker block hashes (producer ${ATTACK_PRODUCER})"
if (( N_ATTACK == 0 )); then
    infra "sybil crafted 0 attacker blocks — it never received a BlockProposal to answer (no vote could land); inconclusive"
fi

# For each node, intersect its APPLIED block hashes with the attacker set.
CAPTURED=0
for n in 1 2 3; do
    logf="$(log_file "${n}")"
    applied="${LOG_DIR}/${SCENARIO_NAME}-node${n}-applied.txt"
    grep -oE 'Finalized and applied block [0-9a-f]+ at height [0-9]+' "${logf}" 2>/dev/null > "${applied}" || true
    # hashes this node APPLIED
    awk '{print $5}' "${applied}" | sort -u > "${applied}.hashes"
    # any attacker hash among them?
    hits="$(comm -12 "${ATTACK_HASH_FILE}" "${applied}.hashes" 2>/dev/null)"
    if [[ -n "${hits}" ]]; then
        CAPTURED=1
        # Report the first captured height for evidence.
        while read -r H; do
            line="$(grep -E "Finalized and applied block ${H} at height [0-9]+" "${logf}" 2>/dev/null | head -1)"
            hgt="$(sed -nE 's/.*at height ([0-9]+).*/\1/p' <<<"${line}")"
            fail "CAPTURE: node${n} applied ATTACK_PRODUCER block ${H} at height ${hgt} (QC-009 vote-capture reproduced — RED on the clamp-only binary)"
        done <<< "${hits}"
    else
        log "node${n}: no attacker block applied (applied $(wc -l < "${applied}.hashes" | tr -d ' ') distinct block hashes)"
    fi
done

# ---- SECONDARY (corroboration only): scan each node's recent blocks via RPC for
# a block whose producer == ATTACK_PRODUCER. Informational — the log intersection
# above is authoritative — but it also fails the scenario if it finds a capture
# the log scrape missed (e.g. log truncation).
PBYTES="$(grep -oE 'ATTACK_PRODUCER_BYTES=\[[0-9,]+\]' "${SYBIL_LOG}" 2>/dev/null | head -1 | cut -d= -f2-)"
if [[ -n "${PBYTES}" ]]; then
    for n in 1 2 3; do
        H="$(get_height "${n}")"; H="${H:-0}"
        (( H < 1 )) && continue
        start=$(( H > 30 ? H - 30 : 1 ))
        for h in $(seq "${start}" "${H}"); do
            body="$(curl -fsS --max-time 2 "http://127.0.0.1:$(rpc_port "${n}")/block/${h}" 2>/dev/null)" || continue
            [[ -z "${body}" ]] && continue
            match="$(jq -e --argjson want "${PBYTES}" '.header.producer == $want' <<<"${body}" 2>/dev/null && echo yes)"
            if [[ "${match}" == "yes" ]]; then
                CAPTURED=1
                fail "CAPTURE (RPC): node${n} /block/${h} producer == ATTACK_PRODUCER — QC-009 vote-capture reproduced (RED)"
                break
            fi
        done
    done
fi

if (( CAPTURED == 0 )); then
    # GREEN would mean the attack did NOT land. On the clamp-only binary that is
    # a false GREEN — see the diagnosis checklist in the report. We do NOT pass
    # silently: emit the evidence needed to debug (heights, peer count, whether
    # any attacker block was even seeded).
    pass "no node applied an ATTACK_PRODUCER block — vote-capture did NOT land (EXPECTED GREEN only after attestation; on the clamp-only binary investigate: peers=${NOW_PEERS}, attacker-blocks=${N_ATTACK}, heights=$(heights_of 1 2 3))"
fi

# No crash under the attack, on any node.
assert_no_panic 1 2 3

# Release the sockets.
for pid in "${SYBIL_PIDS[@]}"; do kill "${pid}" 2>/dev/null || true; done
for pid in "${SYBIL_PIDS[@]}"; do wait "${pid}" 2>/dev/null || true; done
log "released all vote-capture sockets"

scenario_result
