#!/bin/bash
# Item 88: Testnet chaos test — random kill/restart, verify integrity
# Usage: ./scripts/testnet-chaos.sh [iterations] [rpc_port]

ITERATIONS="${1:-5}"
RPC_PORT="${2:-9944}"
BASE_URL="http://127.0.0.1:${RPC_PORT}"
BINARY="./target/release/commputer"

echo "=== Commputer Chaos Test ==="
echo "Iterations: ${ITERATIONS}"
echo ""

for ((i=1; i<=ITERATIONS; i++)); do
    echo "--- Iteration ${i}/${ITERATIONS} ---"

    # Get current height
    HEIGHT_BEFORE=$(curl -s "${BASE_URL}/status" | python3 -c "import sys,json; print(json.load(sys.stdin).get('height', 0))" 2>/dev/null || echo "0")
    echo "Height before kill: ${HEIGHT_BEFORE}"

    # Kill the node
    echo "Killing node..."
    pkill -f "commputer run" 2>/dev/null || true
    sleep 2

    # Restart
    echo "Restarting node..."
    cd ~/Coin/src
    nohup cargo run --release -p commputer -- run --testnet --port 9000 --rpc-port "${RPC_PORT}" > /dev/null 2>&1 &
    sleep 5

    # Check health
    HEALTH=$(curl -s "${BASE_URL}/health" 2>/dev/null)
    if [ -z "$HEALTH" ]; then
        echo "FAIL: Node did not restart"
        continue
    fi

    # Verify height is >= before
    HEIGHT_AFTER=$(curl -s "${BASE_URL}/status" | python3 -c "import sys,json; print(json.load(sys.stdin).get('height', 0))" 2>/dev/null || echo "0")
    echo "Height after restart: ${HEIGHT_AFTER}"

    if [ "$HEIGHT_AFTER" -lt "$HEIGHT_BEFORE" ]; then
        echo "FAIL: Height decreased after restart! Data loss detected."
    else
        echo "PASS: Height preserved."
    fi

    echo ""
    sleep 3
done

echo "=== Chaos test complete ==="
