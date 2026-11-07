#!/usr/bin/env python3
"""Bake the MakeHuman / MPFB human into shotstage's compact binary asset.

Input: a checkout of https://github.com/makehumancommunity/mpfb2 (only its CC0 asset data is
read: the base mesh, the macro and expression targets, the game-engine rig and its weights).
Output: crates/core/assets/human.bin, read by `shotstage_core::human`.

    python3 scripts/mpfb/bake_human.py /path/to/mpfb2 crates/core/assets/human.bin

This script is shotstage's own code (no MPFB program code is used); it only parses the data files.

Format (little endian):
    b"SSHM" u32 version
    u32 verts, u32 quads, u32 bones, u32 morphs
    quads × 4 u16          vertex loops (counter-clockwise from outside)
    quads × u8             part   (0 body, 1 tights, 2 hair cap, 3 eyes, 4 teeth)
    quads × u8             region (0 head/neck, 1 torso, 2 hips, 3 arm, 4 hand, 5 leg, 6 foot)
    quads × u8             along  (arm: 0 shoulder → 100 elbow → 200 wrist; leg: hip → knee → ankle;
                                   head / torso: 100 + cm below the neckline)
    2 × verts × 3 i16      rest positions (female, male), 1/16000 m, Y up, +Z forward, +X the figure's left
    bones × { u8 len, name, i16 parent, u16 n, n × (u16 vertex, f32 weight) }   joint = Σ w·v
    verts × (4 u8 bone, 4 u8 weight)                                             weights sum to 255
    morphs × { u8 len, name, f32 unit, u8 dense, u32 n,
               dense: verts × 3 i16 deltas (× unit metres); sparse: n × (u16 vertex, 3 i16) }
"""

import gzip
import json
import math
import os
import struct
import sys
from collections import defaultdict

SCALE = 0.1  # the base mesh is in decimetres
KEEP = {
    "body": 0,
    "helper-tights": 1,
    "helper-hair": 2,
    "helper-l-eye": 3,
    "helper-r-eye": 3,
    "helper-upper-teeth": 4,
    "helper-lower-teeth": 4,
}
RACES = ["african", "asian", "caucasian"]
POS_SCALE = 16000.0  # rest positions are i16 in 1/16000 m


def load_obj(path):
    verts, faces = [], []
    group = None
    with open(path) as f:
        for line in f:
            p = line.split()
            if not p:
                continue
            if p[0] == "v":
                verts.append([float(x) for x in p[1:4]])
            elif p[0] == "g":
                group = p[1]
            elif p[0] == "f":
                faces.append((group, [int(x.split("/")[0]) - 1 for x in p[1:]]))
    return verts, faces


def load_target(path):
    out = {}
    with gzip.open(path, "rt") as f:
        for line in f:
            p = line.split()
            if len(p) == 4 and not line.startswith("#"):
                out[int(p[0])] = [float(x) for x in p[1:]]
    return out


class Targets:
    def __init__(self, root):
        self.root = root
        self.cache = {}

    def get(self, rel):
        if rel not in self.cache:
            path = os.path.join(self.root, rel + ".target.gz")
            self.cache[rel] = load_target(path) if os.path.exists(path) else None
        return self.cache[rel]


def two_way(v, low, mid, high):
    """Weights of a macro value split at 0.5 between three labels (`""` = no target)."""
    if v < 0.5:
        return {low: (0.5 - v) * 2, mid: 1 - (0.5 - v) * 2}
    return {mid: 1 - (v - 0.5) * 2, high: (v - 0.5) * 2}


def age_weights(a):
    if a < 0.1875:
        t = a / 0.1875
        return {"baby": 1 - t, "child": t}
    if a < 0.5:
        t = (a - 0.1875) / (0.5 - 0.1875)
        return {"child": 1 - t, "young": t}
    t = (a - 0.5) / 0.5
    return {"young": 1 - t, "old": t}


def macro_mesh(base, tg, gender, age=0.5, muscle=0.5, weight=0.5, height=0.5, proportions=0.5):
    """Base mesh with the MakeHuman macro targets for these slider values applied."""
    out = [v[:] for v in base]
    G = {"female": 1 - gender, "male": gender}
    A = age_weights(age)
    M = two_way(muscle, "minmuscle", "averagemuscle", "maxmuscle")
    W = two_way(weight, "minweight", "averageweight", "maxweight")
    H = two_way(height, "minheight", "", "maxheight")
    P = two_way(proportions, "uncommonproportions", "", "idealproportions")
    C = two_way(0.5, "mincup", "averagecup", "maxcup")
    F = two_way(0.5, "minfirmness", "averagefirmness", "maxfirmness")

    def add(rel, w):
        if w < 1e-6:
            return
        t = tg.get(rel)
        if t is None:
            return
        for i, d in t.items():
            o = out[i]
            o[0] += d[0] * w
            o[1] += d[1] * w
            o[2] += d[2] * w

    for g, gw in G.items():
        for a, aw in A.items():
            for r in RACES:
                add(f"macrodetails/{r}-{g}-{a}", gw * aw / 3)
            for m, mw in M.items():
                for wt, ww in W.items():
                    base_w = gw * aw * mw * ww
                    add(f"macrodetails/universal-{g}-{a}-{m}-{wt}", base_w)
                    for h, hw in H.items():
                        if h:
                            add(f"macrodetails/height/{g}-{a}-{m}-{wt}-{h}", base_w * hw)
                    for p, pw in P.items():
                        if p:
                            add(f"macrodetails/proportions/{g}-{a}-{m}-{wt}-{p}", base_w * pw)
                    for c, cw in C.items():
                        for fm, fw in F.items():
                            add(f"breast/{g}-{a}-{m}-{wt}-{c}-{fm}", base_w * cw * fw)
    return out


def joint_heads(rig, groups_by_name, verts_idx):
    """Each bone head as a weighted average of base mesh vertices."""
    out = {}
    for name, b in rig.items():
        h = b["head"]
        s = h["strategy"]
        if s == "CUBE":
            vs = sorted(groups_by_name[h["cube_name"]])
        elif s == "VERTEX":
            vs = [h["vertex_index"]]
        elif s == "MEAN":
            vs = list(h["vertex_indices"])
        else:
            raise SystemExit(f"unknown joint strategy {s} for {name}")
        out[name] = [(v, 1.0 / len(vs)) for v in vs]
    return out


def main():
    mpfb, out_path = sys.argv[1], sys.argv[2]
    data = os.path.join(mpfb, "src", "mpfb", "data")
    verts, faces = load_obj(os.path.join(data, "3dobjs", "base.obj"))
    tg = Targets(os.path.join(data, "targets"))
    rig = json.load(open(os.path.join(data, "rigs", "standard", "rig.game_engine.json")))
    weights = json.load(open(os.path.join(data, "rigs", "standard", "weights.game_engine.json")))["weights"]

    groups = defaultdict(set)
    for g, f in faces:
        groups[g].update(f)

    # bones in parent-first order, without the empty Root
    rig.pop("Root")
    order = []
    while len(order) < len(rig):
        for n, b in sorted(rig.items()):
            if n not in order and (b["parent"] == "Root" or b["parent"] in order):
                order.append(n)
    bone_index = {n: i for i, n in enumerate(order)}
    heads = joint_heads(rig, groups, None)

    # vertices we keep
    kept_faces = [(KEEP[g], f) for g, f in faces if g in KEEP]
    used = sorted({i for _, f in kept_faces for i in f} | {v for h in heads.values() for v, _ in h})
    remap = {old: new for new, old in enumerate(used)}
    assert len(used) < 65536

    # rest shapes (metres) for both sexes and the morph deltas
    def shaped(**kw):
        m = macro_mesh(verts, tg, **kw)
        return [[m[i][0] * SCALE, m[i][1] * SCALE, m[i][2] * SCALE] for i in used]

    bases = {"female": shaped(gender=0.0), "male": shaped(gender=1.0)}
    variants = {
        "weight+": dict(weight=1.0),
        "weight-": dict(weight=0.0),
        "muscle+": dict(muscle=1.0),
        "muscle-": dict(muscle=0.0),
        "height+": dict(height=1.0),
        "height-": dict(height=0.0),
        "old": dict(age=1.0),
        "child": dict(age=0.1875),
    }
    morphs = []
    for sex, g in (("female", 0.0), ("male", 1.0)):
        for vname, kw in variants.items():
            m = shaped(gender=g, **kw)
            d = {i: [m[i][k] - bases[sex][i][k] for k in range(3)] for i in range(len(used))}
            morphs.append((f"{sex}:{vname}", d))
    # expression units: the three ethnic variants averaged (the default mix)
    unit_dir = os.path.join(data, "targets", "expression", "units", "caucasian")
    for fname in sorted(os.listdir(unit_dir)):
        unit = fname.replace(".target.gz", "")
        acc = defaultdict(lambda: [0.0, 0.0, 0.0])
        for r in RACES:
            for i, dv in load_target(os.path.join(data, "targets", "expression", "units", r, fname)).items():
                if i in remap:
                    a = acc[remap[i]]
                    for k in range(3):
                        a[k] += dv[k] * SCALE / 3
        morphs.append((f"expr:{unit}", dict(acc)))

    # skin weights: four strongest bones per vertex
    per_vertex = defaultdict(list)
    for bone, lst in weights.items():
        if bone not in bone_index:
            continue
        for v, w in lst:
            if v in remap and w > 0:
                per_vertex[remap[v]].append((w, bone_index[bone]))
    skin = []
    for i in range(len(used)):
        top = sorted(per_vertex.get(i, []), reverse=True)[:4]
        if not top:
            top = [(1.0, bone_index["head"])]
        total = sum(w for w, _ in top)
        q = [round(w / total * 255) for w, _ in top]
        q[0] += 255 - sum(q)
        bones = [b for _, b in top] + [0] * (4 - len(top))
        q += [0] * (4 - len(q))
        skin.append((bones, q))

    # face regions, from the bones that drive each face (on the average female/male shape)
    avg = [[(bases["female"][i][k] + bases["male"][i][k]) / 2 for k in range(3)] for i in range(len(used))]

    def jpos(name):
        p = [0.0, 0.0, 0.0]
        for v, w in heads[name]:
            for k in range(3):
                p[k] += avg[remap[v]][k] * w
        return p

    def seg_t(p, a, b):
        ab = [b[k] - a[k] for k in range(3)]
        ap = [p[k] - a[k] for k in range(3)]
        l2 = sum(x * x for x in ab) or 1e-9
        t = max(0.0, min(1.0, sum(ap[k] * ab[k] for k in range(3)) / l2))
        c = [a[k] + ab[k] * t for k in range(3)]
        return t, math.dist(c, p)

    def along(p, chain):
        a, b, c = (jpos(n) for n in chain)
        t1, d1 = seg_t(p, a, b)
        t2, d2 = seg_t(p, b, c)
        return t1 if d1 <= d2 else 1.0 + t2

    waist_y = jpos("spine_01")[1] - 0.02
    region_of_bone = {}
    for n in order:
        if n in ("head", "neck_01"):
            r = 0
        elif n.startswith(("spine", "clavicle", "pelvis")):
            r = 1
        elif n.startswith(("upperarm", "lowerarm")):
            r = 3
        elif n.startswith(("hand", "thumb", "index", "middle", "ring", "pinky")):
            r = 4
        elif n.startswith(("thigh", "calf")):
            r = 5
        elif n.startswith(("foot", "ball")):
            r = 6
        else:
            raise SystemExit(f"no region for {n}")
        region_of_bone[bone_index[n]] = r

    quads, parts, regions, alongs = [], [], [], []
    for part, f in kept_faces:
        assert len(f) == 4
        loop = [remap[i] for i in f]
        score = defaultdict(float)
        for i in loop:
            for b, w in zip(*skin[i]):
                score[region_of_bone[b]] += w
        r = max(score.items(), key=lambda kv: kv[1])[0]
        c = [sum(avg[i][k] for i in loop) / 4 for k in range(3)]
        a = 0.0
        # a clean crew neckline, a little lower at the front
        if r in (0, 1):
            neck = jpos("neck_01")
            front = max(0.0, min(1.0, (c[2] - neck[2]) / 0.08))
            cut = neck[1] - 0.025 - 0.02 * front
            r = 0 if c[1] > cut else 1
            a = 1.0 + max(-1.0, min(1.5, cut - c[1])) * 1.0  # 1 = on the neckline, +0.01 per cm below
        if r == 1 and c[1] < waist_y:
            r = 2
        if r == 2 and score.get(5, 0) > 0 and c[1] < jpos("thigh_l")[1] - 0.06:
            r = 5
        if r == 3:
            side = "l" if c[0] > 0 else "r"
            a = along(c, [f"upperarm_{side}", f"lowerarm_{side}", f"hand_{side}"])
        elif r == 5:
            side = "l" if c[0] > 0 else "r"
            a = along(c, [f"thigh_{side}", f"calf_{side}", f"foot_{side}"])
        quads.append(loop)
        parts.append(part)
        regions.append(r)
        alongs.append(max(0, min(255, round(a * 100))))

    # write
    out = bytearray()
    out += b"SSHM" + struct.pack("<I", 1)
    out += struct.pack("<4I", len(used), len(quads), len(order), len(morphs))
    for q in quads:
        out += struct.pack("<4H", *q)
    out += bytes(parts) + bytes(regions) + bytes(alongs)
    for sex in ("female", "male"):
        for p in bases[sex]:
            out += struct.pack("<3h", *(round(x * POS_SCALE) for x in p))
    for n in order:
        nb = n.encode()
        parent = rig[n]["parent"]
        out += struct.pack("<B", len(nb)) + nb + struct.pack("<h", -1 if parent == "Root" else bone_index[parent])
        out += struct.pack("<H", len(heads[n]))
        for v, w in heads[n]:
            out += struct.pack("<Hf", remap[v], w)
    for bones, q in skin:
        out += struct.pack("<4B4B", *bones, *q)
    for name, d in morphs:
        nb = name.encode()
        peak = max((abs(x) for dv in d.values() for x in dv), default=0.0)
        unit = max(peak / 32000, 1e-7)
        items = []
        for i, dv in sorted(d.items()):
            qv = [round(x / unit) for x in dv]
            if any(abs(x) * unit >= 2e-5 for x in qv):
                items.append((i, qv))
        dense = len(items) > len(used) * 0.6
        out += struct.pack("<B", len(nb)) + nb + struct.pack("<fBI", unit, dense, len(items) if not dense else len(used))
        if dense:
            full = dict(items)
            for i in range(len(used)):
                out += struct.pack("<3h", *full.get(i, [0, 0, 0]))
        else:
            for i, qv in items:
                out += struct.pack("<H3h", i, *qv)
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "wb") as f:
        f.write(out)
    print(f"{out_path}: {len(out)} bytes, {len(used)} verts, {len(quads)} quads, {len(order)} bones, {len(morphs)} morphs")
    for sex in ("female", "male"):
        ys = [p[1] for p in bases[sex]]
        print(sex, "height", round(max(ys) - min(ys), 3), "min y", round(min(ys), 3))


if __name__ == "__main__":
    main()
