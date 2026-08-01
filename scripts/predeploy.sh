#!/usr/bin/env bash
# predeploy.sh — the pre-deploy gate, in ONE command, runnable from ANY directory.
#
# WHY THIS EXISTS
#   The standing rule is: every deploy = full test gate + formation harness. Both
#   were being assembled by hand each time, and that kept going wrong in the same
#   two ways:
#
#   1. WRONG WORKING DIRECTORY. The cargo workspace root is `src/`, but the
#      scripts live at the REPO root. So `cd src && cargo test; bash
#      scripts/formation/run_formation.sh` runs the gate correctly and then looks
#      for the harness at `src/scripts/...`, which does not exist. It dies with
#      exit 127 having tested nothing. This happened twice in one session.
#
#   2. PIPED EXIT CODES. `cargo test --workspace | tail -40` reports TAIL's exit
#      status, which is 0 no matter what cargo did, and throws away all but the
#      last 40 lines. A completely failing gate looked identical to a passing one.
#
#   Both are solved here once: every path is absolute and derived from this
#   script's own location, and every exit code is captured from the command that
#   actually matters.
#
# USAGE
#   scripts/predeploy.sh            # gate + harness
#   scripts/predeploy.sh --gate     # gate only
#   scripts/predeploy.sh --harness  # harness only
#
# EXIT
#   0 = everything passed. Non-zero = DO NOT DEPLOY. Logs are printed at the end.

set -uo pipefail

# SINGLE-FLIGHT: two concurrent predeploy runs clobber each other's fixed log
# paths below, and their harness halves collide on the harness's fixed ports
# (which fakes failures — it happened twice). Serialize whole runs: a second
# invocation waits for the first instead of corrupting its evidence.
PREDEPLOY_LOCK="${PREDEPLOY_LOCK:-/tmp/commputer-predeploy.lock}"
if [ -z "${PREDEPLOY_LOCK_HELD:-}" ]; then
  if ! flock -n "${PREDEPLOY_LOCK}" true 2>/dev/null; then
    echo "== another predeploy run holds ${PREDEPLOY_LOCK} — waiting for it to finish =="
  fi
  exec env PREDEPLOY_LOCK_HELD=1 \
    flock -w "${PREDEPLOY_LOCK_WAIT:-7200}" "${PREDEPLOY_LOCK}" bash "$0" "$@"
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SRC_DIR="${REPO_ROOT}/src"
HARNESS="${SCRIPT_DIR}/formation/run_formation.sh"

LOG_DIR="${PREDEPLOY_LOG_DIR:-${TMPDIR:-/tmp}/commputer-predeploy}"
mkdir -p "${LOG_DIR}"
GATE_LOG="${LOG_DIR}/gate.log"
HARNESS_LOG="${LOG_DIR}/harness.log"

RUN_GATE=1
RUN_HARNESS=1
case "${1:-}" in
  --gate)    RUN_HARNESS=0 ;;
  --harness) RUN_GATE=0 ;;
  "")        ;;
  *)         echo "usage: $0 [--gate|--harness]" >&2; exit 2 ;;
esac

FAILED=0

if [ "${RUN_GATE}" -eq 1 ]; then
  echo "== GATE: cargo test --workspace (from ${SRC_DIR}) =="
  # Redirect, never pipe: $? must come from cargo, not from a filter.
  ( cd "${SRC_DIR}" && cargo test --workspace ) > "${GATE_LOG}" 2>&1
  GATE_EXIT=$?
  PASSED=$(awk '/^test result:/{p+=$4} END{print p+0}' "${GATE_LOG}")
  FAILS=$(awk  '/^test result:/{f+=$6} END{print f+0}' "${GATE_LOG}")
  SUITES=$(grep -c '^test result:' "${GATE_LOG}")
  echo "   exit=${GATE_EXIT}  passed=${PASSED}  failed=${FAILS}  suites=${SUITES}"
  echo "   log: ${GATE_LOG}"
  # Sanity-check the MAGNITUDE too: a handful of tests means the run died early
  # or a filter silently matched nothing, even if the exit code is 0.
  if [ "${GATE_EXIT}" -ne 0 ]; then
    echo "   GATE FAILED"; FAILED=1
  elif [ "${PASSED}" -lt 500 ]; then
    echo "   GATE SUSPICIOUS: only ${PASSED} tests ran; this workspace runs ~1800."
    echo "   Treat this as a FAILURE — it usually means the run aborted early."
    FAILED=1
  else
    echo "   GATE PASSED"
  fi
fi

if [ "${RUN_HARNESS}" -eq 1 ]; then
  echo
  echo "== HARNESS: ${HARNESS} =="
  [ -x "${HARNESS}" ] || [ -f "${HARNESS}" ] || {
    echo "   harness not found at ${HARNESS}" >&2; exit 2; }
  # Absolute path: immune to the caller's working directory. The harness itself
  # re-derives its own roots and refuses to run against a stale binary.
  bash "${HARNESS}" > "${HARNESS_LOG}" 2>&1
  HARNESS_EXIT=$?
  echo "   exit=${HARNESS_EXIT}"
  echo "   log: ${HARNESS_LOG}"
  grep -E 'verified fresh|STALE BINARY' "${HARNESS_LOG}" | sed 's/^/   /'
  sed -n '/SUMMARY/,$p' "${HARNESS_LOG}" | sed 's/^/   /'
  if [ "${HARNESS_EXIT}" -ne 0 ]; then
    echo "   HARNESS FAILED"; FAILED=1
  else
    echo "   HARNESS PASSED"
  fi
fi

echo
if [ "${FAILED}" -eq 0 ]; then
  echo "PRE-DEPLOY CHECKS PASSED"
else
  echo "PRE-DEPLOY CHECKS FAILED — do not deploy"
fi
exit "${FAILED}"
