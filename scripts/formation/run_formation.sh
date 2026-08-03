#!/usr/bin/env bash
# run_formation.sh — the Commputer formation-test runner.
#
# Builds the node binary once, then runs formation scenarios inside an
# unprivileged network namespace so local test nodes CANNOT reach the live
# public seed. (A cold local node derives the same genesis hash and chain-id
# as the public alpha, and alpha.5's seed keepalive re-dials
# seed.commputer.xyz every 30s — without the namespace, a harness run joins
# the real network.)
#
# Usage:
#   scripts/formation/run_formation.sh              # fast set (CI gate)
#   scripts/formation/run_formation.sh soak         # soak set
#   scripts/formation/run_formation.sh F5_mesh_convergence   # one scenario
#
# Exit: 0 all passed · 1 a scenario failed an assertion · 2 infrastructure

set -u
set -o pipefail

# SINGLE-FLIGHT: the harness binds FIXED ports, so two concurrent runs collide
# and produce FALSE failures. Take the lock in the outer stage only (the
# namespaced re-exec below inherits the held lock fd); a second run queues
# loudly instead of colliding. Guarded by env var so the re-exec can't deadlock.
HARNESS_LOCK="${FORMATION_LOCK:-/tmp/commputer-harness.lock}"
if [[ "${FORMATION_ISOLATED:-0}" != "1" && -z "${FORMATION_LOCK_HELD:-}" ]]; then
    if ! flock -n "${HARNESS_LOCK}" true 2>/dev/null; then
        echo "[formation] another harness run holds ${HARNESS_LOCK} — waiting (concurrent runs collide on fixed ports)..."
    fi
    exec env FORMATION_LOCK_HELD=1 \
        flock -w "${FORMATION_LOCK_WAIT:-7200}" "${HARNESS_LOCK}" bash "$0" "$@"
fi

FORMATION_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${FORMATION_ROOT}/../.." && pwd)"
SRC_DIR="${REPO_ROOT}/src"
# Harness nodes use randomly-generated wallets, which the production validator
# pin (ALPHA_PINNED_VALIDATORS) excludes from leader rotation — so the harness
# builds with the `formation-test` feature (empty pin) into its own target dir,
# never overwriting the release binary that gets deployed.
FORMATION_TARGET="${SRC_DIR}/target/formation"
NODE_BIN="${FORMATION_NODE_BIN:-${FORMATION_TARGET}/release/commputer}"
# QC-021 adversarial harness client, built into the same target/formation dir
# with the same --features formation-test --release profile as the node.
SYBIL_BIN="${FORMATION_SYBIL_BIN:-${FORMATION_TARGET}/release/sybil_dialer}"

FAST_SET=(
    F5_mesh_convergence
    F4_pair_liveness
    F10_mass_restart
    seed_restart
    F_isolation_solo_gate
    F8_fresh_join
    F1_runaway_detect
    F11_socket_flood
    F12_rung_inflation
)
# No soak scenario exists yet (scenarios/soak_30min.sh was never written), so
# `soak`/`all` would fail with "no such scenario". Left EMPTY until a real soak
# scenario lands; point this at that file when it does.
SOAK_SET=( )

# --------------------------------------------------------------------------
# Stage 1 (outside the namespace): build, then re-exec inside it.
# --------------------------------------------------------------------------
if [[ "${FORMATION_ISOLATED:-0}" != "1" ]]; then
    # ALWAYS build. Cargo is incremental, so a no-op build costs about a second.
    #
    # ⚠ THIS USED TO BE `if [[ ! -x "${NODE_BIN}" ]]`, which rebuilt only when the
    # binary was MISSING — it never compared source mtimes. Every run after the
    # first therefore tested whatever binary happened to be lying in the target
    # dir. On 2026-08-01 that silently ran the PRE-FLIP consensus code against
    # post-flip sources and reported PASS on the first scenario. A harness that
    # can pass while testing code you did not write is worse than no harness at
    # all, because it manufactures false confidence exactly when you are about to
    # change consensus.
    echo "[formation] building harness binary (--features formation-test)..."
    ( cd "${SRC_DIR}" && CARGO_TARGET_DIR="${FORMATION_TARGET}" \
        cargo build --release -p commputer --bin commputer --features formation-test ) || {
        echo "[formation] BUILD FAILED" >&2; exit 2; }
    [[ -x "${NODE_BIN}" ]] || { echo "[formation] no binary at ${NODE_BIN}" >&2; exit 2; }

    # Build the QC-021 attacker binary alongside the node (same package, same
    # feature, same target dir). Cargo is incremental, so this is a no-op after
    # the node build compiled the shared workspace crates.
    echo "[formation] building sybil_dialer (QC-021 attacker, --features formation-test)..."
    ( cd "${SRC_DIR}" && CARGO_TARGET_DIR="${FORMATION_TARGET}" \
        cargo build --release -p commputer --bin sybil_dialer --features formation-test ) || {
        echo "[formation] SYBIL BUILD FAILED" >&2; exit 2; }
    [[ -x "${SYBIL_BIN}" ]] || { echo "[formation] no sybil binary at ${SYBIL_BIN}" >&2; exit 2; }

    # Belt-and-suspenders: prove the binary is NEWER than every source it is
    # built from. If cargo ever declines to rebuild (clock skew, a stale
    # fingerprint), refuse to run rather than report a result about the wrong
    # code.
    #
    # -prune the target dirs FIRST. They live under SRC_DIR and contain
    # build-script generated .rs files (serde, typenum, thiserror, clang-sys...)
    # that are rewritten during the very build this is checking, so scanning
    # them made the guard fire on a perfectly fresh binary — a false alarm on a
    # check whose only value is being trustworthy. It also walked a multi-GB
    # tree on every run.
    #
    # Cargo.toml / Cargo.lock are included: a feature or dependency change alters
    # the binary just as surely as a .rs edit does.
    NEWEST_SRC="$(find "${SRC_DIR}" \
        \( -path '*/target' -o -path '*/target/*' \) -prune -o \
        \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'build.rs' \) \
        -newer "${NODE_BIN}" -print -quit 2>/dev/null)"
    if [[ -n "${NEWEST_SRC}" ]]; then
        echo "[formation] STALE BINARY — ${NEWEST_SRC} is newer than ${NODE_BIN}" >&2
        echo "[formation] refusing to run: results would describe code you did not build" >&2
        echo "[formation] (set SKIP_FRESHNESS_CHECK=1 only if you deliberately want to run a pre-built binary)" >&2
        [[ -n "${SKIP_FRESHNESS_CHECK:-}" ]] || exit 2
        echo "[formation] SKIP_FRESHNESS_CHECK set — proceeding against a KNOWN-STALE binary" >&2
    fi
    echo "[formation] binary: ${NODE_BIN} ($("${NODE_BIN}" --version 2>/dev/null | head -1))"
    echo "[formation] binary is newer than every .rs source — verified fresh"

    unshare -r -n true 2>/dev/null || {
        echo "[formation] unprivileged network namespaces unavailable — REFUSING to run" >&2
        echo "[formation] (running unisolated would join local test nodes to the live public chain)" >&2
        exit 2; }

    echo "[formation] re-exec inside network namespace (loopback only)"
    exec unshare -r -n -- bash -c '
        ip link set lo up 2>/dev/null || ifconfig lo up 2>/dev/null || true
        FORMATION_ISOLATED=1 exec "$0" "$@"
    ' "${BASH_SOURCE[0]}" "$@"
fi

# --------------------------------------------------------------------------
# Stage 2 (inside the namespace): verify isolation, run scenarios.
# --------------------------------------------------------------------------
if curl -s --max-time 2 "http://174.138.35.16:9000" >/dev/null 2>&1; then
    echo "[formation] ISOLATION CHECK FAILED — public seed reachable" >&2
    exit 2
fi
echo "[formation] isolation verified (no route to the public network)"

case "${1:-fast}" in
    fast) SCENARIOS=( "${FAST_SET[@]}" ) ;;
    soak) SCENARIOS=( "${SOAK_SET[@]}" ) ;;
    all)  SCENARIOS=( "${FAST_SET[@]}" "${SOAK_SET[@]}" ) ;;
    *)    SCENARIOS=( "$1" ) ;;
esac

declare -a RESULTS=()
worst=0
started=$(date +%s)

for scn in "${SCENARIOS[@]}"; do
    path="${FORMATION_ROOT}/scenarios/${scn}.sh"
    if [[ ! -f "${path}" ]]; then
        echo "[formation] no such scenario: ${scn}" >&2
        RESULTS+=("${scn} MISSING"); worst=2; continue
    fi
    echo
    echo "════════════════════════════════════════════════════════════════"
    echo "[formation] RUN ${scn}"
    echo "════════════════════════════════════════════════════════════════"
    t0=$(date +%s)
    bash "${path}"; rc=$?
    dt=$(( $(date +%s) - t0 ))
    case ${rc} in
        0) RESULTS+=("PASS  ${scn} (${dt}s)") ;;
        1) RESULTS+=("FAIL  ${scn} (${dt}s)"); (( worst < 1 )) && worst=1 ;;
        *) RESULTS+=("INFRA ${scn} (${dt}s, rc=${rc})"); worst=2 ;;
    esac
    pkill -f "${NODE_BIN}" 2>/dev/null || true
    sleep 2
done

echo
echo "════════════════════════════════════════════════════════════════"
echo "[formation] SUMMARY ($(( $(date +%s) - started ))s total)"
printf '  %s\n' "${RESULTS[@]}"
echo "════════════════════════════════════════════════════════════════"
exit ${worst}
