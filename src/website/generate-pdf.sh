#!/usr/bin/env bash
# Generate commputer-whitepaper.pdf from WHITEPAPER.md
# Requires: pandoc and a LaTeX engine (pdflatex/xelatex)
# Install: sudo pacman -S pandoc texlive-core (Arch) or equivalent

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WHITEPAPER="$SCRIPT_DIR/../../protocol/whitepaper/WHITEPAPER.md"
OUTPUT="$SCRIPT_DIR/commputer-whitepaper.pdf"

if ! command -v pandoc &>/dev/null; then
    echo "ERROR: pandoc is required but not installed."
    echo "  Arch:   sudo pacman -S pandoc texlive-core"
    echo "  Debian: sudo apt install pandoc texlive-latex-recommended"
    echo "  macOS:  brew install pandoc basictex"
    exit 1
fi

echo "Generating PDF from $WHITEPAPER ..."

pandoc "$WHITEPAPER" \
    -o "$OUTPUT" \
    --pdf-engine=xelatex \
    -V geometry:margin=1in \
    -V fontsize=11pt \
    -V mainfont="DejaVu Sans" \
    -V monofont="DejaVu Sans Mono" \
    -V title="Commputer Whitepaper" \
    -V subtitle="\$COMME — A Communal Supercomputer" \
    -V date="2026" \
    -V colorlinks=true \
    -V linkcolor=teal \
    -V urlcolor=teal \
    --toc \
    --toc-depth=3 \
    --highlight-style=tango \
    -f markdown+smart

echo "Generated: $OUTPUT"
