#!/bin/bash
# Item 92: Automated release pipeline script
# Usage: ./scripts/release.sh <version>

set -euo pipefail

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
    echo "Usage: $0 <version> (e.g., 0.1.0)"
    exit 1
fi

echo "=== Commputer Release Pipeline v${VERSION} ==="

# Step 1: Run tests
echo "--- Step 1: Running tests ---"
cd "$(dirname "$0")/../src"
cargo test --workspace
echo "All tests passed."

# Step 2: Build release binary
echo "--- Step 2: Building release binary ---"
cargo build --release -p commputer
BINARY="target/release/commputer"
echo "Binary: ${BINARY} ($(du -h "${BINARY}" | cut -f1))"

# Step 3: Create release directory
RELEASE_DIR="../releases/v${VERSION}"
mkdir -p "${RELEASE_DIR}"

# Step 4: Copy artifacts
cp "${BINARY}" "${RELEASE_DIR}/commputer"
cp ../deploy/commputer.service "${RELEASE_DIR}/"
cp ../deploy/commputer.toml "${RELEASE_DIR}/"

# Step 5: Create checksum
cd "${RELEASE_DIR}"
sha256sum commputer > checksums.sha256

# Step 6: Create tarball
cd ..
tar -czf "commputer-v${VERSION}-linux-x86_64.tar.gz" "v${VERSION}/"

echo ""
echo "=== Release v${VERSION} complete ==="
echo "Artifacts in: releases/"
echo "Tarball: releases/commputer-v${VERSION}-linux-x86_64.tar.gz"
