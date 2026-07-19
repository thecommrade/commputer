#!/bin/bash
# Item 93: from-source installer script (Linux + macOS).
#
# NOTE: this script always BUILDS FROM SOURCE (git clone + cargo build). It does
# NOT download the prebuilt release.yml binaries — for a binary install, use the
# website installer instead: `curl -sSf https://commputer.xyz/install.sh | sh`
# (see src/website/install.sh; asset names commputer-{linux,macos}-{x86_64,aarch64}).
# This script's OS/ARCH detection is kept aligned with that naming convention
# purely for consistent platform labeling in its own output.
#
# Usage: curl -sSL https://raw.githubusercontent.com/thecommrade/commputer/main/scripts/install.sh | bash

set -euo pipefail

INSTALL_DIR="${COMMPUTER_INSTALL_DIR:-/usr/local/bin}"
DATA_DIR="${HOME}/.commputer"

echo "=== Installing Commputer ==="

# Detect architecture. `uname -m` reports "arm64" on Apple Silicon and
# "aarch64" on most Linux ARM64 systems — both map to the same "aarch64" name
# so Apple Silicon is never rejected here.
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)         ARCH_NAME="x86_64" ;;
    aarch64|arm64)  ARCH_NAME="aarch64" ;;
    *)
        echo "Unsupported architecture: ${ARCH}"
        exit 1
        ;;
esac

# Detect OS. Darwin -> macos to match the release asset naming convention;
# Linux behavior is unchanged from before.
UNAME_S=$(uname -s)
case "$UNAME_S" in
    Linux)  OS="linux" ;;
    Darwin) OS="macos" ;;
    *)      OS=$(echo "$UNAME_S" | tr '[:upper:]' '[:lower:]') ;;
esac
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
