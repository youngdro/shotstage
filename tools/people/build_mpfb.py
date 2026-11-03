"""Build the realistic people with MakeHuman's MPFB add-on and its CC0 system assets.

blender -b --python tools/people/build_mpfb.py -- <out folder>

Needs MPFB (extensions.blender.org/add-ons/mpfb) and the "makehuman_system_assets" pack
(static.makehumancommunity.org/assets/assetpacks/makehuman_system_assets.html, CC0) unpacked
into MPFB's user data folder. MakeHuman models are CC0; so is every asset used here.
"""

import importlib
import os
import sys

import bpy

sys.path.insert(0, os.path.dirname(__file__))
import rigify_for_shotstage as rig  # noqa: E402

OUT = sys.argv[sys.argv.index("--") + 1]
SERVICES = "bl_ext.user_default.mpfb.services."
HumanService = importlib.import_module(SERVICES + "humanservice").HumanService
LocationService = importlib.import_module(SERVICES + "locationservice").LocationService
DATA = LocationService.get_user_data()

PEOPLE = [
    {
        "file": "realistic_woman",
        "macro": {"gender": 0.0, "age": 0.5, "muscle": 0.5, "weight": 0.45, "height": 0.45},
        "assets": [("hair", "ponytail01"), ("clothes", "female_casualsuit01"), ("clothes", "shoes02"),
                   ("eyebrows", "eyebrow010"), ("eyes", "low-poly")],
        "outfit": "clothes_female_casualsuit01",
        "colours": {"top": (0.80, 0.45, 0.40), "bottom": (0.20, 0.22, 0.30), "shoes": (0.90, 0.90, 0.88), "hair": (0.22, 0.13, 0.07)},
    },
    {
        "file": "realistic_man",
        "macro": {"gender": 1.0, "age": 0.5, "muscle": 0.6, "weight": 0.5, "height": 0.55},
        "assets": [("hair", "short02"), ("clothes", "male_casualsuit02"), ("clothes", "shoes01"),
                   ("eyebrows", "eyebrow001"), ("eyes", "low-poly")],
        "outfit": "clothes_male_casualsuit02",
        "colours": {"top": (0.30, 0.40, 0.55), "bottom": (0.62, 0.55, 0.42), "shoes": (0.25, 0.18, 0.12), "hair": (0.12, 0.08, 0.05)},
    },
    {
        "file": "realistic_woman_older",
        "macro": {"gender": 0.0, "age": 0.8, "muscle": 0.4, "weight": 0.6, "height": 0.4},
        "assets": [("hair", "bob02"), ("clothes", "female_elegantsuit01"), ("clothes", "shoes03"),
                   ("eyebrows", "eyebrow004"), ("eyes", "low-poly")],
        "outfit": "clothes_female_elegantsuit01",
        "colours": {"top": (0.25, 0.27, 0.35), "bottom": (0.25, 0.27, 0.35), "shoes": (0.12, 0.12, 0.13), "hair": (0.86, 0.86, 0.88)},
    },
    {
        "file": "realistic_man_older",
        "macro": {"gender": 1.0, "age": 0.85, "muscle": 0.4, "weight": 0.75, "height": 0.5},
        "assets": [("hair", "short04"), ("clothes", "male_worksuit01"), ("clothes", "shoes06"),
                   ("eyebrows", "eyebrow002"), ("eyes", "low-poly")],
        "outfit": "clothes_male_worksuit01",
        "colours": {"top": (0.55, 0.62, 0.70), "bottom": (0.25, 0.28, 0.33), "shoes": (0.20, 0.15, 0.10), "hair": (0.80, 0.80, 0.82)},
    },
]

SKIN = (0.82, 0.62, 0.50)
BASE_COLOURS = {"eyebrow": (0.15, 0.10, 0.07), "eye": (0.92, 0.92, 0.9), "human": SKIN, "body": SKIN}


def clear():
    for o in list(bpy.data.objects):
        bpy.data.objects.remove(o, do_unlink=True)
    for coll in (bpy.data.meshes, bpy.data.armatures, bpy.data.materials, bpy.data.images):
        for item in list(coll):
            if item.users == 0:
                coll.remove(item)


def bake_mesh_state():
    """Bake shape keys (the body shape) and apply every modifier except the armature, so the
    meshes are plain skinned geometry (helpers and the body under the clothes removed)."""
    for o in [o for o in bpy.data.objects if o.type == "MESH"]:
        bpy.context.view_layer.objects.active = o
        bpy.ops.object.select_all(action="DESELECT")
        o.select_set(True)
        if o.data.shape_keys:
            bpy.ops.object.shape_key_remove(all=True, apply_mix=True)
        for m in list(o.modifiers):
            if m.type != "ARMATURE":
                bpy.ops.object.modifier_apply(modifier=m.name)


for person in PEOPLE:
    clear()
    macro = dict(person["macro"], proportions=0.5, cupsize=0.5, firmness=0.5,
                 race={"asian": 0.34, "caucasian": 0.33, "african": 0.33})
    basemesh = HumanService.create_human(macro_detail_dict=macro)
    basemesh.name = "human"
    HumanService.add_builtin_rig(basemesh, "game_engine")
    for kind, name in person["assets"]:
        path = os.path.join(DATA, kind, name, name + ".mhclo")
        asset_type = {"hair": "Hair", "clothes": "Clothes", "eyebrows": "Eyebrows", "eyes": "Eyes"}[kind]
        obj = HumanService.add_mhclo_asset(path, basemesh, asset_type=asset_type, subdiv_levels=0)
        if obj is not None:
            # "hair_short02", "eyes_low-poly": the colour table matches on the kind
            obj.name = f"{kind}_{name}"
    bake_mesh_state()
    colours = dict(person["colours"], **BASE_COLOURS)
    rig.finish(os.path.join(OUT, person["file"] + ".glb"), colours, max_tris_per_mesh=9000, outfit=person["outfit"])
    print("BUILT", person["file"])
