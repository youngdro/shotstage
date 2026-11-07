//! Edit-mode tools for polygon meshes, in the spirit of Blender's edit mode: work on selected
//! vertices, edges and faces, then extrude, inset, bevel, loop cut, subdivide, merge, delete,
//! fill, connect, flip, and select loops, rings and linked parts.
//!
//! Every tool is a pure function `(&Mesh, &Elements, params) -> (Mesh, Elements)`: it returns
//! the new mesh and the elements a user would have selected afterwards (Blender keeps the
//! extruded faces selected, a loop cut selects the new loop...), so scripts can chain tools
//! without tracking index changes. Element indices refer to the mesh they were given with.

use std::collections::{BTreeSet, HashMap, HashSet};

use glam::{EulerRot, Quat, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;
use crate::{CoreError, Result};

/// Selected parts of a mesh. Edges are vertex pairs (either order); faces are face indices.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct Elements {
    pub vertices: Vec<u32>,
    pub edges: Vec<[u32; 2]>,
    pub faces: Vec<u32>,
}

impl Elements {
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty() && self.edges.is_empty() && self.faces.is_empty()
    }
    pub fn faces(faces: impl IntoIterator<Item = u32>) -> Self {
        Self { faces: faces.into_iter().collect(), ..Default::default() }
    }
    pub fn edges(edges: impl IntoIterator<Item = [u32; 2]>) -> Self {
        Self { edges: edges.into_iter().collect(), ..Default::default() }
    }
    pub fn vertices(vertices: impl IntoIterator<Item = u32>) -> Self {
        Self { vertices: vertices.into_iter().collect(), ..Default::default() }
    }
    /// Sort and remove duplicates (edges as `[min, max]`).
    pub fn normalized(mut self) -> Self {
        self.vertices.sort_unstable();
        self.vertices.dedup();
        for e in &mut self.edges {
            *e = [e[0].min(e[1]), e[0].max(e[1])];
        }
        self.edges.sort_unstable();
        self.edges.dedup();
        self.faces.sort_unstable();
        self.faces.dedup();
        self
    }
}

pub type Edge = (u32, u32);

/// Undirected edge key.
pub fn ek(a: u32, b: u32) -> Edge {
    (a.min(b), a.max(b))
}

/// Which faces use each edge and vertex.
pub struct Topology {
    pub edge_faces: HashMap<Edge, Vec<usize>>,
    pub vert_faces: Vec<Vec<usize>>,
    /// Edges in first-seen order (stable across runs).
    pub edges: Vec<Edge>,
}

impl Topology {
    pub fn new(m: &Mesh) -> Self {
        let mut edge_faces: HashMap<Edge, Vec<usize>> = HashMap::new();
        let mut edges = Vec::new();
        let mut vert_faces = vec![Vec::new(); m.positions.len()];
        for (fi, f) in m.faces.iter().enumerate() {
            for i in 0..f.len() {
                let k = ek(f[i], f[(i + 1) % f.len()]);
                let list = edge_faces.entry(k).or_default();
                if list.is_empty() {
                    edges.push(k);
                }
                if !list.contains(&fi) {
                    list.push(fi);
                }
                if let Some(v) = vert_faces.get_mut(f[i] as usize)
                    && !v.contains(&fi)
                {
                    v.push(fi);
                }
            }
        }
        Self { edge_faces, vert_faces, edges }
    }
    pub fn faces_of(&self, e: Edge) -> &[usize] {
        self.edge_faces.get(&e).map(Vec::as_slice).unwrap_or(&[])
    }
    /// Vertices joined to `v` by an edge.
    pub fn neighbours(&self, m: &Mesh, v: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for &fi in &self.vert_faces[v as usize] {
            let f = &m.faces[fi];
            for i in 0..f.len() {
                if f[i] == v {
                    for w in [f[(i + f.len() - 1) % f.len()], f[(i + 1) % f.len()]] {
                        if w != v && !out.contains(&w) {
                            out.push(w);
                        }
                    }
                }
            }
        }
        out
    }
}

fn pos(m: &Mesh, v: u32) -> Vec3 {
    Vec3::from_array(m.positions[v as usize])
}

/// Newell normal (length = 2 × area).
pub fn face_normal_raw(m: &Mesh, f: &[u32]) -> Vec3 {
    let mut n = Vec3::ZERO;
    for i in 0..f.len() {
        let a = pos(m, f[i]);
        let b = pos(m, f[(i + 1) % f.len()]);
        n += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    n
}

pub fn face_normal(m: &Mesh, f: &[u32]) -> Vec3 {
    face_normal_raw(m, f).normalize_or_zero()
}

pub fn face_center(m: &Mesh, f: &[u32]) -> Vec3 {
    f.iter().map(|&v| pos(m, v)).sum::<Vec3>() / f.len().max(1) as f32
}

/// The selection with Blender's "flush": faces select their edges and vertices; in vertex mode
/// (only vertices given) edges and faces whose corners are all selected count as selected; in
/// edge mode faces whose edges are all selected do.
#[derive(Clone, Debug, Default)]
pub struct Flushed {
    pub verts: BTreeSet<u32>,
    pub edges: BTreeSet<Edge>,
    pub faces: BTreeSet<u32>,
}

pub fn flush(m: &Mesh, sel: &Elements) -> Result<Flushed> {
    let topo = Topology::new(m);
    flush_with(m, &topo, sel)
}

/// [`flush`] with a topology already built for `m`.
pub fn flush_with(m: &Mesh, topo: &Topology, sel: &Elements) -> Result<Flushed> {
    let nv = m.positions.len() as u32;
    let nf = m.faces.len() as u32;
    let mut out = Flushed::default();
    for &v in &sel.vertices {
        if v >= nv {
            return Err(CoreError::Invalid(format!("vertex {v} does not exist (mesh has {nv})")));
        }
        out.verts.insert(v);
    }
    for e in &sel.edges {
        let k = ek(e[0], e[1]);
        if !topo.edge_faces.contains_key(&k) {
            return Err(CoreError::Invalid(format!("edge [{}, {}] does not exist", e[0], e[1])));
        }
        out.edges.insert(k);
        out.verts.insert(k.0);
        out.verts.insert(k.1);
    }
    for &f in &sel.faces {
        if f >= nf {
            return Err(CoreError::Invalid(format!("face {f} does not exist (mesh has {nf})")));
        }
        out.faces.insert(f);
        let face = &m.faces[f as usize];
        for i in 0..face.len() {
            out.verts.insert(face[i]);
            out.edges.insert(ek(face[i], face[(i + 1) % face.len()]));
        }
    }
    if sel.faces.is_empty() && sel.edges.is_empty() {
        // vertex mode
        for &e in &topo.edges {
            if out.verts.contains(&e.0) && out.verts.contains(&e.1) {
                out.edges.insert(e);
            }
        }
        for (fi, f) in m.faces.iter().enumerate() {
            if f.iter().all(|v| out.verts.contains(v)) {
                out.faces.insert(fi as u32);
            }
        }
    } else if sel.faces.is_empty() {
        // edge mode
        for (fi, f) in m.faces.iter().enumerate() {
            if (0..f.len()).all(|i| out.edges.contains(&ek(f[i], f[(i + 1) % f.len()]))) {
                out.faces.insert(fi as u32);
            }
        }
    }
    Ok(out)
}

/// Remove repeated corners and faces that collapsed, drop unused vertices, and carry the
/// selection over to the new indices.
fn finish(mut m: Mesh, sel: Elements) -> (Mesh, Elements) {
    let mut face_map = vec![None; m.faces.len()];
    let mut faces = Vec::with_capacity(m.faces.len());
    for (fi, f) in std::mem::take(&mut m.faces).into_iter().enumerate() {
        let mut g: Vec<u32> = Vec::with_capacity(f.len());
        for v in f {
            if g.last() != Some(&v) {
                g.push(v);
            }
        }
        while g.len() > 1 && g.first() == g.last() {
            g.pop();
        }
        let unique: HashSet<u32> = g.iter().copied().collect();
        if unique.len() >= 3 {
            face_map[fi] = Some(faces.len() as u32);
            faces.push(g);
        }
    }
    let mut used = vec![false; m.positions.len()];
    for f in &faces {
        for &v in f {
            used[v as usize] = true;
        }
    }
    let mut vmap = vec![None; m.positions.len()];
    let mut positions = Vec::new();
    for (i, p) in m.positions.iter().enumerate() {
        if used[i] {
            vmap[i] = Some(positions.len() as u32);
            positions.push(*p);
        }
    }
    for f in &mut faces {
        for v in f.iter_mut() {
            *v = vmap[*v as usize].expect("used");
        }
    }
    let map_v = |v: u32| vmap.get(v as usize).copied().flatten();
    let sel = Elements {
        vertices: sel.vertices.iter().filter_map(|&v| map_v(v)).collect(),
        edges: sel.edges.iter().filter_map(|e| Some([map_v(e[0])?, map_v(e[1])?])).filter(|e| e[0] != e[1]).collect(),
        faces: sel.faces.iter().filter_map(|&f| face_map.get(f as usize).copied().flatten()).collect(),
    };
    let out = Mesh { positions, faces };
    // only keep edges that still exist
    let topo = Topology::new(&out);
    let mut sel = sel.normalized();
    sel.edges.retain(|e| topo.edge_faces.contains_key(&(e[0], e[1])));
    (out, sel)
}

fn push_vertex(m: &mut Mesh, p: Vec3) -> u32 {
    m.positions.push(p.to_array());
    m.positions.len() as u32 - 1
}

fn check_finite(v: f32, what: &str) -> Result<f32> {
    if v.is_finite() && v.abs() <= 10_000.0 {
        Ok(v)
    } else {
        Err(CoreError::Invalid(format!("{what} must be finite and at most 10000 in size, got {v}")))
    }
}

fn need(cond: bool, msg: &str) -> Result<()> {
    if cond { Ok(()) } else { Err(CoreError::Invalid(msg.into())) }
}

fn edges_of_faces(m: &Mesh, faces: impl IntoIterator<Item = u32>) -> Vec<[u32; 2]> {
    let mut out = BTreeSet::new();
    for fi in faces {
        let f = &m.faces[fi as usize];
        for i in 0..f.len() {
            let k = ek(f[i], f[(i + 1) % f.len()]);
            out.insert([k.0, k.1]);
        }
    }
    out.into_iter().collect()
}

fn verts_of_faces(m: &Mesh, faces: impl IntoIterator<Item = u32>) -> Vec<u32> {
    let mut out = BTreeSet::new();
    for fi in faces {
        out.extend(m.faces[fi as usize].iter().copied());
    }
    out.into_iter().collect()
}

/// The selection as a face selection with its edges and vertices (what a face-mode tool
/// leaves selected).
fn face_selection(m: &Mesh, faces: Vec<u32>) -> Elements {
    Elements {
        vertices: verts_of_faces(m, faces.iter().copied()),
        edges: edges_of_faces(m, faces.iter().copied()),
        faces,
    }
}

// ---------------------------------------------------------------------------------------------
// transform

/// Move, turn and scale the selected vertices about `pivot` (default: their centre).
/// `rotate` is `[x, y, z]` degrees (YXZ order like entity rotation). Applied as scale, rotate,
/// translate.
pub fn transform(
    m: &Mesh,
    sel: &Elements,
    translate: Vec3,
    rotate: Vec3,
    scale: Vec3,
    pivot: Option<Vec3>,
) -> Result<(Mesh, Elements)> {
    let f = flush(m, sel)?;
    need(!f.verts.is_empty(), "select some vertices, edges or faces to transform")?;
    let center = pivot.unwrap_or_else(|| selection_center(m, &f.verts));
    let q = Quat::from_euler(EulerRot::YXZ, rotate.y.to_radians(), rotate.x.to_radians(), rotate.z.to_radians());
    let mut out = m.clone();
    for &v in &f.verts {
        let p = pos(m, v) - center;
        out.positions[v as usize] = (center + q * (p * scale) + translate).to_array();
    }
    Ok((out, sel.clone().normalized()))
}

pub fn selection_center(m: &Mesh, verts: &BTreeSet<u32>) -> Vec3 {
    if verts.is_empty() {
        return Vec3::ZERO;
    }
    verts.iter().map(|&v| pos(m, v)).sum::<Vec3>() / verts.len() as f32
}

/// Area-weighted average normal of some faces.
pub fn region_normal(m: &Mesh, faces: impl IntoIterator<Item = u32>) -> Vec3 {
    let n: Vec3 = faces.into_iter().map(|f| face_normal_raw(m, &m.faces[f as usize])).sum();
    n.normalize_or_zero()
}

/// Per-vertex normal over some faces.
fn vertex_normals(m: &Mesh, faces: &BTreeSet<u32>) -> HashMap<u32, Vec3> {
    let mut out: HashMap<u32, Vec3> = HashMap::new();
    for &fi in faces {
        let f = &m.faces[fi as usize];
        let n = face_normal_raw(m, f);
        for &v in f {
            *out.entry(v).or_default() += n;
        }
    }
    for n in out.values_mut() {
        *n = n.normalize_or_zero();
    }
    out
}

// ---------------------------------------------------------------------------------------------
// extrude

/// Duplicate a face region's vertices, move the region onto the copies and stitch side walls
/// along its border. Returns old vertex -> copy. Unused originals are dropped by `finish`.
fn extrude_region_raw(m: &mut Mesh, region: &BTreeSet<u32>) -> (HashMap<u32, u32>, Vec<(u32, u32)>) {
    let mut half: HashSet<(u32, u32)> = HashSet::new();
    for &fi in region {
        let f = &m.faces[fi as usize];
        for i in 0..f.len() {
            half.insert((f[i], f[(i + 1) % f.len()]));
        }
    }
    let mut dup: HashMap<u32, u32> = HashMap::new();
    for &fi in region {
        for i in 0..m.faces[fi as usize].len() {
            let v = m.faces[fi as usize][i];
            if let std::collections::hash_map::Entry::Vacant(slot) = dup.entry(v) {
                let p = Vec3::from_array(m.positions[v as usize]);
                m.positions.push(p.to_array());
                slot.insert(m.positions.len() as u32 - 1);
            }
        }
    }
    // border half-edges in face order (stable output)
    let mut border = Vec::new();
    for &fi in region {
        let f = m.faces[fi as usize].clone();
        for i in 0..f.len() {
            let (a, b) = (f[i], f[(i + 1) % f.len()]);
            if !half.contains(&(b, a)) {
                border.push((a, b));
            }
        }
    }
    for &(a, b) in &border {
        m.faces.push(vec![a, b, dup[&b], dup[&a]]);
    }
    for &fi in region {
        for v in m.faces[fi as usize].iter_mut() {
            *v = dup[v];
        }
    }
    (dup, border)
}

/// How far and where extruded geometry moves.
#[derive(Clone, Copy, Debug, Default)]
pub struct Offset {
    /// Along the normal (faces) or outward in the surface (edges).
    pub distance: f32,
    /// Extra fixed vector in mesh space.
    pub vector: Vec3,
}

/// Extrude selected faces (as one region, or each on its own with `individual`), else
/// selected edges (new faces hang off them). Leaves the extruded faces / edges selected.
pub fn extrude(m: &Mesh, sel: &Elements, offset: Offset, individual: bool) -> Result<(Mesh, Elements)> {
    check_finite(offset.distance, "distance")?;
    let f = flush(m, sel)?;
    if !f.faces.is_empty() {
        let mut out = m.clone();
        let groups: Vec<BTreeSet<u32>> =
            if individual { f.faces.iter().map(|&x| BTreeSet::from([x])).collect() } else { vec![f.faces.clone()] };
        for g in groups {
            let n = region_normal(&out, g.iter().copied());
            let (dup, _) = extrude_region_raw(&mut out, &g);
            for &c in dup.values() {
                let p = pos(&out, c) + n * offset.distance + offset.vector;
                out.positions[c as usize] = p.to_array();
            }
        }
        let faces: Vec<u32> = f.faces.iter().copied().collect();
        let sel = face_selection(&out, faces);
        return Ok(finish(out, sel));
    }
    need(!f.edges.is_empty(), "select faces or edges to extrude (vertices alone cannot be extruded)")?;
    let topo = Topology::new(m);
    let mut out = m.clone();
    let mut dup: HashMap<u32, u32> = HashMap::new();
    let mut outward: HashMap<u32, Vec3> = HashMap::new();
    let mut new_edges = Vec::new();
    for &e in &f.edges {
        // orient like the face that owns it, so the new face continues the surface
        let (mut a, mut b) = e;
        let mut dir_out = Vec3::ZERO;
        if let Some(&fi) = topo.faces_of(e).first() {
            let face = &m.faces[fi];
            let i = face.iter().position(|&v| v == e.0).unwrap_or(0);
            if face[(i + 1) % face.len()] == e.1 {
                (a, b) = (e.0, e.1);
            } else {
                (a, b) = (e.1, e.0);
            }
            let n = face_normal(m, face);
            dir_out = (pos(m, b) - pos(m, a)).cross(n).normalize_or_zero();
        }
        for v in [a, b] {
            if let std::collections::hash_map::Entry::Vacant(slot) = dup.entry(v) {
                slot.insert(push_vertex(&mut out, pos(m, v)));
            }
            *outward.entry(v).or_default() += dir_out;
        }
        out.faces.push(vec![b, a, dup[&a], dup[&b]]);
        new_edges.push([dup[&a], dup[&b]]);
    }
    for (v, c) in &dup {
        let d = outward.get(v).copied().unwrap_or_default().normalize_or_zero();
        let p = pos(m, *v) + d * offset.distance + offset.vector;
        out.positions[*c as usize] = p.to_array();
    }
    let sel = Elements { vertices: dup.values().copied().collect(), edges: new_edges, faces: vec![] };
    Ok(finish(out, sel))
}

// ---------------------------------------------------------------------------------------------
// inset

/// Inset selected faces: a smaller copy inside each region (or each face with `individual`),
/// `thickness` in from the border and raised by `depth` along the normal. The inner faces stay
/// selected.
pub fn inset(m: &Mesh, sel: &Elements, thickness: f32, depth: f32, individual: bool) -> Result<(Mesh, Elements)> {
    check_finite(thickness, "thickness")?;
    check_finite(depth, "depth")?;
    let f = flush(m, sel)?;
    need(!f.faces.is_empty(), "select faces to inset")?;
    let mut out = m.clone();
    let groups: Vec<BTreeSet<u32>> =
        if individual { f.faces.iter().map(|&x| BTreeSet::from([x])).collect() } else { vec![f.faces.clone()] };
    for g in groups {
        let normals = vertex_normals(&out, &g);
        let before = out.clone();
        let (dup, border) = extrude_region_raw(&mut out, &g);
        let mut incoming: HashMap<u32, Vec3> = HashMap::new();
        let mut outgoing: HashMap<u32, Vec3> = HashMap::new();
        for &(a, b) in &border {
            let n = (normals[&a] + normals[&b]).normalize_or_zero();
            let e = (pos(&before, b) - pos(&before, a)).normalize_or_zero();
            let inward = n.cross(e).normalize_or_zero();
            outgoing.entry(a).or_insert(inward);
            incoming.entry(b).or_insert(inward);
        }
        for (&v, &c) in &dup {
            let mut p = pos(&before, v);
            if let (Some(i1), Some(i2)) = (incoming.get(&v), outgoing.get(&v)) {
                let denom = (1.0 + i1.dot(*i2)).max(0.15);
                p += (*i1 + *i2) * thickness / denom;
            }
            p += normals[&v] * depth;
            out.positions[c as usize] = p.to_array();
        }
    }
    let faces: Vec<u32> = f.faces.iter().copied().collect();
    let sel = face_selection(&out, faces);
    Ok(finish(out, sel))
}

// ---------------------------------------------------------------------------------------------
// bevel

/// Bevel (chamfer) selected edges: each becomes a flat strip `width` wide, with corner patches
/// where bevelled edges meet. `segments` > 1 rounds the strip. Only edges shared by exactly two
/// faces are bevelled. The new strip faces stay selected.
pub fn bevel(m: &Mesh, sel: &Elements, width: f32, segments: u32) -> Result<(Mesh, Elements)> {
    check_finite(width, "width")?;
    need(width > 0.0, "bevel width must be above 0")?;
    need((1..=16).contains(&segments), "bevel segments must be 1..=16")?;
    let topo = Topology::new(m);
    let fl = flush_with(m, &topo, sel)?;
    let edges: BTreeSet<Edge> = fl.edges.iter().copied().filter(|e| topo.faces_of(*e).len() == 2).collect();
    need(!edges.is_empty(), "select edges shared by two faces to bevel")?;
    let bverts: HashSet<u32> = edges.iter().flat_map(|e| [e.0, e.1]).collect();

    let mut out = Mesh { positions: m.positions.clone(), faces: Vec::new() };
    // origin of every vertex (new ones come from the vertex they were split off)
    let mut origin: Vec<u32> = (0..m.positions.len() as u32).collect();
    let mut slides: HashMap<(u32, u32), u32> = HashMap::new();
    let mut corners: HashMap<(usize, u32), u32> = HashMap::new();
    let add = |out: &mut Mesh, origin: &mut Vec<u32>, from: u32, p: Vec3| {
        origin.push(from);
        push_vertex(out, p)
    };
    // pass 1: new vertices
    for (fi, f) in m.faces.iter().enumerate() {
        let n = face_normal(m, f);
        for i in 0..f.len() {
            let v = f[i];
            if !bverts.contains(&v) {
                continue;
            }
            let p = f[(i + f.len() - 1) % f.len()];
            let nx = f[(i + 1) % f.len()];
            let (sp, sn) = (edges.contains(&ek(p, v)), edges.contains(&ek(v, nx)));
            let pv = pos(m, v);
            match (sp, sn) {
                (true, true) => {
                    let i1 = n.cross((pv - pos(m, p)).normalize_or_zero()).normalize_or_zero();
                    let i2 = n.cross((pos(m, nx) - pv).normalize_or_zero()).normalize_or_zero();
                    let denom = (1.0 + i1.dot(i2)).max(0.15);
                    let mut x = (i1 + i2) * width / denom;
                    let lim = 0.49 * (pos(m, p) - pv).length().min((pos(m, nx) - pv).length()) * 1.5;
                    if x.length() > lim {
                        x = x.normalize_or_zero() * lim;
                    }
                    let c = add(&mut out, &mut origin, v, pv + x);
                    corners.insert((fi, v), c);
                }
                (false, true) | (true, false) => {
                    let (along, other) = if sn { (p, nx) } else { (nx, p) };
                    if slides.contains_key(&(v, along)) {
                        continue;
                    }
                    let d = pos(m, along) - pv;
                    let o = pos(m, other) - pv;
                    let sin = d.normalize_or_zero().cross(o.normalize_or_zero()).length().max(0.1);
                    let len = d.length();
                    let t = (width / sin).min(len * 0.49);
                    let c = add(&mut out, &mut origin, v, pv + d.normalize_or_zero() * t);
                    slides.insert((v, along), c);
                }
                (false, false) => {}
            }
        }
    }
    // pass 2: the original faces with their corners replaced
    let mut rep: HashMap<(usize, u32), u32> = HashMap::new();
    for (fi, f) in m.faces.iter().enumerate() {
        let mut g = Vec::with_capacity(f.len() + 2);
        for i in 0..f.len() {
            let v = f[i];
            if !bverts.contains(&v) {
                g.push(v);
                continue;
            }
            let p = f[(i + f.len() - 1) % f.len()];
            let nx = f[(i + 1) % f.len()];
            let (sp, sn) = (edges.contains(&ek(p, v)), edges.contains(&ek(v, nx)));
            match (sp, sn) {
                (true, true) => {
                    let c = corners[&(fi, v)];
                    rep.insert((fi, v), c);
                    g.push(c);
                }
                (false, true) => {
                    let c = slides[&(v, p)];
                    rep.insert((fi, v), c);
                    g.push(c);
                }
                (true, false) => {
                    let c = slides[&(v, nx)];
                    rep.insert((fi, v), c);
                    g.push(c);
                }
                (false, false) => match (slides.get(&(v, p)), slides.get(&(v, nx))) {
                    (Some(&a), Some(&b)) => g.extend([a, b]),
                    (Some(&a), None) => g.extend([a, v]),
                    (None, Some(&b)) => g.extend([v, b]),
                    (None, None) => g.push(v),
                },
            }
        }
        out.faces.push(g);
    }
    // strips
    let mut strip_faces = Vec::new();
    let mut strips: Vec<[u32; 4]> = Vec::new();
    for &e in &edges {
        let fs = topo.faces_of(e);
        let (f1, f2) = (fs[0], fs[1]);
        let face = &m.faces[f1];
        let i = face.iter().position(|&v| v == e.0).unwrap_or(0);
        let (a, b) = if face[(i + 1) % face.len()] == e.1 { (e.0, e.1) } else { (e.1, e.0) };
        // f1 runs a -> b, f2 runs b -> a
        let (Some(&n1a), Some(&n1b), Some(&n2a), Some(&n2b)) =
            (rep.get(&(f1, a)), rep.get(&(f1, b)), rep.get(&(f2, a)), rep.get(&(f2, b)))
        else {
            continue;
        };
        strips.push([n1b, n1a, n2a, n2b]);
    }
    // corner patches: holes whose border vertices all came from one original vertex
    let fill_holes = |out: &mut Mesh, origin: &[u32]| -> Vec<u32> {
        let mut half: HashSet<(u32, u32)> = HashSet::new();
        for f in &out.faces {
            for i in 0..f.len() {
                half.insert((f[i], f[(i + 1) % f.len()]));
            }
        }
        let mut next: HashMap<u32, u32> = HashMap::new();
        for &(a, b) in &half {
            if !half.contains(&(b, a))
                && origin[a as usize] == origin[b as usize]
                && bverts.contains(&origin[a as usize])
            {
                next.entry(a).or_insert(b);
            }
        }
        let mut starts: Vec<u32> = next.keys().copied().collect();
        starts.sort_unstable();
        let mut seen = HashSet::new();
        let mut added = Vec::new();
        for s in starts {
            if seen.contains(&s) {
                continue;
            }
            let mut cycle = vec![s];
            let mut cur = s;
            let mut closed = false;
            while let Some(&nx) = next.get(&cur) {
                if nx == s {
                    closed = true;
                    break;
                }
                if cycle.contains(&nx) || cycle.len() > 64 {
                    break;
                }
                cycle.push(nx);
                cur = nx;
            }
            seen.extend(cycle.iter().copied());
            if closed && cycle.len() >= 3 {
                cycle.reverse();
                added.push(out.faces.len() as u32);
                out.faces.push(cycle);
            }
        }
        added
    };
    if segments == 1 {
        for s in &strips {
            strip_faces.push(out.faces.len() as u32);
            out.faces.push(s.to_vec());
        }
        strip_faces.extend(fill_holes(&mut out, &origin));
    } else {
        // round: split each strip across its width into `segments` rows that bulge out towards
        // where the original edge was (a quadratic Bézier through the old corner)
        let mut cross: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
        for s in &strips {
            let [n1b, n1a, n2a, n2b] = *s;
            let mut rows: Vec<Vec<u32>> = Vec::new();
            for (from, to, orig) in [(n1a, n2a, origin[n1a as usize]), (n1b, n2b, origin[n1b as usize])] {
                let key = (from, to);
                let row = if let Some(r) = cross.get(&key) {
                    r.clone()
                } else if let Some(r) = cross.get(&(to, from)) {
                    r.iter().rev().copied().collect()
                } else {
                    let (p0, p2, c) = (pos(&out, from), pos(&out, to), pos(m, orig));
                    let mut r = vec![from];
                    for k in 1..segments {
                        let t = k as f32 / segments as f32;
                        let p = p0 * (1.0 - t) * (1.0 - t) + c * 2.0 * t * (1.0 - t) + p2 * t * t;
                        r.push(add(&mut out, &mut origin, orig, p));
                    }
                    r.push(to);
                    cross.insert(key, r.clone());
                    r
                };
                rows.push(row);
            }
            let (ra, rb) = (&rows[0], &rows[1]);
            for k in 0..segments as usize {
                strip_faces.push(out.faces.len() as u32);
                out.faces.push(vec![rb[k], ra[k], ra[k + 1], rb[k + 1]]);
            }
        }
        // the original faces' edges that a strip end now subdivides need the new points too
        let mut split_edges: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
        for ((a, b), r) in &cross {
            split_edges.insert((*a, *b), r[1..r.len() - 1].to_vec());
        }
        for f in out.faces.iter_mut() {
            let mut g = Vec::with_capacity(f.len());
            for i in 0..f.len() {
                let (a, b) = (f[i], f[(i + 1) % f.len()]);
                g.push(a);
                if let Some(mid) = split_edges.get(&(a, b)) {
                    g.extend(mid.iter().copied());
                } else if let Some(mid) = split_edges.get(&(b, a)) {
                    g.extend(mid.iter().rev().copied());
                }
            }
            *f = g;
        }
        // the strips themselves were built with the points: undo the doubled insertions
        for &sf in &strip_faces {
            let f = &mut out.faces[sf as usize];
            let mut g: Vec<u32> = Vec::new();
            for &v in f.iter() {
                if !g.contains(&v) {
                    g.push(v);
                }
            }
            *f = g;
        }
        strip_faces.extend(fill_holes(&mut out, &origin));
    }
    let sel = face_selection(&out, strip_faces);
    Ok(finish(out, sel))
}

// ---------------------------------------------------------------------------------------------
// loops and rings

/// The quad ring across `edge`: ring edges oriented consistently (first one as given) and the
/// quads between them. `closed` when it wraps around.
pub struct Ring {
    pub edges: Vec<(u32, u32)>,
    pub faces: Vec<usize>,
    pub closed: bool,
}

fn other_in_quad(f: &[u32], u: u32, not: u32) -> Option<u32> {
    let i = f.iter().position(|&x| x == u)?;
    let a = f[(i + 1) % 4];
    let b = f[(i + 3) % 4];
    if a == not {
        Some(b)
    } else if b == not {
        Some(a)
    } else {
        None
    }
}

pub fn edge_ring(m: &Mesh, topo: &Topology, edge: (u32, u32)) -> Result<Ring> {
    let k = ek(edge.0, edge.1);
    need(topo.edge_faces.contains_key(&k), &format!("edge [{}, {}] does not exist", edge.0, edge.1))?;
    let walk = |start_face: Option<usize>| -> (Vec<(u32, u32)>, Vec<usize>, bool) {
        let mut edges = Vec::new();
        let mut faces = Vec::new();
        let (mut u, mut v) = edge;
        let mut face = start_face;
        while let Some(fi) = face {
            let f = &m.faces[fi];
            if f.len() != 4 || faces.contains(&fi) {
                break;
            }
            let (Some(u2), Some(v2)) = (other_in_quad(f, u, v), other_in_quad(f, v, u)) else { break };
            faces.push(fi);
            if ek(u2, v2) == k {
                return (edges, faces, true);
            }
            edges.push((u2, v2));
            face = topo.faces_of(ek(u2, v2)).iter().copied().find(|&g| g != fi);
            (u, v) = (u2, v2);
            if faces.len() > m.faces.len() {
                break;
            }
        }
        (edges, faces, false)
    };
    let fs = topo.faces_of(k).to_vec();
    let (e1, f1, closed) = walk(fs.first().copied());
    if closed {
        let mut edges = vec![edge];
        edges.extend(e1);
        return Ok(Ring { edges, faces: f1, closed: true });
    }
    let (e2, f2, _) = walk(fs.get(1).copied());
    let mut edges: Vec<(u32, u32)> = e2.into_iter().rev().collect();
    let mut faces: Vec<usize> = f2.into_iter().rev().collect();
    edges.push(edge);
    edges.extend(e1);
    faces.extend(f1);
    faces.dedup();
    Ok(Ring { edges, faces, closed: false })
}

/// Edges of the loop through `edge`: across 4-valent vertices it continues straight on; along a
/// mesh border it follows the border.
pub fn edge_loop(m: &Mesh, topo: &Topology, edge: (u32, u32)) -> Result<Vec<Edge>> {
    let k = ek(edge.0, edge.1);
    need(topo.edge_faces.contains_key(&k), &format!("edge [{}, {}] does not exist", edge.0, edge.1))?;
    let boundary = topo.faces_of(k).len() == 1;
    let step = |from: u32, at: u32| -> Option<u32> {
        let cur = ek(from, at);
        let around = topo.neighbours(m, at);
        if boundary {
            let cands: Vec<u32> =
                around.into_iter().filter(|&w| w != from && topo.faces_of(ek(at, w)).len() == 1).collect();
            return (cands.len() == 1).then(|| cands[0]);
        }
        if around.len() != 4 {
            return None;
        }
        let cur_faces = topo.faces_of(cur);
        let cands: Vec<u32> = around
            .into_iter()
            .filter(|&w| w != from && topo.faces_of(ek(at, w)).iter().all(|f| !cur_faces.contains(f)))
            .collect();
        (cands.len() == 1).then(|| cands[0])
    };
    let mut set: Vec<Edge> = vec![k];
    for (mut a, mut b) in [(edge.0, edge.1), (edge.1, edge.0)] {
        while let Some(c) = step(a, b) {
            let e = ek(b, c);
            if set.contains(&e) {
                break;
            }
            set.push(e);
            (a, b) = (b, c);
            if set.len() > topo.edges.len() {
                break;
            }
        }
    }
    Ok(set)
}

/// Faces connected to the selection (through shared edges).
pub fn linked(m: &Mesh, sel: &Elements) -> Result<Elements> {
    let topo = Topology::new(m);
    let f = flush_with(m, &topo, sel)?;
    let mut seen: HashSet<usize> = HashSet::new();
    let mut stack: Vec<usize> = Vec::new();
    for &v in &f.verts {
        stack.extend(topo.vert_faces[v as usize].iter().copied());
    }
    while let Some(fi) = stack.pop() {
        if !seen.insert(fi) {
            continue;
        }
        let face = &m.faces[fi];
        for i in 0..face.len() {
            for &g in topo.faces_of(ek(face[i], face[(i + 1) % face.len()])) {
                if !seen.contains(&g) {
                    stack.push(g);
                }
            }
        }
    }
    let mut faces: Vec<u32> = seen.into_iter().map(|x| x as u32).collect();
    faces.sort_unstable();
    Ok(face_selection(m, faces))
}

/// Grow the selection by one step (faces touching it).
pub fn grow(m: &Mesh, sel: &Elements) -> Result<Elements> {
    let topo = Topology::new(m);
    let f = flush_with(m, &topo, sel)?;
    let mut faces: BTreeSet<u32> = f.faces.clone();
    for &v in &f.verts {
        faces.extend(topo.vert_faces[v as usize].iter().map(|&x| x as u32));
    }
    if sel.faces.is_empty() {
        let verts: BTreeSet<u32> = verts_of_faces(m, faces.iter().copied()).into_iter().collect();
        let mut out = Elements::vertices(verts.iter().copied());
        if !sel.edges.is_empty() {
            out = Elements::edges(edges_of_faces(m, faces.iter().copied()));
        }
        return Ok(out.normalized());
    }
    Ok(face_selection(m, faces.into_iter().collect()))
}

// ---------------------------------------------------------------------------------------------
// loop cut, subdivide, connect

/// Cut `cuts` new edge loops across the quad ring of `edge`. With one cut, `factor` (0..1)
/// slides it along the ring edges (0.5 = middle). Selects the new loop(s).
pub fn loop_cut(m: &Mesh, edge: [u32; 2], cuts: u32, factor: f32) -> Result<(Mesh, Elements)> {
    need((1..=64).contains(&cuts), "cuts must be 1..=64")?;
    need(factor.is_finite() && factor > 0.0 && factor < 1.0, "factor must be between 0 and 1")?;
    let topo = Topology::new(m);
    let ring = edge_ring(m, &topo, (edge[0], edge[1]))?;
    need(!ring.faces.is_empty(), "the edge has no quads next to it to cut through")?;
    let mut out = m.clone();
    // new points on each ring edge, in the ring's direction
    let mut points: HashMap<Edge, (u32, Vec<u32>)> = HashMap::new();
    for &(u, v) in &ring.edges {
        let (pu, pv) = (pos(m, u), pos(m, v));
        let mut pts = Vec::new();
        for i in 1..=cuts {
            let t = if cuts == 1 { factor } else { i as f32 / (cuts + 1) as f32 };
            pts.push(push_vertex(&mut out, pu.lerp(pv, t)));
        }
        points.insert(ek(u, v), (u, pts));
    }
    let along = |a: u32, b: u32| -> Option<Vec<u32>> {
        let (from, pts) = points.get(&ek(a, b))?;
        Some(if *from == a { pts.clone() } else { pts.iter().rev().copied().collect() })
    };
    let ring_faces: HashSet<usize> = ring.faces.iter().copied().collect();
    let mut faces = Vec::with_capacity(m.faces.len() + ring.faces.len() * cuts as usize);
    let mut loops: Vec<[u32; 2]> = Vec::new();
    for (fi, f) in m.faces.iter().enumerate() {
        if ring_faces.contains(&fi) {
            // the two ring edges of this quad
            let i = (0..4).find(|&i| points.contains_key(&ek(f[i], f[(i + 1) % 4]))).unwrap_or(0);
            let (a0, b0) = (f[i], f[(i + 1) % 4]);
            let (b1, a1) = (f[(i + 2) % 4], f[(i + 3) % 4]);
            let mut row0 = vec![a0];
            row0.extend(along(a0, b0).unwrap_or_default());
            row0.push(b0);
            let mut row1 = vec![a1];
            row1.extend(along(a1, b1).unwrap_or_default());
            row1.push(b1);
            if row0.len() != row1.len() {
                faces.push(f.clone());
                continue;
            }
            for k in 0..row0.len() - 1 {
                faces.push(vec![row0[k], row0[k + 1], row1[k + 1], row1[k]]);
                if k > 0 {
                    loops.push([row0[k], row1[k]]);
                }
            }
            continue;
        }
        let mut g = Vec::with_capacity(f.len());
        for i in 0..f.len() {
            let (a, b) = (f[i], f[(i + 1) % f.len()]);
            g.push(a);
            if let Some(pts) = along(a, b) {
                g.extend(pts);
            }
        }
        faces.push(g);
    }
    out.faces = faces;
    let sel = Elements::edges(loops);
    Ok(finish(out, sel))
}

/// Split selected faces into quads around their centre (a quad becomes four); neighbours get
/// the new edge midpoints so the surface stays closed. Selects the new faces.
pub fn subdivide(m: &Mesh, sel: &Elements) -> Result<(Mesh, Elements)> {
    let f = flush(m, sel)?;
    need(!f.faces.is_empty(), "select faces to subdivide")?;
    let mut out = m.clone();
    let mut mids: HashMap<Edge, u32> = HashMap::new();
    for &fi in &f.faces {
        let face = &m.faces[fi as usize];
        for i in 0..face.len() {
            let k = ek(face[i], face[(i + 1) % face.len()]);
            if let std::collections::hash_map::Entry::Vacant(slot) = mids.entry(k) {
                let p = (pos(m, k.0) + pos(m, k.1)) / 2.0;
                slot.insert(push_vertex(&mut out, p));
            }
        }
    }
    let mut faces = Vec::new();
    let mut new_sel = Vec::new();
    for (fi, face) in m.faces.iter().enumerate() {
        let n = face.len();
        if f.faces.contains(&(fi as u32)) {
            let c = push_vertex(&mut out, face_center(m, face));
            for i in 0..n {
                let prev = face[(i + n - 1) % n];
                let cur = face[i];
                let next = face[(i + 1) % n];
                new_sel.push(faces.len() as u32);
                faces.push(vec![cur, mids[&ek(cur, next)], c, mids[&ek(prev, cur)]]);
            }
        } else {
            let mut g = Vec::with_capacity(n);
            for i in 0..n {
                let (a, b) = (face[i], face[(i + 1) % n]);
                g.push(a);
                if let Some(&mid) = mids.get(&ek(a, b)) {
                    g.push(mid);
                }
            }
            faces.push(g);
        }
    }
    out.faces = faces;
    let sel = face_selection(&out, new_sel);
    Ok(finish(out, sel))
}

/// Split faces between pairs of selected vertices that share a face (Blender's "connect
/// vertex path", J). Selects the new edges.
pub fn connect(m: &Mesh, sel: &Elements) -> Result<(Mesh, Elements)> {
    let f = flush(m, sel)?;
    need(f.verts.len() >= 2, "select at least two vertices on the same face")?;
    let mut out = Mesh { positions: m.positions.clone(), faces: Vec::new() };
    let mut new_edges = Vec::new();
    for face in &m.faces {
        let picked: Vec<usize> = (0..face.len()).filter(|&i| f.verts.contains(&face[i])).collect();
        let n = face.len();
        let split = picked.len() == 2 && {
            let d = picked[1] - picked[0];
            d != 1 && d != n - 1
        };
        if !split {
            out.faces.push(face.clone());
            continue;
        }
        let (i, j) = (picked[0], picked[1]);
        out.faces.push(face[i..=j].to_vec());
        let mut other: Vec<u32> = face[j..].to_vec();
        other.extend_from_slice(&face[..=i]);
        out.faces.push(other);
        new_edges.push([face[i], face[j]]);
    }
    need(!new_edges.is_empty(), "no face has exactly two of the selected vertices away from each other")?;
    let sel = Elements::edges(new_edges);
    Ok(finish(out, sel))
}

// ---------------------------------------------------------------------------------------------
// delete, merge, fill, flip

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DeleteMode {
    /// Remove the vertices and every face using them.
    Vertices,
    /// Remove faces along the selected edges (and vertices left with no face).
    Edges,
    /// Remove the selected faces (and vertices left with no face).
    Faces,
}

pub fn delete(m: &Mesh, sel: &Elements, mode: Option<DeleteMode>) -> Result<(Mesh, Elements)> {
    let topo = Topology::new(m);
    let f = flush_with(m, &topo, sel)?;
    let mode = mode.unwrap_or(if !sel.faces.is_empty() {
        DeleteMode::Faces
    } else if !sel.edges.is_empty() {
        DeleteMode::Edges
    } else {
        DeleteMode::Vertices
    });
    let kill: HashSet<usize> = match mode {
        DeleteMode::Vertices => f.verts.iter().flat_map(|&v| topo.vert_faces[v as usize].iter().copied()).collect(),
        DeleteMode::Edges => f.edges.iter().flat_map(|&e| topo.faces_of(e).iter().copied()).collect(),
        DeleteMode::Faces => f.faces.iter().map(|&x| x as usize).collect(),
    };
    need(!kill.is_empty(), "nothing selected to delete")?;
    let mut out = m.clone();
    out.faces = m.faces.iter().enumerate().filter(|(i, _)| !kill.contains(i)).map(|(_, f)| f.clone()).collect();
    Ok(finish(out, Elements::default()))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MergeAt {
    /// The centre of the selected vertices.
    #[default]
    Center,
    /// The first selected vertex.
    First,
    /// The last selected vertex.
    Last,
}

/// Merge the selected vertices into one.
pub fn merge(m: &Mesh, sel: &Elements, at: MergeAt) -> Result<(Mesh, Elements)> {
    let f = flush(m, sel)?;
    need(f.verts.len() >= 2, "select at least two vertices to merge")?;
    // "first" / "last" follow the order given, falling back to index order
    let order: Vec<u32> =
        if !sel.vertices.is_empty() { sel.vertices.clone() } else { f.verts.iter().copied().collect() };
    let target = match at {
        MergeAt::Center => selection_center(m, &f.verts),
        MergeAt::First => pos(m, order[0]),
        MergeAt::Last => pos(m, *order.last().expect("non-empty")),
    };
    let keep = *f.verts.iter().next().expect("non-empty");
    let mut out = m.clone();
    out.positions[keep as usize] = target.to_array();
    for face in out.faces.iter_mut() {
        for v in face.iter_mut() {
            if f.verts.contains(v) {
                *v = keep;
            }
        }
    }
    Ok(finish(out, Elements::vertices([keep])))
}

/// Merge vertices closer than `distance` (cleans up imported or mirrored meshes). Returns how
/// many vertices were removed with the result.
pub fn merge_by_distance(m: &Mesh, sel: &Elements, distance: f32) -> Result<(Mesh, Elements, usize)> {
    check_finite(distance, "distance")?;
    need(distance >= 0.0, "distance must be 0 or more")?;
    let only: Option<BTreeSet<u32>> = if sel.is_empty() { None } else { Some(flush(m, sel)?.verts) };
    let cell = distance.max(1e-6);
    let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
    let mut map: Vec<u32> = (0..m.positions.len() as u32).collect();
    let key = |p: Vec3| ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64);
    for v in 0..m.positions.len() as u32 {
        if only.as_ref().is_some_and(|s| !s.contains(&v)) {
            continue;
        }
        let p = pos(m, v);
        let (x, y, z) = key(p);
        let mut found = None;
        'look: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(list) = grid.get(&(x + dx, y + dy, z + dz)) {
                        for &w in list {
                            if pos(m, w).distance(p) <= distance {
                                found = Some(w);
                                break 'look;
                            }
                        }
                    }
                }
            }
        }
        match found {
            Some(w) => map[v as usize] = w,
            None => grid.entry((x, y, z)).or_default().push(v),
        }
    }
    let mut out = m.clone();
    for face in out.faces.iter_mut() {
        for v in face.iter_mut() {
            *v = map[*v as usize];
        }
    }
    let before = m.positions.len();
    let (out, sel) = finish(out, Elements::default());
    let removed = before.saturating_sub(out.positions.len());
    Ok((out, sel, removed))
}

/// Make a face from the selection: a closed chain of border edges, or 3+ vertices (ordered
/// around their centre).
pub fn fill(m: &Mesh, sel: &Elements) -> Result<(Mesh, Elements)> {
    let topo = Topology::new(m);
    let f = flush_with(m, &topo, sel)?;
    need(f.verts.len() >= 3, "select at least three vertices (or a closed ring of border edges) to fill")?;
    // border edges in the selection, oriented the opposite way to their face
    let mut next: HashMap<u32, u32> = HashMap::new();
    for &e in &f.edges {
        let fs = topo.faces_of(e);
        if fs.len() != 1 {
            continue;
        }
        let face = &m.faces[fs[0]];
        let i = face.iter().position(|&v| v == e.0).unwrap_or(0);
        let (a, b) = if face[(i + 1) % face.len()] == e.1 { (e.0, e.1) } else { (e.1, e.0) };
        next.insert(b, a);
    }
    let mut out = m.clone();
    let mut new_faces = Vec::new();
    let mut seen = HashSet::new();
    let mut starts: Vec<u32> = next.keys().copied().collect();
    starts.sort_unstable();
    for s in starts {
        if seen.contains(&s) {
            continue;
        }
        let mut cyc = vec![s];
        let mut cur = s;
        let mut closed = false;
        while let Some(&nx) = next.get(&cur) {
            if nx == s {
                closed = true;
                break;
            }
            if cyc.contains(&nx) {
                break;
            }
            cyc.push(nx);
            cur = nx;
        }
        seen.extend(cyc.iter().copied());
        if closed && cyc.len() >= 3 {
            new_faces.push(out.faces.len() as u32);
            out.faces.push(cyc);
        }
    }
    if new_faces.is_empty() {
        // order the vertices around their centre in their best-fit plane
        let verts: Vec<u32> = f.verts.iter().copied().collect();
        let c = selection_center(m, &f.verts);
        let mut n = Vec3::ZERO;
        for i in 0..verts.len() {
            let a = pos(m, verts[i]) - c;
            let b = pos(m, verts[(i + 1) % verts.len()]) - c;
            n += a.cross(b);
        }
        let n = if n.length_squared() < 1e-12 { Vec3::Y } else { n.normalize() };
        let u = n.any_orthonormal_vector();
        let w = n.cross(u);
        let mut ordered = verts.clone();
        ordered.sort_by(|&a, &b| {
            let pa = pos(m, a) - c;
            let pb = pos(m, b) - c;
            let ta = pa.dot(w).atan2(pa.dot(u));
            let tb = pb.dot(w).atan2(pb.dot(u));
            ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
        });
        // face the same way as the neighbouring surface, if any
        let around: Vec3 = verts
            .iter()
            .flat_map(|&v| topo.vert_faces[v as usize].iter())
            .map(|&fi| face_normal(m, &m.faces[fi]))
            .sum();
        let mut tmp = ordered.clone();
        if face_normal(m, &tmp).dot(around) < 0.0 {
            tmp.reverse();
        }
        ordered = tmp;
        new_faces.push(out.faces.len() as u32);
        out.faces.push(ordered);
    }
    let sel = face_selection(&out, new_faces);
    Ok(finish(out, sel))
}

/// Turn selected faces (all faces if nothing is selected) inside out.
pub fn flip(m: &Mesh, sel: &Elements) -> Result<(Mesh, Elements)> {
    let mut out = m.clone();
    let faces: Vec<u32> =
        if sel.is_empty() { (0..m.faces.len() as u32).collect() } else { flush(m, sel)?.faces.into_iter().collect() };
    need(!faces.is_empty(), "select faces to flip")?;
    for &f in &faces {
        out.faces[f as usize].reverse();
    }
    Ok((out, sel.clone().normalized()))
}

// ---------------------------------------------------------------------------------------------
// bridge, edge slide, bisect / knife

/// Border edges of a selection grouped into chains, each oriented the way a new face on the
/// open side would run along it. `closed` chains are loops.
fn border_chains(m: &Mesh, topo: &Topology, edges: &BTreeSet<Edge>) -> Result<Vec<(Vec<u32>, bool)>> {
    let mut next: HashMap<u32, u32> = HashMap::new();
    let mut prev: HashMap<u32, u32> = HashMap::new();
    for &e in edges {
        let fs = topo.faces_of(e);
        if fs.len() != 1 {
            continue;
        }
        let face = &m.faces[fs[0]];
        let i = face.iter().position(|&v| v == e.0).unwrap_or(0);
        // the face runs a→b, so a new face on the open side runs b→a
        let (a, b) = if face[(i + 1) % face.len()] == e.1 { (e.0, e.1) } else { (e.1, e.0) };
        need(
            !next.contains_key(&b) && !prev.contains_key(&a),
            "the border edges branch: select simple loops or chains to bridge",
        )?;
        next.insert(b, a);
        prev.insert(a, b);
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut chains = Vec::new();
    // open chains start where nothing leads in
    let mut starts: Vec<u32> = next.keys().filter(|v| !prev.contains_key(v)).copied().collect();
    starts.sort_unstable();
    for s in starts {
        let mut c = vec![s];
        let mut cur = s;
        while let Some(&n) = next.get(&cur) {
            c.push(n);
            cur = n;
        }
        seen.extend(c.iter().copied());
        chains.push((c, false));
    }
    let mut rest: Vec<u32> = next.keys().filter(|v| !seen.contains(v)).copied().collect();
    rest.sort_unstable();
    for s in rest {
        if seen.contains(&s) {
            continue;
        }
        let mut c = vec![s];
        let mut cur = s;
        while let Some(&n) = next.get(&cur) {
            if n == s {
                break;
            }
            c.push(n);
            cur = n;
        }
        seen.extend(c.iter().copied());
        chains.push((c, true));
    }
    Ok(chains)
}

/// Join two border loops (or two open border chains) with a strip of faces (Blender's
/// "Bridge Edge Loops"). Selected faces are removed first and their borders bridged, so two
/// facing faces become a tunnel. Loops with different vertex counts are joined with
/// triangles. Selects the new faces.
pub fn bridge(m: &Mesh, sel: &Elements) -> Result<(Mesh, Elements)> {
    let fl = flush(m, sel)?;
    let mut work = m.clone();
    let mut edges = fl.edges.clone();
    if !fl.faces.is_empty() {
        let mut count: HashMap<Edge, u32> = HashMap::new();
        for &fi in &fl.faces {
            let f = &m.faces[fi as usize];
            for i in 0..f.len() {
                *count.entry(ek(f[i], f[(i + 1) % f.len()])).or_default() += 1;
            }
        }
        edges = count.into_iter().filter(|&(_, c)| c == 1).map(|(e, _)| e).collect();
        work.faces = m
            .faces
            .iter()
            .enumerate()
            .filter(|(i, _)| !fl.faces.contains(&(*i as u32)))
            .map(|(_, f)| f.clone())
            .collect();
    }
    let topo = Topology::new(&work);
    let chains = border_chains(&work, &topo, &edges)?;
    need(
        chains.len() == 2,
        &format!(
            "select two border edge loops (or two faces / regions) to bridge; the selection has {} border loop(s)",
            chains.len()
        ),
    )?;
    let (a, closed_a) = &chains[0];
    let (b, closed_b) = &chains[1];
    need(closed_a == closed_b, "bridge two closed loops or two open chains, not one of each")?;
    need(a.len() >= 2 && b.len() >= 2, "the loops are too short to bridge")?;
    let closed = *closed_a;
    let p = |v: u32| pos(&work, v);
    // b reordered so b2[k] sits across from a[k]; b runs the other way along the strip
    let b2: Vec<u32> = if closed {
        let n = b.len();
        let s = if a.len() == n && n <= 4096 {
            (0..n)
                .min_by(|&s1, &s2| {
                    let cost =
                        |s: usize| -> f32 { (0..n).map(|i| p(a[i]).distance_squared(p(b[(s + n - i) % n]))).sum() };
                    cost(s1).total_cmp(&cost(s2))
                })
                .unwrap_or(0)
        } else {
            (0..n)
                .min_by(|&x, &y| p(a[0]).distance_squared(p(b[x])).total_cmp(&p(a[0]).distance_squared(p(b[y]))))
                .unwrap_or(0)
        };
        (0..n).map(|i| b[(s + n - i) % n]).collect()
    } else {
        b.iter().rev().copied().collect()
    };
    let start = work.faces.len() as u32;
    let (na, nb) = (a.len(), b2.len());
    let at = |i: usize| a[i % na];
    let bt = |k: usize| b2[k % nb];
    // steps along each side: closed loops wrap round
    let (sa, sb) = if closed { (na, nb) } else { (na - 1, nb - 1) };
    if sa == sb {
        for i in 0..sa {
            work.faces.push(vec![at(i), at(i + 1), bt(i + 1), bt(i)]);
        }
    } else {
        let (mut i, mut k) = (0usize, 0usize);
        while i < sa || k < sb {
            let fa = (i + 1) as f32 / sa as f32;
            let fb = (k + 1) as f32 / sb as f32;
            if k >= sb || (i < sa && fa <= fb) {
                work.faces.push(vec![at(i), at(i + 1), bt(k)]);
                i += 1;
            } else {
                work.faces.push(vec![at(i), bt(k + 1), bt(k)]);
                k += 1;
            }
        }
    }
    let new: Vec<u32> = (start..work.faces.len() as u32).collect();
    let sel = face_selection(&work, new);
    Ok(finish(work, sel))
}

/// Where each vertex of the selected edges can slide: `[side A, side B]` targets along the
/// neighbouring edges that are not selected (Blender's edge slide rails). Side A is to the
/// left of the selected chains as they run.
pub fn slide_rails(m: &Mesh, sel: &Elements) -> Result<HashMap<u32, [Option<u32>; 2]>> {
    let topo = Topology::new(m);
    let fl = flush_with(m, &topo, sel)?;
    need(!fl.edges.is_empty(), "select edges (an edge loop) to slide")?;
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(u, v) in &fl.edges {
        adj.entry(u).or_default().push(v);
        adj.entry(v).or_default().push(u);
    }
    // walk the selected edges as chains so "left" stays on one side
    let mut directed: Vec<(u32, u32)> = Vec::new();
    let mut used: HashSet<Edge> = HashSet::new();
    let mut verts: Vec<u32> = adj.keys().copied().collect();
    verts.sort_unstable_by_key(|v| (adj[v].len() != 1, *v));
    for s in verts {
        let mut cur = s;
        while let Some(&n) = adj[&cur].iter().find(|&&n| !used.contains(&ek(cur, n))) {
            used.insert(ek(cur, n));
            directed.push((cur, n));
            cur = n;
        }
    }
    let mut rails: HashMap<u32, [Option<u32>; 2]> = HashMap::new();
    for (u, v) in directed {
        for &fi in topo.faces_of(ek(u, v)) {
            let f = &m.faces[fi];
            let n = f.len();
            let i = f.iter().position(|&x| x == u).unwrap_or(0);
            let side = if f[(i + 1) % n] == v { 0 } else { 1 };
            for (x, other) in [(u, v), (v, u)] {
                let j = f.iter().position(|&y| y == x).unwrap_or(0);
                let around = [f[(j + 1) % n], f[(j + n - 1) % n]];
                let Some(&t) = around.iter().find(|&&t| t != other) else { continue };
                if fl.verts.contains(&t) {
                    continue;
                }
                let r = rails.entry(x).or_default();
                if r[side].is_none() {
                    r[side] = Some(t);
                }
            }
        }
    }
    Ok(rails)
}

/// Slide the selected edges along their neighbouring edges (Blender's GG edge slide).
/// `factor` 0..1 moves toward side A (left of the loop), 0..-1 toward side B; 1 reaches the
/// next loop. The selection stays the same.
pub fn edge_slide(m: &Mesh, sel: &Elements, factor: f32) -> Result<(Mesh, Elements)> {
    need(factor.is_finite() && (-1.0..=1.0).contains(&factor), "factor must be between -1 and 1")?;
    let rails = slide_rails(m, sel)?;
    let side = if factor >= 0.0 { 0 } else { 1 };
    let t = factor.abs().min(0.999);
    let mut out = m.clone();
    let mut moved = 0;
    for (&v, r) in &rails {
        if let Some(target) = r[side] {
            let p = pos(m, v).lerp(pos(m, target), t);
            out.positions[v as usize] = p.to_array();
            moved += 1;
        }
    }
    need(moved > 0 || factor == 0.0, "nothing to slide along on that side")?;
    Ok((out, sel.clone().normalized()))
}

/// Which side of a bisect plane to remove.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ClearSide {
    /// Keep both halves.
    #[default]
    None,
    /// Remove what is on the side the normal points to.
    Front,
    /// Remove what is behind the plane.
    Back,
}

/// Cut faces along a plane (Blender's Bisect; the knife tool cuts with the plane through the
/// eye and the line you draw). Cuts the selected faces, or every face when nothing is given;
/// neighbours get the new vertices so the surface stays joined. Optionally removes one side.
/// Selects the new edges.
pub fn bisect(m: &Mesh, sel: &Elements, point: Vec3, normal: Vec3, clear: ClearSide) -> Result<(Mesh, Elements)> {
    need(
        point.is_finite() && normal.is_finite() && normal.length_squared() > 1e-12,
        "give a finite point and a non-zero normal",
    )?;
    let n = normal.normalize();
    let cut: BTreeSet<u32> = if sel.is_empty() { (0..m.faces.len() as u32).collect() } else { flush(m, sel)?.faces };
    need(!cut.is_empty(), "select faces to cut (or none for the whole mesh)")?;
    let size = m.positions.iter().map(|p| Vec3::from_array(*p).abs().max_element()).fold(1.0f32, f32::max);
    let eps = 1e-5 * size;
    let dist: Vec<f32> = m.positions.iter().map(|p| (Vec3::from_array(*p) - point).dot(n)).collect();
    let side = |v: u32| -> i8 {
        let d = dist[v as usize];
        if d > eps {
            1
        } else if d < -eps {
            -1
        } else {
            0
        }
    };
    let mut out = m.clone();
    let mut on: HashSet<u32> = (0..m.positions.len() as u32).filter(|&v| side(v) == 0).collect();
    let mut new_pts: HashMap<Edge, u32> = HashMap::new();
    for &fi in &cut {
        let f = &m.faces[fi as usize];
        for i in 0..f.len() {
            let (a, b) = (f[i], f[(i + 1) % f.len()]);
            if side(a) * side(b) < 0 {
                let k = ek(a, b);
                if let std::collections::hash_map::Entry::Vacant(slot) = new_pts.entry(k) {
                    let (da, db) = (dist[k.0 as usize], dist[k.1 as usize]);
                    let p = pos(m, k.0).lerp(pos(m, k.1), da / (da - db));
                    let v = push_vertex(&mut out, p);
                    on.insert(v);
                    slot.insert(v);
                }
            }
        }
    }
    let new_side = |out: &Mesh, f: &[u32]| -> f32 { (face_center(out, f) - point).dot(n) };
    let mut faces = Vec::with_capacity(m.faces.len() + cut.len());
    let mut from_cut = Vec::with_capacity(m.faces.len() + cut.len());
    let mut new_edges = Vec::new();
    for (fi, f) in m.faces.iter().enumerate() {
        let mut g = Vec::with_capacity(f.len() + 2);
        for i in 0..f.len() {
            let (a, b) = (f[i], f[(i + 1) % f.len()]);
            g.push(a);
            if let Some(&v) = new_pts.get(&ek(a, b)) {
                g.push(v);
            }
        }
        let is_cut = cut.contains(&(fi as u32));
        let picked: Vec<usize> = if is_cut { (0..g.len()).filter(|&i| on.contains(&g[i])).collect() } else { vec![] };
        let gl = g.len();
        if picked.len() == 2 && {
            let d = picked[1] - picked[0];
            d != 1 && d != gl - 1
        } {
            let (i, j) = (picked[0], picked[1]);
            let first = g[i..=j].to_vec();
            let mut second: Vec<u32> = g[j..].to_vec();
            second.extend_from_slice(&g[..=i]);
            new_edges.push([g[i], g[j]]);
            faces.push(first);
            faces.push(second);
            from_cut.extend([true, true]);
        } else {
            faces.push(g);
            from_cut.push(is_cut);
        }
    }
    need(!new_edges.is_empty() || clear != ClearSide::None, "the plane does not cross the faces")?;
    if clear != ClearSide::None {
        let sign = if clear == ClearSide::Front { 1.0 } else { -1.0 };
        let mut kept = Vec::with_capacity(faces.len());
        for (f, c) in faces.into_iter().zip(from_cut) {
            if !(c && new_side(&out, &f) * sign > eps) {
                kept.push(f);
            }
        }
        faces = kept;
        need(!faces.is_empty(), "that would remove the whole mesh")?;
    }
    out.faces = faces;
    Ok(finish(out, Elements::edges(new_edges)))
}

/// How the knife looks at the mesh: from an eye point (perspective) or along a direction
/// (orthographic). Each cut runs in the plane through two path points and the view ray.
#[derive(Clone, Copy, Debug)]
pub enum KnifeView {
    Eye(Vec3),
    Direction(Vec3),
}

impl KnifeView {
    fn ray(self, p: Vec3) -> Vec3 {
        match self {
            KnifeView::Eye(e) => (p - e).normalize_or_zero(),
            KnifeView::Direction(d) => d.normalize_or_zero(),
        }
    }
}

/// Whether `p` lies on face `f` (near its plane and inside its outline).
fn point_on_face(m: &Mesh, f: &[u32], p: Vec3, tol: f32) -> bool {
    let n = face_normal(m, f);
    if n == Vec3::ZERO || (p - pos(m, f[0])).dot(n).abs() > tol {
        return false;
    }
    let u = n.any_orthonormal_vector();
    let w = n.cross(u);
    let to2 = |q: Vec3| ((q - p).dot(u), (q - p).dot(w));
    let mut inside = false;
    for i in 0..f.len() {
        let (ax, ay) = to2(pos(m, f[i]));
        let (bx, by) = to2(pos(m, f[(i + 1) % f.len()]));
        if (ay > 0.0) != (by > 0.0) && 0.0 < ax + (0.0 - ay) / (by - ay) * (bx - ax) {
            inside = !inside;
        }
    }
    inside
}

/// Cut along a path of points on the surface, like Blender's knife: each step cuts the faces
/// between its two points (in the plane through them and the view ray), and a point inside a
/// face becomes a new vertex there, so the cut turns corners on the surface. Only `faces`
/// are cut (all when empty; the editor passes the visible ones). Selects the new edges.
pub fn knife(m: &Mesh, faces: &[u32], path: &[Vec3], view: KnifeView) -> Result<(Mesh, Elements)> {
    need(path.len() >= 2, "give at least two points for the knife path")?;
    need(path.len() <= 1000, "the knife path has too many points (limit 1000)")?;
    need(path.iter().all(|p| p.is_finite()), "knife points must be finite")?;
    let nf = m.faces.len() as u32;
    need(faces.iter().all(|&f| f < nf), "a face to cut does not exist")?;
    let limit: BTreeSet<u32> = if faces.is_empty() { (0..nf).collect() } else { faces.iter().copied().collect() };
    let size = m.positions.iter().map(|p| Vec3::from_array(*p).abs().max_element()).fold(1.0f32, f32::max);
    let eps = 1e-5 * size;
    let mut out = m.clone();
    // points made on edges: edge -> [(t from the edge's first vertex, vertex)]
    let mut on_edge: HashMap<Edge, Vec<(f32, u32)>> = HashMap::new();
    // per face, the path's events in order: (vertex, on the border?)
    let mut events: HashMap<u32, Vec<(u32, bool)>> = HashMap::new();
    let mut corner_vertex: HashMap<(usize, u32), u32> = HashMap::new();
    let corner = |out: &mut Mesh, cv: &mut HashMap<(usize, u32), u32>, i: usize, f: u32| -> u32 {
        *cv.entry((i, f)).or_insert_with(|| push_vertex(out, path[i]))
    };
    for i in 0..path.len() - 1 {
        let (a, b) = (path[i], path[i + 1]);
        if (b - a).length_squared() < eps * eps {
            continue;
        }
        let (ra, rb) = (view.ray(a), view.ray(b));
        let n = (b - a).cross(ra);
        if n.length_squared() < 1e-12 {
            continue;
        }
        let n = n.normalize();
        let mut sa = n.cross(ra);
        if (b - a).dot(sa) < 0.0 {
            sa = -sa;
        }
        let mut sb = n.cross(rb);
        if (a - b).dot(sb) < 0.0 {
            sb = -sb;
        }
        let within = |x: Vec3| (x - a).dot(sa) >= -eps && (x - b).dot(sb) >= -eps;
        let along = |x: Vec3| (x - a).dot(b - a);
        let dist = |v: u32| (pos(m, v) - a).dot(n);
        for &fi in &limit {
            let f = &m.faces[fi as usize];
            if i == 0 && point_on_face(m, f, a, eps * 100.0) {
                let v = corner(&mut out, &mut corner_vertex, 0, fi);
                events.entry(fi).or_default().push((v, false));
            }
            let mut hits: Vec<(f32, u32)> = Vec::new();
            for k in 0..f.len() {
                let (u, w) = (f[k], f[(k + 1) % f.len()]);
                let (du, dw) = (dist(u), dist(w));
                if du.abs() <= eps {
                    if within(pos(m, u)) {
                        hits.push((along(pos(m, u)), u));
                    }
                    continue;
                }
                if dw.abs() <= eps || (du > 0.0) == (dw > 0.0) {
                    continue;
                }
                let x = pos(m, u).lerp(pos(m, w), du / (du - dw));
                if !within(x) {
                    continue;
                }
                let key = ek(u, w);
                let t = (x - pos(m, key.0)).length() / (pos(m, key.1) - pos(m, key.0)).length().max(1e-12);
                let list = on_edge.entry(key).or_default();
                let v = match list.iter().find(|(s, _)| (s - t).abs() < 1e-6) {
                    Some(&(_, v)) => v,
                    None => {
                        let v = push_vertex(&mut out, x);
                        list.push((t, v));
                        v
                    }
                };
                hits.push((along(x), v));
            }
            hits.sort_by(|x, y| x.0.total_cmp(&y.0));
            hits.dedup_by_key(|h| h.1);
            let list = events.entry(fi).or_default();
            list.extend(hits.into_iter().map(|(_, v)| (v, true)));
            if point_on_face(m, f, b, eps * 100.0) {
                let v = corner(&mut out, &mut corner_vertex, i + 1, fi);
                list.push((v, false));
            }
        }
    }
    for list in on_edge.values_mut() {
        list.sort_by(|x, y| x.0.total_cmp(&y.0));
    }
    let mut faces_out: Vec<Vec<u32>> = Vec::with_capacity(m.faces.len() + 8);
    let mut new_edges: Vec<[u32; 2]> = Vec::new();
    for (fi, f) in m.faces.iter().enumerate() {
        let mut g = Vec::with_capacity(f.len() + 2);
        for k in 0..f.len() {
            let (u, w) = (f[k], f[(k + 1) % f.len()]);
            g.push(u);
            if let Some(list) = on_edge.get(&ek(u, w)) {
                if u < w {
                    g.extend(list.iter().map(|x| x.1));
                } else {
                    g.extend(list.iter().rev().map(|x| x.1));
                }
            }
        }
        let mut polys = vec![g];
        if let Some(ev) = events.get(&(fi as u32)) {
            // chains: border, inner..., border
            let mut chains: Vec<Vec<u32>> = Vec::new();
            let mut cur: Option<Vec<u32>> = None;
            for &(v, border) in ev {
                match (&mut cur, border) {
                    (None, true) => cur = Some(vec![v]),
                    (None, false) => {}
                    (Some(c), false) => c.push(v),
                    (Some(c), true) => {
                        if c.last() != Some(&v) {
                            c.push(v);
                            chains.push(std::mem::take(c));
                            cur = None;
                        }
                    }
                }
            }
            for chain in chains {
                let (s, e) = (chain[0], *chain.last().unwrap_or(&chain[0]));
                if s == e {
                    continue;
                }
                let inner = &chain[1..chain.len() - 1];
                let Some(pi) = polys.iter().position(|p| p.contains(&s) && p.contains(&e)) else { continue };
                let p = polys.swap_remove(pi);
                let n = p.len();
                let i = p.iter().position(|&x| x == s).unwrap_or(0);
                let j = p.iter().position(|&x| x == e).unwrap_or(0);
                if inner.is_empty() && ((i + 1) % n == j || (j + 1) % n == i) {
                    polys.push(p);
                    continue;
                }
                // s → … → e along the border, then back through the inner points
                let mut first = Vec::new();
                let mut k = i;
                loop {
                    first.push(p[k]);
                    if k == j {
                        break;
                    }
                    k = (k + 1) % n;
                }
                first.extend(inner.iter().rev());
                let mut second = Vec::new();
                let mut k = j;
                loop {
                    second.push(p[k]);
                    if k == i {
                        break;
                    }
                    k = (k + 1) % n;
                }
                second.extend(inner.iter());
                polys.push(first);
                polys.push(second);
                for w in chain.windows(2) {
                    new_edges.push([w[0], w[1]]);
                }
            }
        }
        faces_out.extend(polys);
    }
    need(!new_edges.is_empty(), "the knife path does not cross any face from edge to edge")?;
    out.faces = faces_out;
    Ok(finish(out, Elements::edges(new_edges)))
}

/// How proportional editing fades out with distance (Blender's falloff types).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Falloff {
    #[default]
    Smooth,
    Linear,
    Sharp,
    Sphere,
    Constant,
}

impl Falloff {
    /// Weight at `t` = distance / radius in 0..1.
    pub fn weight(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let s = 1.0 - t;
        match self {
            Falloff::Smooth => 3.0 * s * s - 2.0 * s * s * s,
            Falloff::Linear => s,
            Falloff::Sharp => s * s,
            Falloff::Sphere => (1.0 - t * t).max(0.0).sqrt(),
            Falloff::Constant => 1.0,
        }
    }
}

/// Distance from each vertex to the nearest selected vertex, for vertices within `radius`.
fn near_selection(m: &Mesh, sel: &BTreeSet<u32>, radius: f32) -> Vec<(u32, f32)> {
    let cell = radius.max(1e-6);
    let key = |p: Vec3| ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64);
    let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
    for &v in sel {
        grid.entry(key(pos(m, v))).or_default().push(v);
    }
    let mut out = Vec::new();
    for v in 0..m.positions.len() as u32 {
        if sel.contains(&v) {
            continue;
        }
        let p = pos(m, v);
        let (x, y, z) = key(p);
        let mut best = f32::MAX;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &s in grid.get(&(x + dx, y + dy, z + dz)).map(Vec::as_slice).unwrap_or(&[]) {
                        best = best.min(pos(m, s).distance(p));
                    }
                }
            }
        }
        if best < radius {
            out.push((v, best));
        }
    }
    out
}

/// [`transform`] with proportional editing: vertices within `radius` of the selection follow
/// it, less the farther they are (`falloff`).
#[allow(clippy::too_many_arguments)]
pub fn transform_proportional(
    m: &Mesh,
    sel: &Elements,
    translate: Vec3,
    rotate: Vec3,
    scale: Vec3,
    pivot: Option<Vec3>,
    radius: f32,
    falloff: Falloff,
) -> Result<(Mesh, Elements)> {
    need(radius.is_finite() && radius > 0.0 && radius <= 10_000.0, "radius must be between 0 and 10000")?;
    let f = flush(m, sel)?;
    need(!f.verts.is_empty(), "select some vertices, edges or faces to transform")?;
    let center = pivot.unwrap_or_else(|| selection_center(m, &f.verts));
    let q = Quat::from_euler(EulerRot::YXZ, rotate.y.to_radians(), rotate.x.to_radians(), rotate.z.to_radians());
    let mut out = m.clone();
    let apply = |p: Vec3, w: f32| -> Vec3 {
        let qw = Quat::IDENTITY.slerp(q, w);
        let sw = Vec3::ONE.lerp(scale, w);
        center + qw * ((p - center) * sw) + translate * w
    };
    for &v in &f.verts {
        out.positions[v as usize] = apply(pos(m, v), 1.0).to_array();
    }
    for (v, d) in near_selection(m, &f.verts, radius) {
        out.positions[v as usize] = apply(pos(m, v), falloff.weight(d / radius)).to_array();
    }
    Ok((out, sel.clone().normalized()))
}

/// Extrude the selection again and again while turning it about an axis (Blender's Spin):
/// edges sweep into a surface, faces into a curved solid. A full 360° turn joins the end to
/// the start. Selects the last copy.
pub fn spin(m: &Mesh, sel: &Elements, center: Vec3, axis: Vec3, angle: f32, steps: u32) -> Result<(Mesh, Elements)> {
    need((1..=256).contains(&steps), "steps must be 1..=256")?;
    need(angle.is_finite() && angle != 0.0 && angle.abs() <= 3600.0, "angle must be non-zero degrees, at most 3600")?;
    need(
        center.is_finite() && axis.is_finite() && axis.length_squared() > 1e-12,
        "give a finite centre and a non-zero axis",
    )?;
    let f = flush(m, sel)?;
    need(!f.edges.is_empty(), "select edges or faces to spin")?;
    let q = Quat::from_axis_angle(axis.normalize(), (angle / steps as f32).to_radians());
    let full = (angle.abs() - 360.0).abs() < 1e-3;
    let mut cur = m.clone();
    let mut s = sel.clone();
    for _ in 0..steps {
        let (next, ns) = extrude(&cur, &s, Offset { distance: 0.0, vector: Vec3::ZERO }, false)?;
        let moved = flush(&next, &ns)?.verts;
        cur = next;
        for v in moved {
            let p = pos(&cur, v) - center;
            cur.positions[v as usize] = (center + q * p).to_array();
        }
        s = ns;
    }
    if full {
        // the last copy sits where the first one started: drop its faces, weld the seam, and
        // drop any face pairs that now coincide
        let last = flush(&cur, &s)?.faces;
        cur.faces =
            cur.faces.into_iter().enumerate().filter(|(i, _)| !last.contains(&(*i as u32))).map(|(_, f)| f).collect();
        let (welded, _, _) = merge_by_distance(&cur, &Elements::default(), 1e-4)?;
        let mut count: HashMap<Vec<u32>, usize> = HashMap::new();
        for f in &welded.faces {
            let mut k = f.clone();
            k.sort_unstable();
            *count.entry(k).or_default() += 1;
        }
        let faces = welded
            .faces
            .iter()
            .filter(|f| {
                let mut k = (*f).clone();
                k.sort_unstable();
                count[&k] == 1
            })
            .cloned()
            .collect();
        let out = Mesh { positions: welded.positions, faces };
        return Ok(finish(out, Elements::default()));
    }
    Ok(finish(cur, s))
}

/// Every edge of the mesh as `[a, b]` (a < b), in a stable order.
pub fn all_edges(m: &Mesh) -> Vec<[u32; 2]> {
    Topology::new(m).edges.into_iter().map(|(a, b)| [a, b]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Primitive;

    fn cube() -> Mesh {
        Primitive::Cube { size: [2.0; 3] }.build().unwrap()
    }
    fn grid(n: u32) -> Mesh {
        let mut m = Mesh::default();
        for z in 0..=n {
            for x in 0..=n {
                m.positions.push([x as f32, 0.0, -(z as f32)]);
            }
        }
        let id = |x: u32, z: u32| z * (n + 1) + x;
        for z in 0..n {
            for x in 0..n {
                m.faces.push(vec![id(x, z), id(x + 1, z), id(x + 1, z + 1), id(x, z + 1)]);
            }
        }
        m
    }
    /// Every edge is used by exactly two faces in opposite directions.
    fn closed(m: &Mesh) -> bool {
        let mut half: HashMap<(u32, u32), u32> = HashMap::new();
        for f in &m.faces {
            for i in 0..f.len() {
                *half.entry((f[i], f[(i + 1) % f.len()])).or_default() += 1;
            }
        }
        half.iter().all(|(&(a, b), &c)| c == 1 && half.get(&(b, a)) == Some(&1))
    }
    fn euler(m: &Mesh) -> i64 {
        m.positions.len() as i64 - m.edge_count() as i64 + m.faces.len() as i64
    }

    fn face_facing(m: &Mesh, dir: Vec3) -> u32 {
        (0..m.faces.len() as u32).find(|&f| face_normal(m, &m.faces[f as usize]).dot(dir) > 0.99).unwrap()
    }
    fn two_cubes() -> Mesh {
        let mut m = cube();
        let c = cube();
        let off = m.positions.len() as u32;
        m.positions.extend(c.positions.iter().map(|p| [p[0] + 4.0, p[1], p[2]]));
        m.faces.extend(c.faces.iter().map(|f| f.iter().map(|v| v + off).collect::<Vec<_>>()));
        m
    }

    #[test]
    fn bridge_two_faces_into_a_tube() {
        let m = two_cubes();
        let a = face_facing(&m, Vec3::X);
        let b = (0..m.faces.len() as u32)
            .filter(|&f| face_normal(&m, &m.faces[f as usize]).dot(-Vec3::X) > 0.99)
            .find(|&f| face_center(&m, &m.faces[f as usize]).x > 2.0)
            .unwrap();
        let (out, sel) = bridge(&m, &Elements::faces([a, b])).unwrap();
        assert_eq!(out.faces.len(), 14);
        assert_eq!(sel.faces.len(), 4);
        assert!(closed(&out));
        assert_eq!(euler(&out), 2);
        // the bridge runs straight across: every new face spans the gap in x
        for &f in &sel.faces {
            let xs: Vec<f32> = out.faces[f as usize].iter().map(|&v| out.positions[v as usize][0]).collect();
            assert!(xs.iter().any(|&x| (x - 1.0).abs() < 1e-5) && xs.iter().any(|&x| (x - 3.0).abs() < 1e-5));
        }
    }

    #[test]
    fn bridge_loops_of_different_sizes() {
        let mut m = two_cubes();
        let b = (0..m.faces.len() as u32)
            .filter(|&f| face_normal(&m, &m.faces[f as usize]).dot(-Vec3::X) > 0.99)
            .find(|&f| face_center(&m, &m.faces[f as usize]).x > 2.0)
            .unwrap();
        // give the second cube's facing side an extra vertex
        let (m2, s2) = subdivide(&m, &Elements::faces([b])).unwrap();
        m = m2;
        let a = face_facing(&m, Vec3::X);
        let mut sel = Elements::faces([a]);
        sel.faces.extend(s2.faces);
        let (out, _) = bridge(&m, &sel).unwrap();
        assert!(closed(&out));
        assert_eq!(euler(&out), 2);
        assert!(bridge(&cube(), &Elements::faces([0])).is_err());
    }

    #[test]
    fn edge_slide_moves_a_cut_loop() {
        let (m, sel) = loop_cut(&cube(), [0, 4], 1, 0.5).unwrap();
        let rails = slide_rails(&m, &sel).unwrap();
        assert_eq!(rails.len(), 4);
        assert!(rails.values().all(|r| r[0].is_some() && r[1].is_some()));
        let (a, _) = edge_slide(&m, &sel, 0.5).unwrap();
        let (b, _) = edge_slide(&m, &sel, -0.5).unwrap();
        assert!(closed(&a));
        let v = sel.edges[0][0] as usize;
        let (pa, pb, p0) =
            (Vec3::from_array(a.positions[v]), Vec3::from_array(b.positions[v]), Vec3::from_array(m.positions[v]));
        // opposite sides, half way to the cube's edge (1 m away)
        assert!((pa - p0).length() > 0.45 && (pb - p0).length() > 0.45);
        assert!((pa - pb).length() > 0.95);
        assert!(edge_slide(&m, &sel, 2.0).is_err());
    }

    #[test]
    fn bisect_a_cube() {
        let m = cube();
        let (out, sel) = bisect(&m, &Elements::default(), Vec3::ZERO, Vec3::Y, ClearSide::None).unwrap();
        assert_eq!(out.faces.len(), 10);
        assert_eq!(sel.edges.len(), 4);
        assert!(closed(&out));
        let (half, _) = bisect(&m, &Elements::default(), Vec3::ZERO, Vec3::Y, ClearSide::Front).unwrap();
        assert_eq!(half.faces.len(), 5);
        assert!(half.positions.iter().all(|p| p[1] <= 1e-5));
        // only the selected face: neighbours get the new vertices, stay joined
        let top = face_facing(&m, Vec3::Y);
        let (one, sel) = bisect(&m, &Elements::faces([top]), Vec3::ZERO, Vec3::X, ClearSide::None).unwrap();
        assert_eq!(one.faces.len(), 7);
        assert_eq!(sel.edges.len(), 1);
        assert!(closed(&one));
        assert!(bisect(&m, &Elements::default(), Vec3::new(0.0, 5.0, 0.0), Vec3::Y, ClearSide::None).is_err());
    }

    #[test]
    fn knife_path_turns_on_a_face() {
        let m = cube(); // -1..1
        let top = face_facing(&m, Vec3::Y);
        let eye = KnifeView::Direction(-Vec3::Y);
        // straight across the top: splits it, neighbours get the end points
        let path = [Vec3::new(-1.5, 1.0, 0.0), Vec3::new(1.5, 1.0, 0.0)];
        let (a, sel) = knife(&m, &[top], &path, eye).unwrap();
        assert_eq!(a.faces.len(), 7);
        assert_eq!(sel.edges.len(), 1);
        assert!(closed(&a));
        // an L-shaped cut: in from -x, turn in the middle, out through +z
        let path = [Vec3::new(-1.5, 1.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 1.0, 1.5)];
        let (b, sel) = knife(&m, &[top], &path, eye).unwrap();
        assert_eq!(b.faces.len(), 7);
        assert_eq!(sel.edges.len(), 2);
        assert_eq!(b.positions.len(), 11);
        assert!(closed(&b));
        // a short stroke that stays inside one face cuts nothing
        let path = [Vec3::new(-0.2, 1.0, 0.0), Vec3::new(0.2, 1.0, 0.0)];
        assert!(knife(&m, &[top], &path, eye).is_err());
        // the cut stops at the path's ends: a path ending inside the top only cuts up to there
        let path = [
            Vec3::new(-1.5, 1.0, -0.5),
            Vec3::new(-0.5, 1.0, 0.5),
            Vec3::new(0.5, 1.0, -0.5),
            Vec3::new(1.5, 1.0, 0.5),
        ];
        let (c, sel) = knife(&m, &[top], &path, eye).unwrap();
        assert!(closed(&c));
        assert_eq!(sel.edges.len(), 3);
    }

    #[test]
    fn proportional_move_fades_out() {
        let m = grid(10);
        let mid = 5 * 11 + 5;
        let (out, _) = transform_proportional(
            &m,
            &Elements::vertices([mid]),
            Vec3::Y,
            Vec3::ZERO,
            Vec3::ONE,
            None,
            3.0,
            Falloff::Smooth,
        )
        .unwrap();
        let y = |v: u32| out.positions[v as usize][1];
        assert!((y(mid) - 1.0).abs() < 1e-6);
        assert!(y(mid + 1) > 0.5 && y(mid + 1) < 1.0);
        assert!(y(mid + 2) > 0.0 && y(mid + 2) < y(mid + 1));
        assert_eq!(y(mid + 3), 0.0);
        assert_eq!(y(0), 0.0);
    }

    #[test]
    fn spin_a_border_into_a_ring() {
        // a square plane at x 1..2 standing in the XY plane, spun round Y: a tube wall
        let m = Mesh {
            positions: vec![[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
            faces: vec![vec![0, 1, 2, 3]],
        };
        let (half, sel) = spin(&m, &Elements::faces([0]), Vec3::ZERO, Vec3::Y, 180.0, 6).unwrap();
        assert_eq!(sel.faces.len(), 1);
        // like Blender, extruding a lone face leaves its start open
        assert_eq!(half.faces.len(), 1 + 6 * 4);
        let (ring, _) = spin(&m, &Elements::faces([0]), Vec3::ZERO, Vec3::Y, 360.0, 8).unwrap();
        assert_eq!(ring.faces.len(), 8 * 4);
        assert_eq!(ring.positions.len(), 8 * 4);
        assert!(closed(&ring));
        // a face on a closed box spun a quarter turn stays watertight
        let (bent, _) = spin(
            &cube(),
            &Elements::faces([face_facing(&cube(), Vec3::X)]),
            Vec3::new(1.0, 0.0, 2.0),
            Vec3::Y,
            90.0,
            4,
        )
        .unwrap();
        assert!(closed(&bent));
        assert!(spin(&m, &Elements::default(), Vec3::ZERO, Vec3::Y, 90.0, 4).is_err());
    }

    #[test]
    fn extrude_top_face_of_cube() {
        let m = cube();
        // face 4 is +Y
        let (out, sel) =
            extrude(&m, &Elements::faces([4]), Offset { distance: 1.0, ..Default::default() }, false).unwrap();
        out.validate().unwrap();
        assert!(closed(&out));
        assert_eq!((out.positions.len(), out.faces.len()), (12, 10));
        assert_eq!(sel.faces.len(), 1);
        let top = &out.faces[sel.faces[0] as usize];
        assert!(top.iter().all(|&v| (out.positions[v as usize][1] - 2.0).abs() < 1e-5));
        assert!(face_normal(&out, top).y > 0.99);
        assert_eq!(euler(&out), 2);
    }

    #[test]
    fn extrude_region_and_individual() {
        let m = cube();
        let (r, _) =
            extrude(&m, &Elements::faces([0, 2]), Offset { distance: 0.5, ..Default::default() }, false).unwrap();
        assert!(closed(&r));
        let (i, sel) =
            extrude(&m, &Elements::faces([0, 2]), Offset { distance: 0.5, ..Default::default() }, true).unwrap();
        assert!(closed(&i));
        assert!(i.faces.len() > r.faces.len());
        assert_eq!(sel.faces.len(), 2);
    }

    #[test]
    fn extrude_border_edge_of_plane() {
        let m = grid(1);
        let (out, sel) =
            extrude(&m, &Elements::edges([[0, 1]]), Offset { distance: 1.0, ..Default::default() }, false).unwrap();
        assert_eq!(out.faces.len(), 2);
        assert_eq!(sel.edges.len(), 1);
        // the new edge moved outward (+Z, away from the face)
        let e = sel.edges[0];
        assert!(out.positions[e[0] as usize][2] > 0.9);
        assert!(face_normal(&out, &out.faces[1]).y > 0.99, "continues the surface");
    }

    #[test]
    fn inset_face() {
        let m = cube();
        let (out, sel) = inset(&m, &Elements::faces([4]), 0.25, 0.0, false).unwrap();
        assert!(closed(&out));
        assert_eq!(out.faces.len(), 10);
        let top = &out.faces[sel.faces[0] as usize];
        for &v in top {
            let p = out.positions[v as usize];
            assert!((p[0].abs() - 0.75).abs() < 1e-4 && (p[2].abs() - 0.75).abs() < 1e-4, "{p:?}");
        }
    }

    #[test]
    fn bevel_one_cube_edge() {
        let m = cube();
        // top front edge: +Z face [4,5,6,7] and +Y face [7,6,2,3] share 6-7
        let (out, sel) = bevel(&m, &Elements::edges([[6, 7]]), 0.2, 1).unwrap();
        out.validate().unwrap();
        assert!(closed(&out), "{out:?}");
        assert_eq!(out.faces.len(), 7);
        assert_eq!(out.positions.len(), 10);
        assert_eq!(euler(&out), 2);
        assert_eq!(sel.faces.len(), 1);
        let n = face_normal(&out, &out.faces[sel.faces[0] as usize]);
        assert!((n - Vec3::new(0.0, 1.0, 1.0).normalize()).length() < 1e-4, "{n}");
    }

    #[test]
    fn bevel_all_cube_edges() {
        let m = cube();
        let all = all_edges(&m);
        let (out, _) = bevel(&m, &Elements::edges(all), 0.2, 1).unwrap();
        assert!(closed(&out));
        // 6 faces + 12 strips + 8 corner triangles
        assert_eq!(out.faces.len(), 26);
        assert_eq!(out.positions.len(), 24);
        let (round, _) = bevel(&m, &Elements::edges([[6, 7]]), 0.3, 3).unwrap();
        round.validate().unwrap();
        assert!(closed(&round), "{round:?}");
        assert_eq!(round.faces.len(), 6 + 3);
        assert_eq!(euler(&round), 2);
    }

    #[test]
    fn bevel_edge_through_grid() {
        // a chain through the middle of a 2x2 grid ends at border vertices
        let m = grid(2);
        let (out, _) = bevel(&m, &Elements::edges([[1, 4], [4, 7]]), 0.2, 1).unwrap();
        out.validate().unwrap();
        assert_eq!(euler(&out), 1);
    }

    #[test]
    fn loop_cut_cube_and_cylinder() {
        let m = cube();
        let (out, sel) = loop_cut(&m, [4, 5], 1, 0.5).unwrap();
        assert!(closed(&out));
        assert_eq!(out.faces.len(), 10);
        assert_eq!(out.positions.len(), 12);
        assert_eq!(sel.edges.len(), 4);
        let (two, sel) = loop_cut(&m, [4, 5], 2, 0.5).unwrap();
        assert!(closed(&two));
        assert_eq!(sel.edges.len(), 8);
        // vertical side edge of a cylinder: ring around the sides, caps untouched
        let cyl = Primitive::Cylinder { radius: 1.0, height: 2.0, segments: 8 }.build().unwrap();
        let (c, sel) = loop_cut(&cyl, [0, 8], 1, 0.5).unwrap();
        assert!(closed(&c));
        assert_eq!(sel.edges.len(), 8);
        assert_eq!(c.faces.len(), cyl.faces.len() + 8);
        // horizontal edge: ring ends at the caps, which gain the new points
        let (c2, _) = loop_cut(&cyl, [0, 1], 1, 0.5).unwrap();
        assert!(closed(&c2));
    }

    #[test]
    fn loops_rings_and_linked() {
        let m = grid(3);
        let topo = Topology::new(&m);
        // row edges along z = 1 (vertices 4..8)
        let l = edge_loop(&m, &topo, (5, 6)).unwrap();
        assert_eq!(l.len(), 3, "{l:?}");
        let r = edge_ring(&m, &topo, (5, 6)).unwrap();
        assert_eq!(r.faces.len(), 3);
        let c = cube();
        let lk = linked(&c, &Elements::vertices([0])).unwrap();
        assert_eq!(lk.faces.len(), 6);
        let g = grow(&m, &Elements::faces([4])).unwrap();
        assert_eq!(g.faces.len(), 9);
    }

    #[test]
    fn subdivide_connect_merge_delete_fill_flip() {
        let m = cube();
        let (s, sel) = subdivide(&m, &Elements::faces([4])).unwrap();
        assert!(closed(&s));
        assert_eq!(sel.faces.len(), 4);
        let (c, sel) = connect(&m, &Elements::vertices([4, 6])).unwrap();
        assert!(closed(&c));
        assert_eq!(c.faces.len(), 7);
        assert_eq!(sel.edges, vec![[4, 6]]);
        let (mg, _) = merge(&m, &Elements::vertices([6, 7]), MergeAt::Center).unwrap();
        assert!(closed(&mg));
        assert_eq!(mg.positions.len(), 7);
        let (d, _) = delete(&m, &Elements::faces([4]), None).unwrap();
        assert_eq!(d.faces.len(), 5);
        assert!(!closed(&d));
        let (border, _) = (all_edges(&d), ());
        let open: Vec<[u32; 2]> =
            border.into_iter().filter(|e| Topology::new(&d).faces_of((e[0], e[1])).len() == 1).collect();
        let (filled, sel) = fill(&d, &Elements::edges(open)).unwrap();
        assert!(closed(&filled));
        assert_eq!(sel.faces.len(), 1);
        let (dv, _) = delete(&m, &Elements::vertices([0]), None).unwrap();
        assert_eq!((dv.faces.len(), dv.positions.len()), (3, 7));
        let (f, _) = flip(&m, &Elements::default()).unwrap();
        assert!(face_normal(&f, &f.faces[4]).y < -0.99);
        let doubled = {
            let mut d = m.clone();
            d.append(&m);
            d
        };
        let (md, _, removed) = merge_by_distance(&doubled, &Elements::default(), 1e-4).unwrap();
        assert_eq!(removed, 8);
        assert_eq!(md.positions.len(), 8);
    }

    #[test]
    fn transform_moves_only_selection() {
        let m = cube();
        let (t, _) = transform(&m, &Elements::faces([4]), Vec3::Y, Vec3::ZERO, Vec3::ONE, None).unwrap();
        assert_eq!(t.bounds().unwrap().1.y, 2.0);
        assert_eq!(t.bounds().unwrap().0.y, -1.0);
        let (s, _) = transform(&m, &Elements::faces([4]), Vec3::ZERO, Vec3::ZERO, Vec3::splat(0.5), None).unwrap();
        assert_eq!(s.bounds().unwrap().1.x, 1.0); // bottom stays
        assert!(flush(&m, &Elements::vertices([99])).is_err());
        assert!(flush(&m, &Elements::edges([[0, 6]])).is_err());
    }
}
