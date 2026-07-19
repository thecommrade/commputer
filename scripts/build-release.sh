#!/usr/bin/env bash
# Build release binaries for linux-x86_64 and linux-aarch64
# Output to dist/ with sha256 checksums
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SRC_DIR="$PROJECT_ROOT/src"
DIST_DIR="$PROJECT_ROOT/dist"

# VERSION SOURCE OF TRUTH: src/Cargo.toml [workspace.package] version (same
# field release.yml's verify-version job checks the tag against). Derive it so
# this script can't silently drift from the crate; fall back to a hardcoded
# value only if the grep fails for some reason (keep the fallback in sync with
# src/Cargo.toml manually if that ever happens).
VERSION="$(grep -m1 -E '^version' "$SRC_DIR/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
if [ -z "$VERSION" ]; then
    VERSION="0.1.0-alpha.1"
    echo "WARNING: could not read version from $SRC_DIR/Cargo.toml, using fallback $VERSION"
fi

echo "=== Commputer Release Build ==="
echo "Version: $VERSION"
echo ""

# Clean dist directory
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

cd "$SRC_DIR"

# Build for native target (linux-x86_64 on most dev machines)
TARGETS=()

# Detect current architecture
NATIVE_ARCH="$(uname -m)"
case "$NATIVE_ARCH" in
    x86_64|amd64)
        TARGETS+=("x86_64-unknown-linux-gnu")
        ;;
    aarch64|arm64)
        TARGETS+=("aarch64-unknown-linux-gnu")
        ;;
esac

# Check for cross-compilation targets
if rustup target list --installed | grep -q "x86_64-unknown-linux-gnu" && [ "$NATIVE_ARCH" != "x86_64" ]; then
    TARGETS+=("x86_64-unknown-linux-gnu")
fi
if rustup target list --installed | grep -q "aarch64-unknown-linux-gnu" && [ "$NATIVE_ARCH" != "aarch64" ]; then
    TARGETS+=("aarch64-unknown-linux-gnu")
fi

# Remove duplicates
TARGETS=($(printf '%s\n' "${TARGETS[@]}" | sort -u))

for TARGET in "${TARGETS[@]}"; do
    echo "Building for $TARGET..."

    # Map target to our naming convention
    case "$TARGET" in
        x86_64-unknown-linux-gnu)
            OUT_NAME="commputer-linux-x86_64"
            ;;
        aarch64-unknown-linux-gnu)
            OUT_NAME="commputer-linux-aarch64"
            ;;
        *)
            OUT_NAME="commputer-${TARGET}"
            ;;
    esac

    cargo build --release --target "$TARGET" -p commputer 2>&1

    # Copy binary to dist
    BINARY="target/$TARGET/release/commputer"
    if [ -f "$BINARY" ]; then
        cp "$BINARY" "$DIST_DIR/$OUT_NAME"
        echo "  -> $DIST_DIR/$OUT_NAME"
    else
        echo "  WARNING: Binary not found at $BINARY"
        # Try without target prefix (native build)
        if [ -f "target/release/commputer" ]; then
            cp "target/release/commputer" "$DIST_DIR/$OUT_NAME"
            echo "  -> $DIST_DIR/$OUT_NAME (from native build)"
        fi
    fi
done

# If no cross targets available, build native release
if [ ${#TARGETS[@]} -eq 0 ]; then
    echo "No cross-compilation targets installed. Building native release..."
    cargo build --release -p commputer
    case "$NATIVE_ARCH" in
        x86_64|amd64) OUT_NAME="commputer-linux-x86_64" ;;
        aarch64|arm64) OUT_NAME="commputer-linux-aarch64" ;;
        *) OUT_NAME="commputer-linux-$NATIVE_ARCH" ;;
    esac
    cp "target/release/commputer" "$DIST_DIR/$OUT_NAME"
    echo "  -> $DIST_DIR/$OUT_NAME"
fi

# Generate checksums
echo ""
echo "Generating checksums..."
cd "$DIST_DIR"
sha256sum commputer-* > checksums-sha256.txt 2>/dev/null || shasum -a 256 commputer-* > checksums-sha256.txt
cat checksums-sha256.txt

echo ""
echo "=== Build complete ==="
echo "Binaries in: $DIST_DIR"
ls -lh "$DIST_DIR"/commputer-*
