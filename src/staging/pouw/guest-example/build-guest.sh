#!/usr/bin/env bash
# Rebuilds src/wasm/fixtures/guest_example.wasm (spec §4 guest constraints):
#   (a) --initial-memory == --max-memory  -> memory min==max (gate rule 5)
#   (b) static bump arena, no dlmalloc    -> no memory.grow  (gate rule 4)
#   target-cpu=mvp -> plain integer MVP output (no post-MVP surprises).
# NOT required for `cargo test` — the artifact is checked in.
set -euo pipefail
cd "$(dirname "$0")"
rustup target add wasm32-unknown-unknown
RUSTFLAGS="-C target-cpu=mvp -C link-arg=--initial-memory=1048576 -C link-arg=--max-memory=1048576 -C link-arg=-zstack-size=131072" \
    cargo build --release --target wasm32-unknown-unknown
mkdir -p ../src/wasm/fixtures
cp target/wasm32-unknown-unknown/release/guest_example.wasm ../src/wasm/fixtures/guest_example.wasm
rustc -V
echo "rebuilt with $(rustc -V):"
sha256sum ../src/wasm/fixtures/guest_example.wasm
