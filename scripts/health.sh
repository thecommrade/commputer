#!/bin/bash
# Item 47: Multi-node health check script
# Usage: ./scripts/health.sh [user@host1] [user@host2] ...
#   Or set COMMPUTER_HOSTS_FILE to a file containing one host per line.
#   Queries each node's /health endpoint and reports a status table.

set -euo pipefail

RPC_PORT="${COMMPUTER_RPC_PORT:-9944}"
HOSTS_FILE="${COMMPUTER_HOSTS_FILE:-}"
TIMEOUT="${HEALTH_TIMEOUT:-5}"

# Collect hosts from arguments or hosts file
HOSTS=()
if [ "$#" -ge 1 ]; then
    HOSTS=("$@")
elif [ -n "$HOSTS_FILE" ] && [ -f "$HOSTS_FILE" ]; then
    while IFS= read -r line; do
        line=$(echo "$line" | sed 's/#.*//' | xargs)
        [ -n "$line" ] && HOSTS+=("$line")
    done < "$HOSTS_FILE"
else
    # Default: check localhost
    HOSTS=("127.0.0.1")
fi

echo "=== Commputer Health Check ==="
echo "Nodes: ${#HOSTS[@]}"
echo "RPC Port: ${RPC_PORT}"
echo ""

# Print table header
printf "%-30s %-10s %-10s %-8s %-8s %-10s\n" "HOST" "STATUS" "HEIGHT" "EPOCH" "PEERS" "PENDING"
printf "%-30s %-10s %-10s %-8s %-8s %-10s\n" "------------------------------" "----------" "----------" "--------" "--------" "----------"

TOTAL=0
HEALTHY=0
UNHEALTHY=0

for host in "${HOSTS[@]}"; do
    TOTAL=$((TOTAL + 1))
    IP=$(echo "$host" | cut -d@ -f2)
    BASE_URL="http://${IP}:${RPC_PORT}"

    # Check health endpoint
    HEALTH=$(curl -s --connect-timeout "$TIMEOUT" --max-time "$TIMEOUT" "${BASE_URL}/health" 2>/dev/null || echo "")
    if [ -z "$HEALTH" ]; then
        printf "%-30s %-10s %-10s %-8s %-8s %-10s\n" "$host" "DOWN" "-" "-" "-" "-"
        UNHEALTHY=$((UNHEALTHY + 1))
        continue
    fi

    # Query status endpoint
    STATUS=$(curl -s --connect-timeout "$TIMEOUT" --max-time "$TIMEOUT" "${BASE_URL}/status" 2>/dev/null || echo "{}")
    HEIGHT=$(echo "$STATUS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('height', '?'))" 2>/dev/null || echo "?")
    EPOCH=$(echo "$STATUS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('epoch', '?'))" 2>/dev/null || echo "?")

    # Query metrics endpoint
    METRICS=$(curl -s --connect-timeout "$TIMEOUT" --max-time "$TIMEOUT" "${BASE_URL}/metrics" 2>/dev/null || echo "{}")
    PEERS=$(echo "$METRICS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('peers_connected', '?'))" 2>/dev/null || echo "?")
    PENDING=$(echo "$METRICS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('pending_txs', '?'))" 2>/dev/null || echo "?")

    printf "%-30s %-10s %-10s %-8s %-8s %-10s\n" "$host" "OK" "$HEIGHT" "$EPOCH" "$PEERS" "$PENDING"
    HEALTHY=$((HEALTHY + 1))
done

echo ""
echo "=== Summary ==="
echo "Total: ${TOTAL}  Healthy: ${HEALTHY}  Unhealthy: ${UNHEALTHY}"

if [ "$UNHEALTHY" -gt 0 ]; then
    exit 1
fi
