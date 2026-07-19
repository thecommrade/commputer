#!/usr/bin/env bash
set -euo pipefail

# Cross-compilation build script for Commputer
# Builds release binaries for multiple targets
#
# DEV HELPER — NOT the release pipeline. This script stages each build at
# dist/<target-triple>/commputer (e.g. dist/x86_64-apple-darwin/commputer),
# which does NOT match the asset naming used by src/staging/ops/release.yml or
# scripts/build-release.sh (commputer-<os>-<arch>, e.g. commputer-macos-x86_64,
# commputer-linux-aarch64). Kept intentionally unaligned: this script exists for
# a developer to sanity-check that all four targets still compile locally
# (including the two Darwin targets, which release.yml can only build on a real
# macOS runner) — it is not meant to produce release-ready, correctly-named
# assets. If you need release assets, use scripts/build-release.sh (Linux) or
# the release.yml build-macos/build-windows jobs (CI-hosted, correct names).

TARGETS=(
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-apple-darwin"
    "aarch64-apple-darwin"
)

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_DIR="$REPO_ROOT/src"
DIST_DIR="$REPO_ROOT/dist"

echo "=== Commputer Cross-Compilation Build ==="
echo "Source: $SRC_DIR"
echo "Output: $DIST_DIR"
echo ""

mkdir -p "$DIST_DIR"

for target in "${TARGETS[@]}"; do
    echo "--- Building for $target ---"

    # Ensure the target is installed
    rustup target add "$target" 2>/dev/null || true

    if cargo build --release --target "$target" --manifest-path "$SRC_DIR/Cargo.toml"; then
        # Copy binary to dist
        TARGET_DIR="$DIST_DIR/$target"
        mkdir -p "$TARGET_DIR"
        BINARY_PATH="$SRC_DIR/target/$target/release/commputer"
        if [ -f "$BINARY_PATH" ]; then
            cp "$BINARY_PATH" "$TARGET_DIR/"
            echo "  -> $TARGET_DIR/commputer"
        else
            echo "  -> Binary not found at $BINARY_PATH (may be a library-only build)"
        fi
    else
        echo "  -> FAILED (cross-linker may not be installed for $target)"
    fi

    echo ""
done

echo "=== Build complete. Artifacts in $DIST_DIR ==="
ls -R "$DIST_DIR" 2>/dev/null || echo "(no artifacts produced)"
