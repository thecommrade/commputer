#!/bin/bash
# Item 86: Testnet health checker script
# Usage: ./scripts/testnet-health.sh [rpc_port]

RPC_PORT="${1:-9944}"
BASE_URL="http://127.0.0.1:${RPC_PORT}"

echo "=== Commputer Testnet Health Check ==="
echo "RPC: ${BASE_URL}"
echo ""

# Check health endpoint
HEALTH=$(curl -s "${BASE_URL}/health" 2>/dev/null)
if [ -z "$HEALTH" ]; then
    echo "CRITICAL: Node unreachable at ${BASE_URL}"
    exit 1
fi
echo "Health: ${HEALTH}"

# Check status
STATUS=$(curl -s "${BASE_URL}/status" 2>/dev/null)
if [ -n "$STATUS" ]; then
    HEIGHT=$(echo "$STATUS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('height', 'unknown'))" 2>/dev/null || echo "parse error")
    EPOCH=$(echo "$STATUS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('epoch', 'unknown'))" 2>/dev/null || echo "parse error")
    ACCOUNTS=$(echo "$STATUS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('accounts', 'unknown'))" 2>/dev/null || echo "parse error")
    echo "Height:   ${HEIGHT}"
    echo "Epoch:    ${EPOCH}"
    echo "Accounts: ${ACCOUNTS}"
fi

# Check metrics
METRICS=$(curl -s "${BASE_URL}/metrics" 2>/dev/null)
if [ -n "$METRICS" ]; then
    PEERS=$(echo "$METRICS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('peers_connected', 0))" 2>/dev/null || echo "0")
    PENDING=$(echo "$METRICS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('pending_txs', 0))" 2>/dev/null || echo "0")
    BANNED=$(echo "$METRICS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('peers_banned', 0))" 2>/dev/null || echo "0")
    echo "Peers:    ${PEERS}"
    echo "Pending:  ${PENDING}"
    echo "Banned:   ${BANNED}"

    if [ "$PEERS" -eq 0 ]; then
        echo "WARNING: No peers connected!"
    fi
fi

echo ""
echo "=== Health check complete ==="
