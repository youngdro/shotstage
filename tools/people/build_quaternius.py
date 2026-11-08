"""Build the stylized people from Quaternius' Universal Base Characters (Standard, CC0).

blender -b --python tools/people/build_quaternius.py -- <pack folder> <out folder>

The pack is downloaded from https://quaternius.com/packs/universalbasecharacters.html
(itch.io: quaternius.itch.io/universal-base-characters), "Standard" zip, CC0 1.0.
"""

import os
import sys

import bpy

sys.path.insert(0, os.path.dirname(__file__))
import rigify_for_shotstage as rig  # noqa: E402

PACK, OUT = sys.argv[sys.argv.index("--") + 1:][:2]
BODIES = os.path.join(PACK, "Base Characters", "Godot - UE")
HAIR = os.path.join(PACK, "Hairstyles", "Rigged to Head Bone", "glTF (Godot -Unreal)")

SKIN = (0.80, 0.60, 0.48)
COLOURS = {
    "shirt": (0.33, 0.42, 0.56),
    "pants": (0.22, 0.24, 0.30),
    "shoes": (0.15, 0.15, 0.17),
    "skin": SKIN,
    "eyebrow": (0.12, 0.08, 0.05),
    "hair": (0.16, 0.10, 0.06),
    "beard": (0.16, 0.10, 0.06),
    "eye": (0.92, 0.92, 0.9),
    "superhero": SKIN,
}

PEOPLE = [
    # (file stem, body, body mesh, pieces, shirt colour, sleeves)
    ("stylized_man", "Superhero_Male_FullBody", "SuperHero_Male", ["Hair_SimpleParted"], (0.33, 0.42, 0.56), True),
    ("stylized_man_beard", "Superhero_Male_FullBody", "SuperHero_Male", ["Hair_Buzzed", "Hair_Beard"],
     (0.55, 0.30, 0.22), False),
    ("stylized_woman", "Superhero_Female_FullBody", "Superhero_Female", ["Hair_Long"], (0.72, 0.38, 0.40), True),
    ("stylized_woman_buns", "Superhero_Female_FullBody", "Superhero_Female", ["Hair_Buns"], (0.30, 0.52, 0.45),
     False),
]


def attach(piece, arm):
    """Import a piece rigged to the head bone and bind it to the body's armature instead."""
    before = set(bpy.data.objects)
    bpy.ops.import_scene.gltf(filepath=os.path.join(HAIR, piece + ".gltf"))
    new = [o for o in bpy.data.objects if o not in before]
    for o in new:
        if o.type == "MESH" and o.vertex_groups:
            world = o.matrix_world.copy()
            o.parent = arm
            o.matrix_world = world
            for m in o.modifiers:
                if m.type == "ARMATURE":
                    m.object = arm
            o.name = piece
    for o in new:
        if o.type != "MESH" or not o.vertex_groups:
            bpy.data.objects.remove(o, do_unlink=True)


for stem, body, body_mesh, pieces, shirt, sleeves in PEOPLE:
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=os.path.join(BODIES, body + ".gltf"))
    arm = rig.armature()
    for piece in pieces:
        attach(piece, arm)
    colours = dict(COLOURS, shirt=shirt)
    rig.finish(os.path.join(OUT, stem + ".glb"), colours, max_tris_per_mesh=14000, dress_body=body_mesh, sleeves=sleeves)
    print("BUILT", stem)
