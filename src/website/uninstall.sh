#!/usr/bin/env sh
# Commputer uninstaller — removes binary but preserves wallet data
# Usage: curl -sSf https://commputer.xyz/uninstall.sh | sh
set -eu

INSTALL_DIR="${COMMPUTER_INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="commputer"
WALLET_DIR="$HOME/.commputer/wallet"
DATA_DIR="$HOME/.commputer"

main() {
    echo ""
    echo "  Commputer Uninstaller"
    echo "  ====================="
    echo ""

    REMOVED=0

    # Remove binary
    if [ -f "$INSTALL_DIR/$BINARY_NAME" ]; then
        rm -f "$INSTALL_DIR/$BINARY_NAME"
        echo "  Removed: $INSTALL_DIR/$BINARY_NAME"
        REMOVED=1
    else
        echo "  Binary not found at $INSTALL_DIR/$BINARY_NAME"
    fi

    # Check common install locations
    for DIR in /usr/local/bin /usr/bin; do
        if [ -f "$DIR/$BINARY_NAME" ] && [ "$DIR" != "$INSTALL_DIR" ]; then
            echo "  NOTE: Binary also found at $DIR/$BINARY_NAME (not removed)"
        fi
    done

    if [ $REMOVED -eq 0 ]; then
        echo ""
        echo "  Commputer does not appear to be installed."
        exit 0
    fi

    echo ""
    echo "  Binary removed."
    echo ""

    # Warn about wallet data — never delete it automatically
    if [ -d "$WALLET_DIR" ]; then
        echo "  ⚠  Your wallet is still at: $WALLET_DIR"
        echo "  ⚠  This was NOT deleted. Your funds are safe."
        echo ""
        echo "  To remove wallet data (IRREVERSIBLE if no backup):"
        echo "    rm -rf $WALLET_DIR"
    fi

    if [ -d "$DATA_DIR" ]; then
        echo ""
        echo "  Chain data is at: $DATA_DIR"
        echo "  To remove all data: rm -rf $DATA_DIR"
    fi

    echo ""
    echo "  Commputer has been uninstalled."
    echo ""
}

main
