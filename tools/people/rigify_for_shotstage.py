"""Turn a rigged human in the open Blender scene into a shotstage person GLB.

shotstage poses people through joints named pelvis, spine, neck, head, shoulder_*, elbow_*,
wrist_*, hip_*, knee_*, ankle_* whose rest rotation is the identity, in a rest pose with the
arms hanging straight down, facing +Z (glTF). This module:

1. renames the bones of an Unreal-mannequin style skeleton (MPFB "game engine" rig,
   Quaternius humanoid) to those joint names; the other bones (clavicles, extra spine bones,
   fingers, toes) keep their names and stay posable as extra joints;
2. bends the arms down into the rest pose and applies it to the meshes;
3. rebuilds every bone pointing straight up with no roll, so each joint's rest rotation is the
   identity after export;
4. gives every mesh one flat colour (shotstage shades in clay, textures are dropped), trims
   the polycount for the web and writes a GLB with the skin and no animation.

Run inside Blender; see build_mpfb.py and build_quaternius.py.
"""

import bmesh
import bpy
from mathutils import Matrix, Vector

UE_TO_SHOTSTAGE = {
    "pelvis": "pelvis",
    "spine_01": "spine",
    "neck_01": "neck",
    "head": "head",
    "Head": "head",
    "upperarm_l": "shoulder_l",
    "lowerarm_l": "elbow_l",
    "hand_l": "wrist_l",
    "upperarm_r": "shoulder_r",
    "lowerarm_r": "elbow_r",
    "hand_r": "wrist_r",
    "thigh_l": "hip_l",
    "calf_l": "knee_l",
    "foot_l": "ankle_l",
    "thigh_r": "hip_r",
    "calf_r": "knee_r",
    "foot_r": "ankle_r",
}

REQUIRED = ["pelvis", "spine", "neck", "head", "shoulder_l", "elbow_l", "wrist_l", "shoulder_r", "elbow_r",
            "wrist_r", "hip_l", "knee_l", "ankle_l", "hip_r", "knee_r", "ankle_r"]


def armature():
    arms = [o for o in bpy.data.objects if o.type == "ARMATURE"]
    assert len(arms) == 1, f"expected one armature, found {[a.name for a in arms]}"
    return arms[0]


def skinned_meshes(arm):
    return [o for o in bpy.data.objects
            if o.type == "MESH" and any(m.type == "ARMATURE" and m.object == arm for m in o.modifiers)]


def rename_bones(arm, mapping=UE_TO_SHOTSTAGE):
    for b in arm.data.bones:
        if b.name in mapping:
            b.name = mapping[b.name]  # Blender renames the meshes' vertex groups along
    missing = [j for j in REQUIRED if j not in arm.data.bones]
    assert not missing, f"bones missing after renaming: {missing}"


def _point_down(arm, bone_name, want=Vector((0, 0, -1))):
    """Rotate a pose bone about its head so its rest direction becomes `want` (world)."""
    pb = arm.pose.bones[bone_name]
    bpy.context.view_layer.update()
    m = arm.matrix_world @ pb.matrix
    head = m.translation.copy()
    direction = (m.to_3x3() @ Vector((0, 1, 0))).normalized()
    rot = direction.rotation_difference(want).to_matrix().to_4x4()
    new_world = Matrix.Translation(head) @ rot @ Matrix.Translation(-head) @ m
    pb.matrix = arm.matrix_world.inverted() @ new_world
    bpy.context.view_layer.update()


def arms_down(arm):
    """Bring the arms (and legs) into shotstage's rest pose: hanging straight down."""
    bpy.context.view_layer.objects.active = arm
    bpy.ops.object.mode_set(mode="POSE")
    for side in "lr":
        for b in [f"shoulder_{side}", f"elbow_{side}", f"wrist_{side}"]:
            _point_down(arm, b)
        # palms towards the thighs: turn the hand so the fingers stay below the wrist
        for b in [f"hip_{side}", f"knee_{side}"]:
            _point_down(arm, b)
    bpy.ops.object.mode_set(mode="OBJECT")


def apply_pose_as_rest(arm):
    for o in skinned_meshes(arm):
        bpy.context.view_layer.objects.active = o
        if o.data.shape_keys:
            o.shape_key_clear()
        mod = next(m for m in o.modifiers if m.type == "ARMATURE" and m.object == arm)
        name = mod.name
        bpy.ops.object.modifier_apply(modifier=name)
        new = o.modifiers.new(name, "ARMATURE")
        new.object = arm
    bpy.context.view_layer.objects.active = arm
    bpy.ops.object.mode_set(mode="POSE")
    bpy.ops.pose.select_all(action="SELECT")
    bpy.ops.pose.armature_apply(selected=False)
    bpy.ops.object.mode_set(mode="OBJECT")


def bones_up(arm):
    """Every bone points along +Z (Blender up) with no roll: identity rest rotations in glTF."""
    bpy.context.view_layer.objects.active = arm
    bpy.ops.object.mode_set(mode="EDIT")
    for eb in arm.data.edit_bones:
        eb.use_connect = False
    for eb in arm.data.edit_bones:
        length = max(eb.length, 0.02)
        eb.tail = eb.head + Vector((0, 0, length))
        eb.roll = 0.0
    bpy.ops.object.mode_set(mode="OBJECT")


def flat_colours(colours, default=(0.8, 0.8, 0.8)):
    """One flat base colour per mesh, by the first key found in its object or material name."""
    for o in bpy.data.objects:
        if o.type != "MESH":
            continue
        # material names first: a split body part is named after its region
        names = (" ".join(m.name for m in o.data.materials if m) + " " + o.name).lower()
        c = next((v for k, v in colours.items() if k in names), default)
        mat = bpy.data.materials.new(f"{o.name}_flat")
        mat.use_nodes = True
        bsdf = mat.node_tree.nodes.get("Principled BSDF")
        bsdf.inputs["Base Color"].default_value = (*c, 1.0)
        bsdf.inputs["Roughness"].default_value = 0.7
        o.data.materials.clear()
        o.data.materials.append(mat)


# body region of each bone, for dressing a bare base body in flat clay colours
REGIONS = [
    ("shoes", ["ankle_", "ball_"]),
    ("pants", ["pelvis", "hip_", "knee_"]),
    ("shirt", ["spine", "clavicle_", "shoulder_"]),
    ("skin", []),  # head, neck, forearms, hands, fingers
]


def dress(body_name, sleeves=True):
    """Split a bare body mesh into shirt / pants / shoes / skin parts by the bone each face
    follows most, so it reads as dressed when shaded in flat colours."""
    o = bpy.data.objects[body_name]
    groups = {g.index: g.name for g in o.vertex_groups}
    mats = []
    for region, _ in REGIONS:
        m = bpy.data.materials.new(region)
        o.data.materials.append(m)
        mats.append(len(o.data.materials) - 1)

    def region_of(bone):
        if not sleeves and bone.startswith("shoulder_"):
            return len(REGIONS) - 1
        for i, (_, prefixes) in enumerate(REGIONS[:-1]):
            if any(bone.startswith(p) for p in prefixes):
                return i
        return len(REGIONS) - 1

    me = o.data
    for poly in me.polygons:
        weight = {}
        for vi in poly.vertices:
            for g in me.vertices[vi].groups:
                weight[g.group] = weight.get(g.group, 0.0) + g.weight
        if not weight:
            continue
        bone = groups.get(max(weight, key=weight.get), "")
        poly.material_index = mats[region_of(bone)]
    bpy.ops.object.select_all(action="DESELECT")
    o.select_set(True)
    bpy.context.view_layer.objects.active = o
    bpy.ops.object.mode_set(mode="EDIT")
    bpy.ops.mesh.select_all(action="SELECT")
    bpy.ops.mesh.separate(type="MATERIAL")
    bpy.ops.object.mode_set(mode="OBJECT")


def split_top_bottom(name):
    """Split an outfit into a top and a bottom (by the bone each face follows most), so the two
    can have their own colours."""
    o = bpy.data.objects[name]
    groups = {g.index: g.name for g in o.vertex_groups}
    o.data.materials.clear()
    for m in ("top", "bottom"):
        o.data.materials.append(bpy.data.materials.new(m))
    me = o.data
    lower = ("pelvis", "hip_", "knee_", "ankle_", "ball_")
    for poly in me.polygons:
        weight = {}
        for vi in poly.vertices:
            for g in me.vertices[vi].groups:
                weight[g.group] = weight.get(g.group, 0.0) + g.weight
        bone = groups.get(max(weight, key=weight.get), "") if weight else ""
        poly.material_index = 1 if bone.startswith(lower) else 0
    bpy.ops.object.select_all(action="DESELECT")
    o.select_set(True)
    bpy.context.view_layer.objects.active = o
    bpy.ops.object.mode_set(mode="EDIT")
    bpy.ops.mesh.select_all(action="SELECT")
    bpy.ops.mesh.separate(type="MATERIAL")
    bpy.ops.object.mode_set(mode="OBJECT")


def triangles(o):
    return sum(len(p.vertices) - 2 for p in o.data.polygons)


def trim(max_tris_per_mesh):
    for o in bpy.data.objects:
        if o.type == "MESH" and triangles(o) > max_tris_per_mesh:
            bpy.context.view_layer.objects.active = o
            d = o.modifiers.new("trim", "DECIMATE")
            d.ratio = max_tris_per_mesh / triangles(o)
            # the decimate must come before the armature, so it acts on the rest shape
            while o.modifiers.find("trim") > 0:
                bpy.ops.object.modifier_move_up(modifier="trim")
            bpy.ops.object.modifier_apply(modifier="trim")


def drop_unskinned(arm, keep=()):
    for o in list(bpy.data.objects):
        if o.type == "MESH" and o not in skinned_meshes(arm) and o.name not in keep:
            bpy.data.objects.remove(o, do_unlink=True)


def export(path):
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.export_scene.gltf(
        filepath=path,
        export_format="GLB",
        use_selection=True,
        export_animations=False,
        export_skins=True,
        export_morph=False,
        export_texcoords=False,
        export_normals=True,
        export_image_format="NONE",
        export_yup=True,
        export_apply=False,
    )


def report():
    arm = armature()
    meshes = skinned_meshes(arm)
    print("REPORT bones", len(arm.data.bones), "meshes", [(o.name, triangles(o)) for o in meshes],
          "tris", sum(triangles(o) for o in meshes))
    lo = min((arm.matrix_world @ o.matrix_world @ v.co).z for o in meshes for v in o.data.vertices[:1])
    hi = max(max((o.matrix_world @ v.co).z for v in o.data.vertices) for o in meshes)
    print("REPORT height", round(hi, 3))


def finish(out_path, colours, max_tris_per_mesh=12000, mapping=UE_TO_SHOTSTAGE, dress_body=None, sleeves=True,
           outfit=None):
    arm = armature()
    rename_bones(arm, mapping)
    drop_unskinned(arm)
    if dress_body:
        dress(dress_body, sleeves)
    if outfit:
        split_top_bottom(outfit)
    arms_down(arm)
    apply_pose_as_rest(arm)
    bones_up(arm)
    flat_colours(colours)
    trim(max_tris_per_mesh)
    report()
    export(out_path)
