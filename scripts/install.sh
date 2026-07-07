#!/bin/bash
# Item 93: Linux installer script
# Usage: curl -sSL https://raw.githubusercontent.com/thecommrade/commputer/main/scripts/install.sh | bash

set -euo pipefail

INSTALL_DIR="${COMMPUTER_INSTALL_DIR:-/usr/local/bin}"
DATA_DIR="${HOME}/.commputer"

echo "=== Installing Commputer ==="

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)  ARCH_NAME="x86_64" ;;
    aarch64) ARCH_NAME="aarch64" ;;
    *)
        echo "Unsupported architecture: ${ARCH}"
        exit 1
        ;;
esac

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
echo "Platform: ${OS}-${ARCH_NAME}"

# Check for existing installation
if command -v commputer &>/dev/null; then
    CURRENT=$(commputer --version 2>/dev/null || echo "unknown")
    echo "Existing installation found: ${CURRENT}"
fi

# Create data directory
mkdir -p "${DATA_DIR}/wallet"
echo "Data directory: ${DATA_DIR}"

# For now, build from source (binary releases will be added later)
if ! command -v cargo &>/dev/null; then
    echo "Rust is required. Install via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

echo "Building from source..."
TEMP_DIR=$(mktemp -d)
cd "${TEMP_DIR}"
git clone --depth 1 https://github.com/thecommrade/commputer.git
cd commputer/src
cargo build --release -p commputer

# Install binary
if [ -w "${INSTALL_DIR}" ]; then
    cp target/release/commputer "${INSTALL_DIR}/"
else
    sudo cp target/release/commputer "${INSTALL_DIR}/"
fi

# Cleanup
rm -rf "${TEMP_DIR}"

echo ""
echo "=== Installation complete ==="
echo "Binary: ${INSTALL_DIR}/commputer"
echo "Data:   ${DATA_DIR}"
echo ""
echo "Quick start:"
echo "  commputer wallet create"
echo "  commputer run --testnet"
