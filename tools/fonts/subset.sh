#!/usr/bin/env bash
# Rebuild assets/fonts/NotoSansSC-subset.otf: Noto Sans SC (SIL OFL 1.1, from
# github.com/notofonts/noto-cjk) cut down to the characters the interface uses plus the 3755
# most common Chinese characters (GB2312 level 1), so typed names usually render too.
# Usage: tools/fonts/subset.sh <path to NotoSansSC-Regular.otf>   (needs fonttools)
set -euo pipefail
cd "$(dirname "$0")/../.."
src="$1"
chars="$(mktemp)"
python3 - "$chars" <<'PY'
import glob, sys
chars = set()
for f in glob.glob("crates/app/src/*.rs"):
    chars.update(c for c in open(f, encoding="utf-8").read() if ord(c) > 127)
for hi in range(0xB0, 0xD8):
    for lo in range(0xA1, 0xFF):
        try:
            chars.add(bytes([hi, lo]).decode("gb2312"))
        except UnicodeDecodeError:
            pass
chars.update("，。、；：？！“”‘’（）《》【】…—·～")
open(sys.argv[1], "w", encoding="utf-8").write("".join(sorted(chars)))
PY
pyftsubset "$src" --text-file="$chars" --output-file=assets/fonts/NotoSansSC-subset.otf \
  --layout-features='' --no-hinting --name-IDs='*'
rm "$chars"
ls -l assets/fonts/NotoSansSC-subset.otf
