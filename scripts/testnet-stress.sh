#!/bin/bash
# Item 87: Testnet stress test script — N TPS for M minutes
# Usage: ./scripts/testnet-stress.sh [tps] [minutes] [rpc_port]

TPS="${1:-10}"
MINUTES="${2:-1}"
RPC_PORT="${3:-9944}"
BASE_URL="http://127.0.0.1:${RPC_PORT}"

TOTAL_TXS=$((TPS * MINUTES * 60))
DELAY_MS=$((1000 / TPS))

echo "=== Commputer Stress Test ==="
echo "Target TPS: ${TPS}"
echo "Duration:   ${MINUTES} minutes"
echo "Total txs:  ${TOTAL_TXS}"
echo "RPC:        ${BASE_URL}"
echo ""

# Check node is reachable
HEALTH=$(curl -s "${BASE_URL}/health" 2>/dev/null)
if [ -z "$HEALTH" ]; then
    echo "ERROR: Node unreachable"
    exit 1
fi

START_HEIGHT=$(curl -s "${BASE_URL}/status" | python3 -c "import sys,json; print(json.load(sys.stdin).get('height', 0))" 2>/dev/null || echo "0")

echo "Start height: ${START_HEIGHT}"
echo "Starting stress test..."

SENT=0
ERRORS=0
START_TIME=$(date +%s)

for ((i=1; i<=TOTAL_TXS; i++)); do
    # Submit a dummy transaction via RPC
    RESULT=$(curl -s -X POST "${BASE_URL}/tx" -H "Content-Type: application/json" \
        -d "{\"from\":\"$(printf '%064d' $i)\",\"nonce\":0,\"kind\":{\"Transfer\":{\"to\":\"$(printf '%064d' $((i+1)))\",\"amount\":1}},\"fee\":100000,\"signature\":\"\",\"public_key\":\"\"}" 2>/dev/null)

    if [ -n "$RESULT" ]; then
        SENT=$((SENT + 1))
    else
        ERRORS=$((ERRORS + 1))
    fi

    # Progress every 100 txs
    if [ $((i % 100)) -eq 0 ]; then
        ELAPSED=$(($(date +%s) - START_TIME))
        ACTUAL_TPS=$((SENT / (ELAPSED + 1)))
        echo "  Sent: ${SENT}/${TOTAL_TXS} (${ACTUAL_TPS} TPS, ${ERRORS} errors)"
    fi

    # Rate limiting
    sleep "0.$(printf '%03d' ${DELAY_MS})" 2>/dev/null || true
done

END_TIME=$(date +%s)
DURATION=$((END_TIME - START_TIME))
END_HEIGHT=$(curl -s "${BASE_URL}/status" | python3 -c "import sys,json; print(json.load(sys.stdin).get('height', 0))" 2>/dev/null || echo "0")

echo ""
echo "=== Stress Test Results ==="
echo "Duration:    ${DURATION}s"
echo "Sent:        ${SENT}"
echo "Errors:      ${ERRORS}"
echo "Actual TPS:  $((SENT / (DURATION + 1)))"
echo "Blocks:      $((END_HEIGHT - START_HEIGHT))"
