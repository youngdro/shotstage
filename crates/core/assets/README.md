# Built-in assets

## human.bin

Realistic people: the MakeHuman base mesh (hm08), its macro targets (gender, age, weight,
muscle, height, proportions), the facial expression units, and the "game engine" skeleton with
its skin weights, all taken from [MPFB](https://github.com/makehumancommunity/mpfb2)
(commit `6ff1eb3`) and baked by `scripts/mpfb/bake_human.py`.

These assets are released by the MakeHuman team under **CC0 1.0** (public domain dedication; see
MPFB's `LICENSE.ASSETS.md`). No MPFB program code (GPL) is included in shotstage: the bake script
only reads the data files.

The desktop app and the CLI embed this file. The browser build leaves it out (it is a third of
the download) and receives it from the page (`provideHumanData`), so the editor opens sooner;
`build.rs` fingerprints it so the page can't hand over a different version.

To rebuild:

    git clone --depth 1 https://github.com/makehumancommunity/mpfb2
    python3 scripts/mpfb/bake_human.py mpfb2 crates/core/assets/human.bin
