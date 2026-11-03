//! Editable polygon meshes, parametric primitives and a non-destructive modifier stack.
//!
//! A [`Mesh`] stores n-gon faces (like Blender), so later edit-mode tools (extrude, bevel,
//! loop cut) can work on quads. Rendering triangulates through [`Mesh::triangulate`].

use std::collections::HashMap;
use std::f32::consts::TAU;

use glam::{Mat4, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

/// Polygon mesh: shared vertex positions and faces as vertex-index loops (counter-clockwise
/// seen from outside).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub faces: Vec<Vec<u32>>,
}

/// Parametric primitives. Sizes in metres; the primitive is centred on its origin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Primitive {
    Cube {
        size: [f32; 3],
    },
    Plane {
        size: [f32; 2],
    },
    Sphere {
        radius: f32,
        segments: u32,
        rings: u32,
    },
    Cylinder {
        radius: f32,
        height: f32,
        segments: u32,
    },
    Cone {
        radius: f32,
        height: f32,
        segments: u32,
    },
    Torus {
        major_radius: f32,
        minor_radius: f32,
        major_segments: u32,
        minor_segments: u32,
    },
    /// A cylinder with rounded ends along Y; `height` is the total length (at least 2 × radius).
    Capsule {
        radius: f32,
        height: f32,
        segments: u32,
    },
    /// Seamless photo sweep ("infinity cove"): a floor `size.z` deep that curves up with
    /// `radius` into a back wall `size.y` high (at -Z), `size.x` wide. Faces the inside.
    Cove {
        size: [f32; 3],
        radius: f32,
    },
}

/// Where an entity's base geometry comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "from", rename_all = "camelCase")]
pub enum MeshSource {
    /// Re-generated from parameters, so its size and segment counts stay editable.
    Primitive { primitive: Primitive },
    /// Explicit geometry (imported, applied modifiers, merged, or edited).
    Data { mesh: Mesh },
    /// Imported geometry bound to a skeleton: `mesh` is the rest pose, and every vertex follows
    /// up to four joint entities (see [`crate::eval::posed_mesh`]).
    Skinned { mesh: Mesh, skin: Skin },
}

/// How a skinned mesh follows its joints (glTF skinning).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Skin {
    /// Joint entity ids.
    pub joints: Vec<String>,
    /// One column-major inverse bind matrix per joint.
    pub inverse_bind: Vec<[f32; 16]>,
    /// Per vertex: four indices into `joints`.
    pub vertex_joints: Vec<[u16; 4]>,
    /// Per vertex: the four weights (they are normalised when used).
    pub vertex_weights: Vec<[f32; 4]>,
}

impl Skin {
    pub fn validate(&self, vertices: usize) -> Result<()> {
        if self.joints.is_empty() || self.joints.len() != self.inverse_bind.len() {
            return Err(CoreError::Invalid("a skin needs one inverse bind matrix per joint".into()));
        }
        if self.vertex_joints.len() != vertices || self.vertex_weights.len() != vertices {
            return Err(CoreError::Invalid("a skin needs joints and weights for every vertex".into()));
        }
        let n = self.joints.len() as u16;
        if self.vertex_joints.iter().flatten().any(|j| *j >= n) {
            return Err(CoreError::Invalid("a skin weight refers to a joint that does not exist".into()));
        }
        if self.inverse_bind.iter().flatten().chain(self.vertex_weights.iter().flatten()).any(|v| !v.is_finite()) {
            return Err(CoreError::Invalid("skin data must be finite".into()));
        }
        Ok(())
    }
}

/// Non-destructive modifiers, evaluated top to bottom.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Modifier {
    /// Mirror across the local plane through the origin whose normal is `axis` (0=X, 1=Y, 2=Z).
    Mirror { axis: u8 },
    /// `count` copies, each shifted by `offset` metres from the previous one.
    Array { count: u32, offset: [f32; 3] },
    /// Catmull-Clark subdivision when `smooth`, otherwise linear (shape kept, more faces).
    Subdivide { levels: u32, smooth: bool },
}

/// Triangle mesh ready for rendering or export.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

const MAX_SEGMENTS: u32 = 256;
const MAX_FACES: usize = 2_000_000;

fn check_size(v: f32, what: &str) -> Result<f32> {
    if v.is_finite() && v > 0.0 && v <= 10_000.0 {
        Ok(v)
    } else {
        Err(CoreError::Invalid(format!("{what} must be in (0, 10000], got {v}")))
    }
}

fn check_segments(v: u32, min: u32, what: &str) -> Result<u32> {
    if (min..=MAX_SEGMENTS).contains(&v) {
        Ok(v)
    } else {
        Err(CoreError::Invalid(format!("{what} must be in {min}..={MAX_SEGMENTS}, got {v}")))
    }
}

impl Primitive {
    pub fn build(&self) -> Result<Mesh> {
        match *self {
            Primitive::Cube { size } => {
                let [x, y, z] = [
                    check_size(size[0], "size.x")? / 2.0,
                    check_size(size[1], "size.y")? / 2.0,
                    check_size(size[2], "size.z")? / 2.0,
                ];
                let positions = vec![
                    [-x, -y, -z],
                    [x, -y, -z],
                    [x, y, -z],
                    [-x, y, -z],
                    [-x, -y, z],
                    [x, -y, z],
                    [x, y, z],
                    [-x, y, z],
                ];
                let faces = vec![
                    vec![4, 5, 6, 7], // +Z
                    vec![1, 0, 3, 2], // -Z
                    vec![5, 1, 2, 6], // +X
                    vec![0, 4, 7, 3], // -X
                    vec![7, 6, 2, 3], // +Y
                    vec![0, 1, 5, 4], // -Y
                ];
                Ok(Mesh { positions, faces })
            }
            Primitive::Plane { size } => {
                let [x, z] = [check_size(size[0], "size.x")? / 2.0, check_size(size[1], "size.y")? / 2.0];
                Ok(Mesh {
                    positions: vec![[-x, 0.0, z], [x, 0.0, z], [x, 0.0, -z], [-x, 0.0, -z]],
                    faces: vec![vec![0, 1, 2, 3]],
                })
            }
            Primitive::Sphere { radius, segments, rings } => {
                let r = check_size(radius, "radius")?;
                let seg = check_segments(segments, 3, "segments")?;
                let rings = check_segments(rings, 2, "rings")?;
                let mut positions = vec![[0.0, r, 0.0]];
                for i in 1..rings {
                    let phi = std::f32::consts::PI * i as f32 / rings as f32;
                    for j in 0..seg {
                        let th = TAU * j as f32 / seg as f32;
                        positions.push([r * phi.sin() * th.cos(), r * phi.cos(), -r * phi.sin() * th.sin()]);
                    }
                }
                positions.push([0.0, -r, 0.0]);
                let bottom = positions.len() as u32 - 1;
                let ring = |i: u32, j: u32| 1 + i * seg + (j % seg);
                let mut faces = Vec::new();
                for j in 0..seg {
                    faces.push(vec![0, ring(0, j), ring(0, j + 1)]);
                }
                for i in 0..rings - 2 {
                    for j in 0..seg {
                        faces.push(vec![ring(i, j), ring(i + 1, j), ring(i + 1, j + 1), ring(i, j + 1)]);
                    }
                }
                for j in 0..seg {
                    faces.push(vec![ring(rings - 2, j + 1), ring(rings - 2, j), bottom]);
                }
                Ok(Mesh { positions, faces })
            }
            Primitive::Cylinder { radius, height, segments } | Primitive::Cone { radius, height, segments } => {
                let cone = matches!(self, Primitive::Cone { .. });
                let r = check_size(radius, "radius")?;
                let h = check_size(height, "height")? / 2.0;
                let seg = check_segments(segments, 3, "segments")?;
                let mut positions = Vec::new();
                for j in 0..seg {
                    let th = TAU * j as f32 / seg as f32;
                    positions.push([r * th.cos(), -h, -r * th.sin()]);
                }
                let mut faces = Vec::new();
                if cone {
                    positions.push([0.0, h, 0.0]);
                    for j in 0..seg {
                        faces.push(vec![j, (j + 1) % seg, seg]);
                    }
                } else {
                    for j in 0..seg {
                        let th = TAU * j as f32 / seg as f32;
                        positions.push([r * th.cos(), h, -r * th.sin()]);
                    }
                    for j in 0..seg {
                        let n = (j + 1) % seg;
                        faces.push(vec![j, n, seg + n, seg + j]);
                    }
                    faces.push((0..seg).map(|j| seg + j).collect());
                }
                faces.push((0..seg).rev().collect());
                Ok(Mesh { positions, faces })
            }
            Primitive::Cove { size, radius } => {
                let [w, h, d] =
                    [check_size(size[0], "size.x")?, check_size(size[1], "size.y")?, check_size(size[2], "size.z")?];
                let r = check_size(radius, "radius")?.min(h * 0.99).min(d * 0.99);
                // profile (y, z) from the front edge of the floor, round the bend, up the wall
                let mut prof = vec![(0.0, d / 2.0)];
                let n = 12;
                for i in 0..=n {
                    let a = std::f32::consts::FRAC_PI_2 * i as f32 / n as f32;
                    prof.push((r - r * a.cos(), -d / 2.0 + r - r * a.sin()));
                }
                prof.push((h, -d / 2.0));
                let mut positions = Vec::new();
                for (y, z) in &prof {
                    positions.push([-w / 2.0, y - h / 2.0, *z]);
                    positions.push([w / 2.0, y - h / 2.0, *z]);
                }
                let faces = (0..prof.len() as u32 - 1).map(|i| vec![2 * i, 2 * i + 1, 2 * i + 3, 2 * i + 2]).collect();
                Ok(Mesh { positions, faces })
            }
            Primitive::Capsule { radius, height, segments } => {
                let r = check_size(radius, "radius")?;
                let half = (check_size(height, "height")? / 2.0 - r).max(0.0);
                let seg = check_segments(segments, 3, "segments")?;
                let cap = (seg / 4).max(2);
                // rings from the top pole to the bottom pole: hemisphere, (straight part), hemisphere
                let mut rings: Vec<(f32, f32)> = Vec::new();
                for i in 1..=cap {
                    let a = std::f32::consts::FRAC_PI_2 * i as f32 / cap as f32;
                    rings.push((r * a.sin(), half + r * a.cos()));
                }
                // a capsule no longer than its width is a sphere: skip the duplicate equator
                for i in (if half > 0.0 { 0 } else { 1 })..cap {
                    let a = std::f32::consts::FRAC_PI_2 * i as f32 / cap as f32;
                    rings.push((r * a.cos(), -half - r * a.sin()));
                }
                let mut positions = vec![[0.0, half + r, 0.0]];
                for (rr, y) in &rings {
                    for j in 0..seg {
                        let th = TAU * j as f32 / seg as f32;
                        positions.push([rr * th.cos(), *y, -rr * th.sin()]);
                    }
                }
                positions.push([0.0, -half - r, 0.0]);
                let bottom = positions.len() as u32 - 1;
                let n = rings.len() as u32;
                let ring = |i: u32, j: u32| 1 + i * seg + (j % seg);
                let mut faces = Vec::new();
                for j in 0..seg {
                    faces.push(vec![0, ring(0, j), ring(0, j + 1)]);
                }
                for i in 0..n - 1 {
                    for j in 0..seg {
                        faces.push(vec![ring(i, j), ring(i + 1, j), ring(i + 1, j + 1), ring(i, j + 1)]);
                    }
                }
                for j in 0..seg {
                    faces.push(vec![ring(n - 1, j + 1), ring(n - 1, j), bottom]);
                }
                Ok(Mesh { positions, faces })
            }
            Primitive::Torus { major_radius, minor_radius, major_segments, minor_segments } => {
                let big = check_size(major_radius, "majorRadius")?;
                let small = check_size(minor_radius, "minorRadius")?;
                let ms = check_segments(major_segments, 3, "majorSegments")?;
                let ns = check_segments(minor_segments, 3, "minorSegments")?;
                let mut positions = Vec::new();
                for i in 0..ms {
                    let u = TAU * i as f32 / ms as f32;
                    for j in 0..ns {
                        let v = TAU * j as f32 / ns as f32;
                        let rr = big + small * v.cos();
                        positions.push([rr * u.cos(), small * v.sin(), -rr * u.sin()]);
                    }
                }
                let idx = |i: u32, j: u32| (i % ms) * ns + (j % ns);
                let mut faces = Vec::new();
                for i in 0..ms {
                    for j in 0..ns {
                        faces.push(vec![idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1)]);
                    }
                }
                Ok(Mesh { positions, faces })
            }
        }
    }
}

impl MeshSource {
    pub fn base_mesh(&self) -> Result<Mesh> {
        match self {
            MeshSource::Primitive { primitive } => primitive.build(),
            MeshSource::Data { mesh } => {
                mesh.validate()?;
                Ok(mesh.clone())
            }
            MeshSource::Skinned { mesh, skin } => {
                mesh.validate()?;
                skin.validate(mesh.positions.len())?;
                Ok(mesh.clone())
            }
        }
    }
}

impl Modifier {
    pub fn validate(&self) -> Result<()> {
        match *self {
            Modifier::Mirror { axis } if axis > 2 => {
                Err(CoreError::Invalid(format!("mirror axis must be 0, 1 or 2, got {axis}")))
            }
            Modifier::Array { count, offset } => {
                if !(1..=1000).contains(&count) {
                    return Err(CoreError::Invalid(format!("array count must be 1..=1000, got {count}")));
                }
                if offset.iter().any(|v| !v.is_finite()) {
                    return Err(CoreError::Invalid("array offset must be finite".into()));
                }
                Ok(())
            }
            Modifier::Subdivide { levels, .. } if levels > 4 => {
                Err(CoreError::Invalid(format!("subdivide levels must be 0..=4, got {levels}")))
            }
            _ => Ok(()),
        }
    }

    pub fn apply(&self, mesh: &Mesh) -> Result<Mesh> {
        self.validate()?;
        let out = match *self {
            Modifier::Mirror { axis } => {
                let mut m = mesh.clone();
                let mut flipped = mesh.clone();
                for p in &mut flipped.positions {
                    p[axis as usize] = -p[axis as usize];
                }
                for f in &mut flipped.faces {
                    f.reverse();
                }
                m.append(&flipped);
                m
            }
            Modifier::Array { count, offset } => {
                let mut m = Mesh::default();
                for i in 0..count {
                    let d = Vec3::from_array(offset) * i as f32;
                    m.append(&mesh.transformed(Mat4::from_translation(d)));
                }
                m
            }
            Modifier::Subdivide { levels, smooth } => {
                let mut m = mesh.clone();
                for _ in 0..levels {
                    m = m.subdivide(smooth);
                }
                m
            }
        };
        if out.faces.len() > MAX_FACES {
            return Err(CoreError::Invalid(format!("result would have {} faces (limit {MAX_FACES})", out.faces.len())));
        }
        Ok(out)
    }
}

/// Base mesh with every modifier applied.
pub fn evaluate_mesh(source: &MeshSource, modifiers: &[Modifier]) -> Result<Mesh> {
    let mut m = source.base_mesh()?;
    for md in modifiers {
        m = md.apply(&m)?;
    }
    Ok(m)
}

impl Mesh {
    pub fn validate(&self) -> Result<()> {
        let n = self.positions.len() as u32;
        if self.positions.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CoreError::Invalid("mesh positions must be finite".into()));
        }
        for (i, f) in self.faces.iter().enumerate() {
            if f.len() < 3 {
                return Err(CoreError::Invalid(format!("face {i} has {} vertices (need 3+)", f.len())));
            }
            if let Some(bad) = f.iter().find(|&&v| v >= n) {
                return Err(CoreError::Invalid(format!("face {i} uses vertex {bad}, mesh has {n}")));
            }
        }
        if self.faces.len() > MAX_FACES {
            return Err(CoreError::Invalid(format!("mesh has {} faces (limit {MAX_FACES})", self.faces.len())));
        }
        Ok(())
    }

    pub fn append(&mut self, other: &Mesh) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.faces.extend(other.faces.iter().map(|f| f.iter().map(|v| v + base).collect()));
    }

    pub fn transformed(&self, m: Mat4) -> Mesh {
        let flip = m.determinant() < 0.0;
        Mesh {
            positions: self.positions.iter().map(|p| m.transform_point3(Vec3::from_array(*p)).to_array()).collect(),
            faces: self
                .faces
                .iter()
                .map(|f| {
                    let mut f = f.clone();
                    if flip {
                        f.reverse();
                    }
                    f
                })
                .collect(),
        }
    }

    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut it = self.positions.iter().map(|p| Vec3::from_array(*p));
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p))))
    }

    pub fn edge_count(&self) -> usize {
        let mut edges = std::collections::HashSet::new();
        for f in &self.faces {
            for i in 0..f.len() {
                let (a, b) = (f[i], f[(i + 1) % f.len()]);
                edges.insert((a.min(b), a.max(b)));
            }
        }
        edges.len()
    }

    /// One subdivision step: every n-gon becomes n quads.
    pub fn subdivide(&self, smooth: bool) -> Mesh {
        let pos: Vec<Vec3> = self.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        let face_pts: Vec<Vec3> =
            self.faces.iter().map(|f| f.iter().map(|&v| pos[v as usize]).sum::<Vec3>() / f.len() as f32).collect();

        // edge -> faces that use it
        let mut edge_faces: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        for (fi, f) in self.faces.iter().enumerate() {
            for i in 0..f.len() {
                let (a, b) = (f[i], f[(i + 1) % f.len()]);
                edge_faces.entry((a.min(b), a.max(b))).or_default().push(fi);
            }
        }
        let mut out = Mesh::default();
        let mut new_pos: Vec<Vec3> = pos.clone();
        let face_base = new_pos.len() as u32;
        new_pos.extend(face_pts.iter().copied());
        let mut edge_index: HashMap<(u32, u32), u32> = HashMap::new();
        for (&(a, b), fs) in &edge_faces {
            let mid = (pos[a as usize] + pos[b as usize]) / 2.0;
            let p = if smooth && fs.len() == 2 {
                (pos[a as usize] + pos[b as usize] + face_pts[fs[0]] + face_pts[fs[1]]) / 4.0
            } else {
                mid
            };
            edge_index.insert((a, b), new_pos.len() as u32);
            new_pos.push(p);
        }
        if smooth {
            // Catmull-Clark vertex rule; boundary vertices use the boundary-curve rule.
            let mut v_faces: Vec<Vec<usize>> = vec![Vec::new(); pos.len()];
            let mut v_edges: Vec<Vec<(u32, u32)>> = vec![Vec::new(); pos.len()];
            for (fi, f) in self.faces.iter().enumerate() {
                for &v in f {
                    v_faces[v as usize].push(fi);
                }
            }
            for &(a, b) in edge_faces.keys() {
                v_edges[a as usize].push((a, b));
                v_edges[b as usize].push((a, b));
            }
            for v in 0..pos.len() {
                let n = v_faces[v].len() as f32;
                if n == 0.0 {
                    continue;
                }
                let boundary: Vec<_> = v_edges[v].iter().filter(|e| edge_faces[e].len() != 2).collect();
                if !boundary.is_empty() {
                    if boundary.len() == 2 {
                        let other = |e: &(u32, u32)| if e.0 as usize == v { e.1 } else { e.0 };
                        new_pos[v] = pos[v] * 0.75
                            + (pos[other(boundary[0]) as usize] + pos[other(boundary[1]) as usize]) * 0.125;
                    }
                    continue;
                }
                let f_avg = v_faces[v].iter().map(|&f| face_pts[f]).sum::<Vec3>() / n;
                let e_avg = v_edges[v].iter().map(|&(a, b)| (pos[a as usize] + pos[b as usize]) / 2.0).sum::<Vec3>()
                    / v_edges[v].len() as f32;
                new_pos[v] = (f_avg + 2.0 * e_avg + (n - 3.0) * pos[v]) / n;
            }
        }
        for (fi, f) in self.faces.iter().enumerate() {
            let fc = face_base + fi as u32;
            for i in 0..f.len() {
                let prev = f[(i + f.len() - 1) % f.len()];
                let cur = f[i];
                let next = f[(i + 1) % f.len()];
                let e_prev = edge_index[&(prev.min(cur), prev.max(cur))];
                let e_next = edge_index[&(cur.min(next), cur.max(next))];
                out.faces.push(vec![cur, e_next, fc, e_prev]);
            }
        }
        out.positions = new_pos.into_iter().map(|p| p.to_array()).collect();
        out
    }

    /// Triangulate with normals smoothed across edges flatter than `angle_degrees` and kept
    /// sharp across steeper ones: spheres and capsules look round, boxes keep crisp corners.
    pub fn triangulate_auto_smooth(&self, angle_degrees: f32) -> TriMesh {
        let pos: Vec<Vec3> = self.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        let face_normal = |f: &[u32]| {
            let mut n = Vec3::ZERO;
            for i in 0..f.len() {
                let a = pos[f[i] as usize];
                let b = pos[f[(i + 1) % f.len()] as usize];
                n += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
            }
            n
        };
        // area-weighted face normals (Newell's vector is twice the area) and, per vertex, its faces
        let weighted: Vec<Vec3> = self.faces.iter().map(|f| face_normal(f)).collect();
        let unit: Vec<Vec3> = weighted.iter().map(|n| n.normalize_or_zero()).collect();
        let mut around: Vec<Vec<u32>> = vec![Vec::new(); pos.len()];
        for (fi, f) in self.faces.iter().enumerate() {
            for &v in f {
                around[v as usize].push(fi as u32);
            }
        }
        let cos = angle_degrees.to_radians().cos();
        let mut t = TriMesh::default();
        for (fi, f) in self.faces.iter().enumerate() {
            let base = t.positions.len() as u32;
            for &v in f {
                let mut n = Vec3::ZERO;
                for &g in &around[v as usize] {
                    if unit[g as usize].dot(unit[fi]) >= cos {
                        n += weighted[g as usize];
                    }
                }
                let n = n.normalize_or_zero();
                t.positions.push(self.positions[v as usize]);
                t.normals.push(if n == Vec3::ZERO { unit[fi].to_array() } else { n.to_array() });
            }
            for i in 1..f.len() as u32 - 1 {
                t.indices.extend_from_slice(&[base, base + i, base + i + 1]);
            }
        }
        t
    }

    /// Fan-triangulate faces. `smooth` averages normals per vertex; otherwise faces are flat.
    pub fn triangulate(&self, smooth: bool) -> TriMesh {
        let pos: Vec<Vec3> = self.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        let face_normal = |f: &[u32]| {
            // Newell's method: robust for non-planar n-gons.
            let mut n = Vec3::ZERO;
            for i in 0..f.len() {
                let a = pos[f[i] as usize];
                let b = pos[f[(i + 1) % f.len()] as usize];
                n += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
            }
            n.normalize_or_zero()
        };
        let mut t = TriMesh::default();
        if smooth {
            let mut vn = vec![Vec3::ZERO; pos.len()];
            for f in &self.faces {
                let n = face_normal(f);
                for &v in f {
                    vn[v as usize] += n;
                }
            }
            t.positions = self.positions.clone();
            t.normals = vn.iter().map(|n| n.normalize_or_zero().to_array()).collect();
            for f in &self.faces {
                for i in 1..f.len() - 1 {
                    t.indices.extend_from_slice(&[f[0], f[i], f[i + 1]]);
                }
            }
        } else {
            for f in &self.faces {
                let n = face_normal(f).to_array();
                let base = t.positions.len() as u32;
                for &v in f {
                    t.positions.push(self.positions[v as usize]);
                    t.normals.push(n);
                }
                for i in 1..f.len() as u32 - 1 {
                    t.indices.extend_from_slice(&[base, base + i, base + i + 1]);
                }
            }
        }
        t
    }

    /// Wavefront OBJ text (1-based indices, n-gons kept).
    pub fn to_obj(&self, name: &str) -> String {
        let mut s = format!("# shotstage\no {name}\n");
        for p in &self.positions {
            s.push_str(&format!("v {} {} {}\n", p[0], p[1], p[2]));
        }
        for f in &self.faces {
            s.push('f');
            for v in f {
                s.push_str(&format!(" {}", v + 1));
            }
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Mesh {
        Primitive::Cube { size: [2.0; 3] }.build().unwrap()
    }

    #[test]
    fn auto_smooth_keeps_box_corners_sharp_and_spheres_round() {
        let t = cube().triangulate_auto_smooth(40.0);
        for (p, n) in t.positions.iter().zip(&t.normals) {
            // every cube normal is one of the face axes
            let n = Vec3::from_array(*n);
            assert!((n.abs().max_element() - 1.0).abs() < 1e-5, "{p:?} {n}");
        }
        let s =
            Primitive::Sphere { radius: 1.0, segments: 16, rings: 8 }.build().unwrap().triangulate_auto_smooth(40.0);
        for (p, n) in s.positions.iter().zip(&s.normals) {
            // round: the normal points along the position
            assert!(Vec3::from_array(*p).normalize().dot(Vec3::from_array(*n)) > 0.97);
        }
    }

    #[test]
    fn cube_counts() {
        let m = cube();
        assert_eq!((m.positions.len(), m.faces.len(), m.edge_count()), (8, 6, 12));
    }

    #[test]
    fn cube_normals_point_out() {
        let t = cube().triangulate(false);
        for (p, n) in t.positions.iter().zip(&t.normals) {
            assert!(Vec3::from_array(*p).dot(Vec3::from_array(*n)) > 0.0);
        }
    }

    #[test]
    fn subdivide_counts() {
        let m = cube().subdivide(true);
        assert_eq!(m.faces.len(), 24);
        assert_eq!(m.positions.len(), 8 + 6 + 12);
        // smooth subdivision shrinks the cube towards a sphere
        // corners are pulled in: Catmull-Clark moves a cube corner from 1.0 to 5/9
        let corner = m.positions.iter().map(|p| p[0].abs() + p[1].abs() + p[2].abs()).fold(0.0, f32::max);
        assert!(corner < 3.0);
        assert!((m.positions[0][0] + 5.0 / 9.0).abs() < 1e-5);
        let lin = cube().subdivide(false);
        assert_eq!(lin.bounds().unwrap().1, Vec3::ONE);
    }

    #[test]
    fn mirror_and_array() {
        let m = Modifier::Mirror { axis: 0 }.apply(&cube()).unwrap();
        assert_eq!(m.faces.len(), 12);
        let a = Modifier::Array { count: 3, offset: [3.0, 0.0, 0.0] }.apply(&cube()).unwrap();
        assert_eq!(a.bounds().unwrap().1.x, 7.0);
    }

    #[test]
    fn bad_params_are_errors() {
        assert!(Primitive::Sphere { radius: -1.0, segments: 8, rings: 4 }.build().is_err());
        assert!(Primitive::Sphere { radius: 1.0, segments: 100_000, rings: 4 }.build().is_err());
        assert!(Modifier::Subdivide { levels: 9, smooth: true }.apply(&cube()).is_err());
        let cove = Primitive::Cove { size: [3.0, 2.0, 2.0], radius: 0.5 }.build().unwrap().triangulate(false);
        assert!(cove.normals.first().unwrap()[1] > 0.99, "floor faces up");
        assert!(cove.normals.last().unwrap()[2] > 0.99, "wall faces the front");
        let cap = Primitive::Capsule { radius: 0.5, height: 3.0, segments: 12 }.build().unwrap();
        let (lo, hi) = cap.bounds().unwrap();
        assert!((hi.y - 1.5).abs() < 1e-5 && (lo.y + 1.5).abs() < 1e-5 && (hi.x - 0.5).abs() < 1e-5);
        assert!(Mesh { positions: vec![[0.0; 3]], faces: vec![vec![0, 1, 2]] }.validate().is_err());
    }

    #[test]
    fn primitives_build() {
        for p in [
            Primitive::Plane { size: [1.0, 1.0] },
            Primitive::Sphere { radius: 1.0, segments: 16, rings: 8 },
            Primitive::Cylinder { radius: 1.0, height: 2.0, segments: 12 },
            Primitive::Cone { radius: 1.0, height: 2.0, segments: 12 },
            Primitive::Torus { major_radius: 1.0, minor_radius: 0.25, major_segments: 16, minor_segments: 8 },
            Primitive::Capsule { radius: 0.5, height: 3.0, segments: 12 },
            Primitive::Capsule { radius: 0.5, height: 0.2, segments: 8 },
        ] {
            let m = p.build().unwrap();
            m.validate().unwrap();
            // every face normal points away from the origin (all primitives are centred, convex
            // in the direction of their face centres)
            let t = m.triangulate(false);
            for tri in t.indices.chunks(3) {
                let c: Vec3 = tri.iter().map(|&i| Vec3::from_array(t.positions[i as usize])).sum::<Vec3>() / 3.0;
                let n = Vec3::from_array(t.normals[tri[0] as usize]);
                assert!(c.dot(n) >= -1e-4 || matches!(p, Primitive::Torus { .. }), "{p:?} face points inward");
            }
            assert!(!m.triangulate(true).indices.is_empty());
        }
    }
}
