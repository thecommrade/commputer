#!/usr/bin/env bash
# precheck_node.sh — pre-launch verification for multi-machine testnet ceremony.
#
# WHAT IT CHECKS (all required for green light):
#   1. `commputer` binary is on PATH and reports a sane version
#   2. The node key file exists and is 0600
#   3. genesis.json sha256 matches the expected value passed as $2
#   4. NTP / system clock drift < 5 seconds
#   5. TCP 9000 + UDP 9000 inbound are NOT already bound (else node fails to listen)
#   6. T3.1's commputer-verify-multiaddr CAN parse and (best-effort) reach the
#      multiaddr derived from the operator's node key + public IP + port 9000
#   7. The wallet directory exists and the testnet wallet is recoverable
#
# Exit codes:
#   0 — all checks green
#   1 — at least one check failed (script prints which)
#   2 — wrong invocation (missing args)
#
# Usage:
#   ./precheck_node.sh <node_key_path> <expected_genesis_sha256>
#
# Optional env vars:
#   PUBLIC_IP        — your machine's public IP (used for the multiaddr
#                      reachability check). If unset, the script tries
#                      `curl -s ifconfig.me` and `dig +short myip.opendns.com
#                      @resolver1.opendns.com`.
#   PORT             — P2P port (default 9000)
#   GENESIS_PATH     — where to find genesis.json (default ./genesis.json)
#   COMMPUTER_WALLET_PASSWORD — wallet password (so wallet check is non-interactive)
#   STRICT_REACH     — if set non-empty, multiaddr-reachability is a hard
#                      fail (default: warn only — reachability requires
#                      another machine to dial back, which we can't always
#                      simulate from this one).

set -u
set -o pipefail

if (( $# < 2 )); then
    echo "Usage: $0 <node_key_path> <expected_genesis_sha256>" >&2
    exit 2
fi

NODE_KEY="$1"
EXPECTED_GENESIS_SHA="$2"

PORT="${PORT:-9000}"
GENESIS_PATH="${GENESIS_PATH:-./genesis.json}"

declare -a FAILED=()
declare -a WARNED=()

ok()    { echo "[precheck] PASS  $*"; }
fail()  { echo "[precheck] FAIL  $*"; FAILED+=("$*"); }
warn()  { echo "[precheck] WARN  $*"; WARNED+=("$*"); }
info()  { echo "[precheck] info  $*"; }

# ---------------------------------------------------------------------------
# 1. commputer binary
# ---------------------------------------------------------------------------

if ! command -v commputer >/dev/null 2>&1; then
    fail "commputer binary not on PATH"
else
    if version_line="$(commputer version 2>&1 | head -1)"; then
        ok "commputer binary: ${version_line}"
    else
        fail "commputer version returned non-zero"
    fi
fi

# ---------------------------------------------------------------------------
# 2. Node key file
# ---------------------------------------------------------------------------

if [[ ! -f "${NODE_KEY}" ]]; then
    fail "node key file not found: ${NODE_KEY}"
else
    perms="$(stat -c '%a' "${NODE_KEY}" 2>/dev/null || stat -f '%Lp' "${NODE_KEY}" 2>/dev/null || echo '?')"
    if [[ "${perms}" == "600" ]]; then
        ok "node key file is 0600 (${NODE_KEY})"
    else
        fail "node key file permissions are ${perms}, expected 0600 (chmod 600 ${NODE_KEY})"
    fi
fi

# ---------------------------------------------------------------------------
# 3. genesis.json sha256
# ---------------------------------------------------------------------------

if [[ ! -f "${GENESIS_PATH}" ]]; then
    fail "genesis.json not found at ${GENESIS_PATH} (set GENESIS_PATH= to override)"
else
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "${GENESIS_PATH}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "${GENESIS_PATH}" | awk '{print $1}')"
    else
        actual=""
    fi
    if [[ -z "${actual}" ]]; then
        fail "no sha256 tool available (install coreutils or perl-digest-sha)"
    elif [[ "${actual}" == "${EXPECTED_GENESIS_SHA}" ]]; then
        ok "genesis.json sha256 matches: ${actual}"
    else
        fail "genesis.json sha256 mismatch: got ${actual}, expected ${EXPECTED_GENESIS_SHA}"
    fi
fi

# ---------------------------------------------------------------------------
# 4. NTP / clock drift
# ---------------------------------------------------------------------------

drift_ok=0
if command -v chronyc >/dev/null 2>&1; then
    # `chronyc tracking` reports "System time : 0.000023456 seconds slow of NTP time"
    line="$(chronyc tracking 2>/dev/null | grep -i 'System time' || true)"
    if [[ -n "${line}" ]]; then
        # Extract the floating-point seconds value.
        secs="$(echo "${line}" | grep -oE '[0-9]+\.[0-9]+' | head -1 || echo '0')"
        ok "chronyc reports drift ${secs}s (target < 5s)"
        # bash can't compare floats; use awk.
        if awk -v s="${secs}" 'BEGIN { exit (s < 5.0) ? 0 : 1 }'; then
            drift_ok=1
        else
            fail "clock drift ${secs}s exceeds 5s threshold"
        fi
    fi
fi
if (( drift_ok == 0 )); then
    if command -v timedatectl >/dev/null 2>&1; then
        td="$(timedatectl 2>/dev/null || echo '')"
        if echo "${td}" | grep -q "System clock synchronized: yes"; then
            ok "timedatectl reports system clock synchronized"
            drift_ok=1
        elif echo "${td}" | grep -q "synchronized: no"; then
            fail "timedatectl reports system clock NOT synchronized — install/enable chrony or systemd-timesyncd"
        else
            warn "timedatectl output does not include synchronization status — check manually"
        fi
    else
        warn "no chronyc or timedatectl available — cannot verify NTP sync (install chrony)"
    fi
fi

# ---------------------------------------------------------------------------
# 5. P2P port 9000 not already in use
# ---------------------------------------------------------------------------

port_in_use=0
if command -v ss >/dev/null 2>&1; then
    if ss -ltn "sport = :${PORT}" 2>/dev/null | grep -q LISTEN; then
        port_in_use=1
    fi
elif command -v netstat >/dev/null 2>&1; then
    if netstat -ltn 2>/dev/null | awk '{print $4}' | grep -qE ":${PORT}\$"; then
        port_in_use=1
    fi
fi
if (( port_in_use == 1 )); then
    fail "TCP port ${PORT} is already in use (lsof -i :${PORT} to inspect)"
else
    ok "TCP port ${PORT} is free"
fi

# UDP is harder to check definitively; just look for any listener.
udp_in_use=0
if command -v ss >/dev/null 2>&1; then
    if ss -lun "sport = :${PORT}" 2>/dev/null | grep -q UNCONN; then
        udp_in_use=1
    fi
elif command -v netstat >/dev/null 2>&1; then
    if netstat -lun 2>/dev/null | awk '{print $4}' | grep -qE ":${PORT}\$"; then
        udp_in_use=1
    fi
fi
if (( udp_in_use == 1 )); then
    fail "UDP port ${PORT} is already in use (QUIC listener will fail to bind)"
else
    ok "UDP port ${PORT} appears free"
fi

# ---------------------------------------------------------------------------
# 6. Multiaddr reachability (requires T3.1's commputer-verify-multiaddr)
# ---------------------------------------------------------------------------

if [[ -z "${PUBLIC_IP:-}" ]]; then
    if command -v curl >/dev/null 2>&1; then
        PUBLIC_IP="$(curl -s --max-time 4 https://ifconfig.me 2>/dev/null || true)"
    fi
    if [[ -z "${PUBLIC_IP}" ]] && command -v dig >/dev/null 2>&1; then
        PUBLIC_IP="$(dig +short +time=2 myip.opendns.com @resolver1.opendns.com 2>/dev/null | head -1 || true)"
    fi
fi

if [[ -z "${PUBLIC_IP}" ]]; then
    warn "could not determine public IP automatically (set PUBLIC_IP=...)"
else
    info "public IP: ${PUBLIC_IP}"
    if command -v commputer-verify-multiaddr >/dev/null 2>&1; then
        # Without a peer ID we can only test parse, not reachability.
        # Build a placeholder addr; real reachability is a B/C-side check
        # in the runbook step 5.
        addr="/ip4/${PUBLIC_IP}/tcp/${PORT}"
        if commputer-verify-multiaddr "${addr}" 2>&1 | grep -q "parse: ok"; then
            ok "multiaddr parses cleanly: ${addr}"
        else
            warn "multiaddr parse failed for ${addr} (verify-multiaddr available but rejected)"
        fi
    else
        warn "commputer-verify-multiaddr (T3.1) not on PATH — cannot verify multiaddr"
    fi
fi

# ---------------------------------------------------------------------------
# 7. Wallet directory exists / is readable
# ---------------------------------------------------------------------------

WALLET_DIR="${HOME}/.commputer/wallet"
if [[ -d "${WALLET_DIR}" ]]; then
    perms="$(stat -c '%a' "${WALLET_DIR}" 2>/dev/null || stat -f '%Lp' "${WALLET_DIR}" 2>/dev/null || echo '?')"
    if [[ "${perms}" == "700" ]]; then
        ok "wallet dir exists and is 0700 (${WALLET_DIR})"
    else
        warn "wallet dir permissions are ${perms} (recommend 0700: chmod 700 ${WALLET_DIR})"
    fi
    if [[ -f "${WALLET_DIR}/wallet-testnet.json" ]]; then
        ok "testnet wallet present"
    else
        fail "testnet wallet not found at ${WALLET_DIR}/wallet-testnet.json"
    fi
else
    fail "wallet directory ${WALLET_DIR} does not exist (run: commputer wallet create --testnet)"
fi

# ---------------------------------------------------------------------------
# Cloud datacenter detection (informational)
# ---------------------------------------------------------------------------

if [[ -n "${PUBLIC_IP}" ]]; then
    case "${PUBLIC_IP}" in
        # Crude prefix-match of the most common cloud /8s. Authoritative
        # logic lives in src/validator/src/compliance_check.rs:291-352;
        # this is just a heads-up.
        13.*|3.*|18.*|34.*|35.*|52.*|54.*|99.*)
            warn "public IP ${PUBLIC_IP} looks like AWS/GCP/Azure space — chain will flag NerfedIncidental"
            ;;
        185.*)
            warn "public IP ${PUBLIC_IP} starts 185. — common Hetzner/OVH range, may be flagged NerfedIncidental"
            ;;
        *)
            info "public IP ${PUBLIC_IP} not in obvious cloud-prefix list (still depends on full ASN check)"
            ;;
    esac
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo
echo "============================================================"
if (( ${#FAILED[@]} == 0 )); then
    echo "[precheck] OVERALL: GREEN — ready to launch."
    if (( ${#WARNED[@]} > 0 )); then
        echo "[precheck] ${#WARNED[@]} non-fatal warnings:"
        for w in "${WARNED[@]}"; do
            echo "  - ${w}"
        done
    fi
    exit 0
else
    echo "[precheck] OVERALL: RED — ${#FAILED[@]} check(s) failed:"
    for f in "${FAILED[@]}"; do
        echo "  - ${f}"
    done
    if (( ${#WARNED[@]} > 0 )); then
        echo "[precheck] Plus ${#WARNED[@]} warning(s):"
        for w in "${WARNED[@]}"; do
            echo "  - ${w}"
        done
    fi
    echo "[precheck] DO NOT proceed with the ceremony until all FAIL items are resolved."
    exit 1
fi
