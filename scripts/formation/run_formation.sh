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

FORMATION_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${FORMATION_ROOT}/../.." && pwd)"
SRC_DIR="${REPO_ROOT}/src"
# Harness nodes use randomly-generated wallets, which the production validator
# pin (ALPHA_PINNED_VALIDATORS) excludes from leader rotation — so the harness
# builds with the `formation-test` feature (empty pin) into its own target dir,
# never overwriting the release binary that gets deployed.
FORMATION_TARGET="${SRC_DIR}/target/formation"
NODE_BIN="${FORMATION_NODE_BIN:-${FORMATION_TARGET}/release/commputer}"

FAST_SET=(
    F5_mesh_convergence
    F4_pair_liveness
    F10_mass_restart
    seed_restart
    F_isolation_solo_gate
    F1_runaway_detect
)
SOAK_SET=( soak_30min )

# --------------------------------------------------------------------------
# Stage 1 (outside the namespace): build, then re-exec inside it.
# --------------------------------------------------------------------------
if [[ "${FORMATION_ISOLATED:-0}" != "1" ]]; then
    if [[ ! -x "${NODE_BIN}" || -n "${FORCE_BUILD:-}" ]]; then
        echo "[formation] building harness binary (--features formation-test)..."
        ( cd "${SRC_DIR}" && CARGO_TARGET_DIR="${FORMATION_TARGET}" \
            cargo build --release -p commputer --bin commputer --features formation-test ) || {
            echo "[formation] BUILD FAILED" >&2; exit 2; }
    fi
    [[ -x "${NODE_BIN}" ]] || { echo "[formation] no binary at ${NODE_BIN}" >&2; exit 2; }
    echo "[formation] binary: ${NODE_BIN} ($("${NODE_BIN}" --version 2>/dev/null | head -1))"

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
