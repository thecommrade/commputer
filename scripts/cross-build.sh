#!/usr/bin/env bash
set -euo pipefail

# Cross-compilation build script for Commputer
# Builds release binaries for multiple targets

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
