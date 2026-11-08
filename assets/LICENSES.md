# Bundled assets and their licenses

Everything in `assets/` that shotstage did not make itself, where it came from, and its license.

## Realistic people

The realistic (MakeHuman) people are not in this folder: see `crates/core/assets/README.md`.

## Stylized people: `people/stylized_*.glb`

Converted by `tools/people/build_quaternius.py` from **Universal Base Characters (Standard)**
by Quaternius (<https://quaternius.com/packs/universalbasecharacters.html>, downloaded from
<https://quaternius.itch.io/universal-base-characters>). License: **CC0 1.0 Universal**, as
stated on both pages and in the pack's `License_Standard.txt`. Used: Superhero_Male_FullBody,
Superhero_Female_FullBody, Hair_SimpleParted, Hair_Buzzed, Hair_Beard, Hair_Long, Hair_Buns.

## Palette thumbnails: `people/thumbs/*.png`

Rendered from the GLBs above with the shotstage CLI (`tools/people/thumbs.sh`). Same licenses.

## Chinese font: `fonts/NotoSansSC-subset.otf`

A subset of **Noto Sans SC** Regular (<https://github.com/notofonts/noto-cjk>, by Google and
Adobe), licensed under the **SIL Open Font License 1.1** (`fonts/NotoSansSC-LICENSE.txt`). Made
by `tools/fonts/subset.sh`. The subset keeps the font's name, as the OFL allows for Noto,
which has no Reserved Font Name.
