#!/usr/bin/env bash
# Build the browser bundle into dist/:
#   dist/index.html, bridge.js, share.js
#   dist/pkg/shotstage_app{.js,_bg.wasm}       the program
#   dist/assets/human.bin, NotoSansSC-subset.otf  big assets the page downloads separately
# index.html gets each file's fingerprint (`?v=...`), so browsers never mix two releases.
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli (same version
# as the wasm-bindgen crate in Cargo.lock).
set -euo pipefail
cd "$(dirname "$0")/.."
profile="${PROFILE:-wasm-release}"
cargo build -p shotstage-app --lib --target wasm32-unknown-unknown --profile "$profile"
rm -rf dist && mkdir -p dist/pkg dist/assets
# The function-name section is a quarter of the file and only helps a debugger.
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section --out-dir dist/pkg \
  "target/wasm32-unknown-unknown/$profile/shotstage_app.wasm"
cp web/bridge.js web/share.js dist/
cp crates/core/assets/human.bin assets/fonts/NotoSansSC-subset.otf assets/fonts/NotoSansSC-LICENSE.txt dist/assets/

fingerprint() {
  if command -v sha256sum >/dev/null; then cat "$@" | sha256sum | cut -c1-12; else cat "$@" | shasum -a 256 | cut -c1-12; fi
}
sed -e "s/__PKG_HASH__/$(fingerprint dist/pkg/shotstage_app.js dist/pkg/shotstage_app_bg.wasm)/g" \
    -e "s/__WASM_BYTES__/$(wc -c < dist/pkg/shotstage_app_bg.wasm | tr -d ' ')/g" \
    -e "s/__BRIDGE_HASH__/$(fingerprint dist/bridge.js)/g" \
    -e "s/__SHARE_HASH__/$(fingerprint dist/share.js)/g" \
    -e "s/__FONT_HASH__/$(fingerprint dist/assets/NotoSansSC-subset.otf)/g" \
    -e "s/__HUMAN_HASH__/$(fingerprint dist/assets/human.bin)/g" \
    web/index.html > dist/index.html
if grep -q '__[A-Z_]*__' dist/index.html; then
  echo "unfilled placeholder in dist/index.html" >&2
  exit 1
fi

ls -lh dist/pkg dist/assets
echo "program, gzip: $(gzip -9c dist/pkg/shotstage_app_bg.wasm | wc -c | tr -d ' ') bytes"
echo "serve dist/ with any static server, e.g. python3 -m http.server -d dist 8080"
