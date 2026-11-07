#!/usr/bin/env bash
# Build the browser bundle into dist/: dist/pkg/shotstage_app{.js,_bg.wasm} + dist/index.html.
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli (same version
# as the wasm-bindgen crate in Cargo.lock).
set -euo pipefail
cd "$(dirname "$0")/.."
profile="${PROFILE:-wasm-release}"
cargo build -p shotstage-app --lib --target wasm32-unknown-unknown --profile "$profile"
rm -rf dist && mkdir -p dist/pkg
wasm-bindgen --target web --no-typescript --out-dir dist/pkg \
  "target/wasm32-unknown-unknown/$profile/shotstage_app.wasm"
cp web/index.html web/bridge.js dist/
ls -lh dist/pkg
echo "serve dist/ with any static server, e.g. python3 -m http.server -d dist 8080"
