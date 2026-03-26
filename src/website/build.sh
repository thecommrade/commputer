#!/usr/bin/env bash
# Build script: minify CSS and JS for production
# Outputs minified versions to dist/
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DIST="$SCRIPT_DIR/dist"

mkdir -p "$DIST"

minify_css() {
    local input="$1"
    local output="$2"
    # Remove comments, collapse whitespace, trim
    sed 's|/\*[^*]*\*\+\([^/][^*]*\*\+\)*/||g' "$input" \
        | tr '\n' ' ' \
        | sed 's/  */ /g' \
        | sed 's/ *{ */{/g' \
        | sed 's/ *} */}/g' \
        | sed 's/ *: */:/g' \
        | sed 's/ *; */;/g' \
        | sed 's/ *, */,/g' \
        | sed 's/;}/}/g' \
        > "$output"
    echo "  CSS: $(wc -c < "$input") -> $(wc -c < "$output") bytes"
}

minify_js() {
    local input="$1"
    local output="$2"
    # Remove single-line comments (not URLs), collapse whitespace
    sed 's|//[^"'"'"']*$||g' "$input" \
        | sed '/^[[:space:]]*$/d' \
        | tr '\n' ' ' \
        | sed 's/  */ /g' \
        > "$output"
    echo "  JS:  $(wc -c < "$input") -> $(wc -c < "$output") bytes"
}

echo "Building production assets..."

# Copy HTML files
for f in "$SCRIPT_DIR"/*.html; do
    cp "$f" "$DIST/"
done

# Copy static assets
for f in favicon.svg robots.txt sitemap.xml stats.json; do
    [ -f "$SCRIPT_DIR/$f" ] && cp "$SCRIPT_DIR/$f" "$DIST/"
done

# Minify
minify_css "$SCRIPT_DIR/style.css" "$DIST/style.css"
minify_js "$SCRIPT_DIR/app.js" "$DIST/app.js"

echo ""
echo "Production build complete: $DIST/"
ls -lh "$DIST/"
