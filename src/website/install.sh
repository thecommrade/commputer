#!/usr/bin/env sh
# Commputer installer — detects OS/arch and installs the commputer binary
# Usage: curl -sSf https://commputer.xyz/install.sh | sh
set -eu

REPO="thecommrade/commputer"
INSTALL_DIR="${COMMPUTER_INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="commputer"
VERSION="${COMMPUTER_VERSION:-}"

main() {
    echo ""
    echo "  ╔═══════════════════════════════════════════╗"
    echo "  ║         COMMPUTER INSTALLER               ║"
    echo "  ╚═══════════════════════════════════════════╝"
    echo ""

    # GATE: no prebuilt binaries are published yet — remove this block when the
    # first tagged release ships with commputer-{linux,macos}-{x86_64,aarch64} assets.
    echo "  Prebuilt binaries are not published yet."
    echo "  Build from source instead (requires Rust — rustup.rs):"
    echo ""
    echo "    git clone https://github.com/$REPO && cd commputer && cargo build --release"
    echo ""
    echo "  The binary lands at target/release/commputer."
    exit 1

    # Check for required tools
    if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
        echo "ERROR: curl or wget is required but neither was found."
        echo ""
        echo "Install curl:"
        echo "  Ubuntu/Debian: sudo apt install curl"
        echo "  Fedora/RHEL:   sudo dnf install curl"
        echo "  macOS:         brew install curl"
        exit 1
    fi

    if command -v curl >/dev/null 2>&1; then
        FETCH="curl -sSfL"
        DOWNLOAD="curl -sSfL -o"
    else
        FETCH="wget -qO-"
        DOWNLOAD="wget -qO"
    fi

    # Check if already installed
    if command -v "$BINARY_NAME" >/dev/null 2>&1; then
        EXISTING_VERSION=$("$BINARY_NAME" --version 2>/dev/null | head -1 || echo "unknown")
        echo "Commputer is already installed: $EXISTING_VERSION"
        echo "Updating..."
        echo ""
    fi

    # Detect OS
    OS="$(uname -s)"
    case "$OS" in
        Linux)  OS="linux" ;;
        Darwin) OS="macos" ;;
        *)
            echo "ERROR: Unsupported operating system: $OS"
            echo "Commputer currently supports Linux and macOS."
            exit 1
            ;;
    esac

    # Detect architecture
    ARCH="$(uname -m)"
    case "$ARCH" in
        x86_64|amd64)   ARCH="x86_64" ;;
        aarch64|arm64)   ARCH="aarch64" ;;
        *)
            echo "ERROR: Unsupported architecture: $ARCH"
            echo "Commputer currently supports x86_64 and aarch64 (ARM64)."
            exit 1
            ;;
    esac

    echo "  Platform: $OS $ARCH"

    # Determine version to install
    if [ -z "$VERSION" ]; then
        echo "  Fetching latest release..."
        LATEST_TAG=$($FETCH "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//' || true)

        if [ -z "$LATEST_TAG" ]; then
            # Fallback to known version (update this manually when new versions are released)
            LATEST_TAG="v0.1.0"
            echo "  WARNING: Could not fetch latest release info — using $LATEST_TAG (fallback)"
            echo "  If this is an old version, check https://github.com/$REPO/releases"
        fi
        VERSION="$LATEST_TAG"
    fi

    echo "  Version:  $VERSION"

    # Build download URLs
    FILENAME="${BINARY_NAME}-${OS}-${ARCH}"
    BASE_URL="https://github.com/$REPO/releases/download/$VERSION"
    BINARY_URL="$BASE_URL/$FILENAME"
    CHECKSUM_URL="$BASE_URL/checksums-sha256.txt"

    # Create install directory
    mkdir -p "$INSTALL_DIR"

    # Check write permission
    if [ ! -w "$INSTALL_DIR" ]; then
        echo ""
        echo "ERROR: No write permission to $INSTALL_DIR"
        echo ""
        echo "Either:"
        echo "  1. Run with sudo: curl -sSf https://commputer.xyz/install.sh | sudo sh"
        echo "  2. Change install dir: COMMPUTER_INSTALL_DIR=/usr/local/bin curl -sSf https://commputer.xyz/install.sh | sh"
        exit 1
    fi

    # Download binary
    TMPFILE="$(mktemp)"
    echo "  Downloading binary..."
    if ! $DOWNLOAD "$TMPFILE" "$BINARY_URL" 2>/dev/null; then
        rm -f "$TMPFILE"
        echo ""
        echo "ERROR: Failed to download $BINARY_URL"
        echo "Check https://github.com/$REPO/releases for available downloads."
        exit 1
    fi

    # Item 13: Verify sha256 checksum
    echo "  Verifying checksum..."
    CHECKSUMS="$($FETCH "$CHECKSUM_URL" 2>/dev/null || true)"
    if [ -n "$CHECKSUMS" ]; then
        EXPECTED=$(echo "$CHECKSUMS" | grep "$FILENAME" | awk '{print $1}')
        if [ -n "$EXPECTED" ]; then
            if command -v sha256sum >/dev/null 2>&1; then
                ACTUAL=$(sha256sum "$TMPFILE" | awk '{print $1}')
            elif command -v shasum >/dev/null 2>&1; then
                ACTUAL=$(shasum -a 256 "$TMPFILE" | awk '{print $1}')
            else
                ACTUAL=""
                echo "  WARNING: sha256sum not available, skipping checksum verification"
            fi

            if [ -n "$ACTUAL" ]; then
                if [ "$ACTUAL" = "$EXPECTED" ]; then
                    echo "  Checksum verified ✓"
                else
                    rm -f "$TMPFILE"
                    echo ""
                    echo "ERROR: Checksum mismatch!"
                    echo "  Expected: $EXPECTED"
                    echo "  Got:      $ACTUAL"
                    echo ""
                    echo "The download may be corrupted. Try again or download manually."
                    exit 1
                fi
            fi
        else
            echo "  WARNING: No checksum found for $FILENAME, skipping verification"
        fi
    else
        echo "  WARNING: Could not fetch checksums, skipping verification"
    fi

    # Install binary
    chmod +x "$TMPFILE"
    mv "$TMPFILE" "$INSTALL_DIR/$BINARY_NAME"

    echo ""
    echo "  Installed to: $INSTALL_DIR/$BINARY_NAME"

    # Check if install dir is in PATH
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            ;;
        *)
            echo ""
            echo "  NOTE: $INSTALL_DIR is not in your PATH."
            echo "  Add it with:"
            echo ""
            echo "    export PATH=\"$INSTALL_DIR:\$PATH\""
            echo ""
            echo "  Or add that line to your ~/.bashrc or ~/.zshrc"
            ;;
    esac

    echo ""
    echo "  Commputer installed. Run 'commputer run --testnet' to start mining."
    echo ""
    echo "  Quick start:"
    echo "    commputer run --testnet    # Start mining on testnet"
    echo ""
    echo "  Welcome to the communal supercomputer."
    echo ""
}

main
