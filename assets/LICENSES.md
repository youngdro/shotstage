# Bundled assets and their licenses

Everything in `assets/` that shotstage did not make itself, where it came from, and its license.

## Realistic people: `people/realistic_*.glb`

Generated in Blender with **MPFB 2.0.18**, MakeHuman's Blender add-on
(<https://extensions.blender.org/add-ons/mpfb/>, by the MakeHuman Community,
<https://static.makehumancommunity.org/mpfb/index.html>), by `tools/people/build_mpfb.py`.

- The MPFB add-on itself is GPL-3.0-or-later. It is a tool and is not bundled; the add-on's
  output is not covered by its code license.
- The MakeHuman base mesh, targets (body shape) and rigs ("game engine" rig and weights) that
  the characters are made from are core assets, **CC0 1.0**: "All core assets are shared under
  Creative Commons, CC0" (<https://static.makehumancommunity.org/about/license.html>; MPFB's
  source code is GPL, MakeHuman's AGPL, neither is part of the output).
- Hair, clothes, shoes, eyebrows and eyes come from the **makehuman_system_assets** pack
  (<https://static.makehumancommunity.org/assets/assetpacks/makehuman_system_assets.html>,
  downloaded from files.makehumancommunity.org). Every asset in the pack is listed as **CC0**
  on that page and in the pack's `packs/makehuman_system_assets.json`. Their files also say:
  "This asset was explicitly released as CC0 in september 2020". Assets used:
  ponytail01, short02, bob02, short04 (hair); female_casualsuit01, male_casualsuit02,
  female_elegantsuit01, male_worksuit01, shoes01, shoes02, shoes03, shoes06 (clothes);
  eyebrow001, eyebrow002, eyebrow004, eyebrow010; low-poly eyes.

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
