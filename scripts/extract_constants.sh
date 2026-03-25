#!/usr/bin/env bash
# Feature 250: Extract all protocol constants from the Commputer Rust codebase.
# Outputs a markdown table of: name, value, file, description.
#
# Usage: ./scripts/extract_constants.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$SCRIPT_DIR/../src"

echo "# Commputer Protocol Constants"
echo ""
echo "Extracted from source code on $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo ""
echo "| Name | Value | File | Description |"
echo "|------|-------|------|-------------|"

# Find all `pub const` declarations in .rs files under src/
find "$SRC_DIR" -name "*.rs" -not -path "*/target/*" | sort | while read -r file; do
    # Extract lines with `pub const` or `const` declarations
    grep -n 'pub const\|pub(crate) const' "$file" 2>/dev/null | while IFS=: read -r lineno line; do
        # Extract the constant name and value
        name=$(echo "$line" | sed -n 's/.*const \([A-Z_][A-Z0-9_]*\).*/\1/p')
        if [ -z "$name" ]; then
            continue
        fi

        # Extract the value (everything after = and before ;)
        value=$(echo "$line" | sed -n 's/.*= *\(.*\);.*/\1/p' | head -c 60)
        if [ -z "$value" ]; then
            value="(complex)"
        fi

        # Get relative path
        relpath=$(echo "$file" | sed "s|$SRC_DIR/||")

        # Try to extract a doc comment from the line above
        if [ "$lineno" -gt 1 ]; then
            prev_line=$((lineno - 1))
            desc=$(sed -n "${prev_line}p" "$file" | sed -n 's|.*/// *\(.*\)|\1|p' | head -c 80)
        else
            desc=""
        fi
        if [ -z "$desc" ]; then
            desc="-"
        fi

        echo "| \`$name\` | \`$value\` | $relpath:$lineno | $desc |"
    done
done
