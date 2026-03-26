#!/usr/bin/env sh
# Commputer installer — detects OS/arch and installs the commputer binary
# Usage: curl -sSf https://commputer.xyz/install.sh | sh
set -eu

REPO="thecommrade/commputer"
INSTALL_DIR="${COMMPUTER_INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="commputer"

main() {
    echo "Commputer Installer"
    echo "==================="
    echo ""

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
            echo "Commputer currently supports x86_64 and aarch64."
            exit 1
            ;;
    esac

    echo "Detected: $OS $ARCH"

    # Get latest release tag
    if command -v curl >/dev/null 2>&1; then
        FETCH="curl -sSfL"
    elif command -v wget >/dev/null 2>&1; then
        FETCH="wget -qO-"
    else
        echo "ERROR: curl or wget is required."
        exit 1
    fi

    echo "Fetching latest release..."
    LATEST_TAG=$($FETCH "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')

    if [ -z "$LATEST_TAG" ]; then
        echo "ERROR: Could not determine latest release."
        echo "Check https://github.com/$REPO/releases manually."
        exit 1
    fi

    echo "Latest version: $LATEST_TAG"

    # Build download URL
    FILENAME="${BINARY_NAME}-${OS}-${ARCH}"
    URL="https://github.com/$REPO/releases/download/$LATEST_TAG/$FILENAME"

    # Create install directory
    mkdir -p "$INSTALL_DIR"

    # Download
    echo "Downloading $URL ..."
    TMPFILE="$(mktemp)"
    if command -v curl >/dev/null 2>&1; then
        curl -sSfL "$URL" -o "$TMPFILE"
    else
        wget -qO "$TMPFILE" "$URL"
    fi

    # Install
    chmod +x "$TMPFILE"
    mv "$TMPFILE" "$INSTALL_DIR/$BINARY_NAME"

    echo ""
    echo "Installed to: $INSTALL_DIR/$BINARY_NAME"

    # Check if install dir is in PATH
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            echo ""
            echo "NOTE: $INSTALL_DIR is not in your PATH."
            echo "Add it with:"
            echo ""
            echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
            echo ""
            echo "Or add that line to your ~/.bashrc or ~/.zshrc"
            ;;
    esac

    echo ""
    echo "Get started:"
    echo "  commputer wallet create"
    echo "  commputer run --testnet"
    echo ""
    echo "Welcome to the communal supercomputer."
}

main
