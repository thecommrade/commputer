#!/bin/bash
# Item 46: Generic deploy script
# Usage: ./scripts/deploy.sh <user@host1> [user@host2] ...
# Builds the release binary, pushes via scp, and starts the node via ssh.

set -euo pipefail

BINARY_NAME="commputer"
BUILD_DIR="src"
PORT="${COMMPUTER_PORT:-9000}"
RPC_PORT="${COMMPUTER_RPC_PORT:-9944}"
REMOTE_DIR="${COMMPUTER_REMOTE_DIR:-\$HOME/commputer}"
HOSTS_FILE="${COMMPUTER_HOSTS_FILE:-}"

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
    echo "Usage: $0 <user@host1> [user@host2] ..."
    echo "  Or set COMMPUTER_HOSTS_FILE to a file containing one host per line."
    exit 1
fi

if [ "${#HOSTS[@]}" -eq 0 ]; then
    echo "Error: No hosts specified."
    exit 1
fi

echo "=== Building release binary ==="
cd "$(dirname "$0")/../${BUILD_DIR}"
cargo build --release -p commputer
BINARY="target/release/${BINARY_NAME}"

if [ ! -f "${BINARY}" ]; then
    echo "Build failed: ${BINARY} not found"
    exit 1
fi

echo "Binary size: $(du -h "${BINARY}" | cut -f1)"

# Collect seed multiaddrs from all hosts
SEEDS=""
for host in "${HOSTS[@]}"; do
    IP=$(echo "$host" | cut -d@ -f2)
    [ -n "$SEEDS" ] && SEEDS="${SEEDS},"
    SEEDS="${SEEDS}/ip4/${IP}/tcp/${PORT}"
done

echo "=== Deploying to ${#HOSTS[@]} hosts ==="
for host in "${HOSTS[@]}"; do
    echo "--- Deploying to ${host} ---"

    # Create remote directory
    ssh "${host}" "mkdir -p ~/commputer"

    # Stop any existing node gracefully
    ssh "${host}" "pkill -f '${BINARY_NAME} run' 2>/dev/null || true"
    sleep 1

    # Copy binary via scp
    scp "${BINARY}" "${host}:~/commputer/${BINARY_NAME}"

    # Make executable
    ssh "${host}" "chmod +x ~/commputer/${BINARY_NAME}"

    echo "Deployed to ${host}"
done

echo ""
echo "=== Starting nodes ==="
for host in "${HOSTS[@]}"; do
    echo "--- Starting node on ${host} ---"
    ssh "${host}" "cd ~/commputer && nohup ./${BINARY_NAME} run --testnet --port ${PORT} --rpc-port ${RPC_PORT} --seeds '${SEEDS}' > node.log 2>&1 &"
    echo "Node started on ${host}"
done

echo ""
echo "=== Deployment complete ==="
echo "Nodes: ${#HOSTS[@]}"
echo "Seeds: ${SEEDS}"
echo ""
echo "Check logs: ssh <host> 'tail -f ~/commputer/node.log'"
echo "Health:     curl http://<host>:${RPC_PORT}/health"
