#!/bin/bash
# Item 81: Testnet deployment script
# Usage: ./scripts/deploy-testnet.sh user@host1 user@host2 ...
# Builds the binary, scps to N machines, starts with seeds.

set -euo pipefail

BINARY_NAME="commputer"
BUILD_DIR="src"
PORT="${COMMPUTER_PORT:-9000}"
RPC_PORT="${COMMPUTER_RPC_PORT:-9944}"

if [ "$#" -lt 1 ]; then
    echo "Usage: $0 <user@host1> [user@host2] ..."
    exit 1
fi

HOSTS=("$@")

echo "=== Building release binary ==="
cd "$(dirname "$0")/../${BUILD_DIR}"
cargo build --release -p commputer
BINARY="target/release/${BINARY_NAME}"

if [ ! -f "${BINARY}" ]; then
    echo "Build failed: ${BINARY} not found"
    exit 1
fi

echo "Binary size: $(du -h "${BINARY}" | cut -f1)"

# Collect seed multiaddrs
SEEDS=""
for host in "${HOSTS[@]}"; do
    IP=$(echo "$host" | cut -d@ -f2)
    if [ -n "$SEEDS" ]; then
        SEEDS="${SEEDS},"
    fi
    SEEDS="${SEEDS}/ip4/${IP}/tcp/${PORT}"
done

echo "=== Deploying to ${#HOSTS[@]} hosts ==="
for host in "${HOSTS[@]}"; do
    echo "--- Deploying to ${host} ---"

    # Create remote directory
    ssh "${host}" "mkdir -p ~/commputer"

    # Copy binary
    scp "${BINARY}" "${host}:~/commputer/${BINARY_NAME}"

    # Make executable
    ssh "${host}" "chmod +x ~/commputer/${BINARY_NAME}"

    echo "Deployed to ${host}"
done

echo "=== Starting nodes ==="
FIRST_HOST="${HOSTS[0]}"
for host in "${HOSTS[@]}"; do
    echo "--- Starting node on ${host} ---"
    ssh "${host}" "cd ~/commputer && nohup ./${BINARY_NAME} run --testnet --port ${PORT} --rpc-port ${RPC_PORT} --seeds '${SEEDS}' > node.log 2>&1 &"
    echo "Node started on ${host}"
done

echo ""
echo "=== Deployment complete ==="
echo "Nodes: ${#HOSTS[@]}"
echo "Seeds: ${SEEDS}"
echo "Check logs: ssh <host> 'tail -f ~/commputer/node.log'"
