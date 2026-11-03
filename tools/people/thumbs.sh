#!/usr/bin/env bash
# Render the palette thumbnails of the detailed people (assets/people/thumbs/*.png, 128 × 96,
# framed like the other palette thumbnails) with the shotstage CLI.
# Usage: tools/people/thumbs.sh   (after cargo build -p shotstage-cli --release)
set -euo pipefail
cd "$(dirname "$0")/../.."
cli=./target/release/shotstage
tmp="$(mktemp -d)"
mkdir -p assets/people/thumbs
for glb in assets/people/*.glb; do
  stem="$(basename "$glb" .glb)"
  python3 - "$glb" "$tmp/steps.json" <<'PY'
import base64, json, sys
data = base64.b64encode(open(sys.argv[1], "rb").read()).decode()
json.dump([
    {"command": "model.import", "params": {"format": "glb", "data": data, "name": "thumb", "id": "thumb"}},
    {"command": "entity.create", "params": {"type": "camera", "id": "thumb_cam", "camera": {"aspect": 128 / 96, "focalLengthMm": 50}}},
    {"command": "camera.frame", "params": {"camera": "thumb_cam", "subject": "thumb", "size": "full", "angle": "high", "side": "frontRight", "makeActive": True}},
], open(sys.argv[2], "w"))
PY
  rm -f "$tmp/scene.json"
  "$cli" batch -s "$tmp/scene.json" "$tmp/steps.json" > /dev/null
  "$cli" snapshot -s "$tmp/scene.json" -o "assets/people/thumbs/$stem.png" --width 128 --height 96 > /dev/null
done
rm -rf "$tmp"
ls -l assets/people/thumbs
