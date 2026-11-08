//! Textures on meshes: a [`Material`] says which picture is painted on a mesh and how it is
//! laid on — projected (box, planar, cylinder, sphere: robust through any edit) or through the
//! mesh's own UV map. [`unwrap`] makes a UV map (smart charts or a baked projection) and
//! [`transform_uvs`] moves, turns and scales the UVs of some faces.
//!
//! UV space: `u` runs left to right across the picture, `v` top to bottom (like image pixels).

use std::collections::{BTreeSet, HashMap};

use glam::{Vec2, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;
use crate::{CoreError, Result};

/// How a texture is laid on a mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Mapping {
    /// From six sides, each face from the side it faces most (boxes, shelves, crates).
    #[default]
    Box,
    /// From one side along `axis` (a label on the front, a floor decal).
    Planar,
    /// Wrapped round `axis` (cans, bottles, jars); the ends get a flat projection.
    Cylinder,
    /// Wrapped round a ball (fruit, globes).
    Sphere,
    /// The mesh's own UV map (from unwrapping or an imported model); box where it has none.
    Uv,
}

/// What a mesh is painted with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct Material {
    /// Key of an image asset painted on the surface (multiplied by the mesh colour). None: plain
    /// colour.
    pub texture: Option<String>,
    pub mapping: Mapping,
    /// Axis for planar and cylinder mapping: 0 = X, 1 = Y, 2 = Z. Default: Z for planar (seen
    /// from the front), Y for cylinder (upright).
    pub axis: Option<u8>,
    /// How many times the picture repeats across `[u, v]`. Default `[1, 1]`.
    pub repeat: [f32; 2],
    /// Shift of the picture in UV units (1 = one picture).
    pub offset: [f32; 2],
    /// Turn of the picture, degrees.
    pub rotation: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self { texture: None, mapping: Mapping::Box, axis: None, repeat: [1.0, 1.0], offset: [0.0, 0.0], rotation: 0.0 }
    }
}

impl Material {
    pub fn validate(&self) -> Result<()> {
        if self.axis.is_some_and(|a| a > 2) {
            return Err(CoreError::Invalid("material axis must be 0, 1 or 2".into()));
        }
        let ok = |v: f32| v.is_finite() && v.abs() <= 10_000.0;
        if !(self.repeat.iter().all(|r| ok(*r) && *r != 0.0) && self.offset.iter().all(|o| ok(*o)) && ok(self.rotation))
        {
            return Err(CoreError::Invalid("material repeat (non-zero), offset and rotation must be finite".into()));
        }
        Ok(())
    }

    fn axis_or(&self, default: u8) -> usize {
        self.axis.unwrap_or(default).min(2) as usize
    }

    /// Repeat, offset and rotation applied to a base coordinate.
    pub fn adjust(&self, uv: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.rotation.to_radians().sin_cos();
        let d = Vec2::new(uv[0] - 0.5, uv[1] - 0.5);
        let r = Vec2::new(d.x * c - d.y * s, d.x * s + d.y * c);
        [r.x * self.repeat[0] + 0.5 + self.offset[0], r.y * self.repeat[1] + 0.5 + self.offset[1]]
    }
}

/// Bounding box used to fit projections to the object.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    pub fn of(points: impl IntoIterator<Item = Vec3>) -> Self {
        let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in points {
            min = min.min(p);
            max = max.max(p);
        }
        if min.x > max.x {
            return Self { min: Vec3::ZERO, max: Vec3::ONE };
        }
        Self { min, max }
    }
    fn unit(&self, p: Vec3) -> Vec3 {
        (p - self.min) / (self.max - self.min).max(Vec3::splat(1e-6))
    }
    fn center(&self) -> Vec3 {
        (self.min + self.max) / 2.0
    }
}

/// Coordinates of a box side seen from outside: `dir` 0..6 = +X, -X, +Y, -Y, +Z, -Z.
fn side(t: Vec3, dir: usize) -> [f32; 2] {
    match dir {
        0 => [1.0 - t.z, 1.0 - t.y],
        1 => [t.z, 1.0 - t.y],
        2 => [t.x, t.z],
        3 => [t.x, 1.0 - t.z],
        4 => [t.x, 1.0 - t.y],
        _ => [1.0 - t.x, 1.0 - t.y],
    }
}

fn dominant(n: Vec3) -> usize {
    let a = n.abs();
    if a.x >= a.y && a.x >= a.z {
        if n.x >= 0.0 { 0 } else { 1 }
    } else if a.y >= a.z {
        if n.y >= 0.0 { 2 } else { 3 }
    } else if n.z >= 0.0 {
        4
    } else {
        5
    }
}

/// Swap coordinates so `axis` becomes Y (for cylinder and sphere wraps).
fn to_y_up(v: Vec3, axis: usize) -> Vec3 {
    match axis {
        0 => Vec3::new(v.y, v.x, v.z),
        2 => Vec3::new(v.x, v.z, -v.y),
        _ => v,
    }
}

/// Wrap angle round Y: 0.5 at the front (+Z), the seam at the back.
fn around(v: Vec3) -> f32 {
    0.5 + v.x.atan2(v.z) / std::f32::consts::TAU
}

/// Keep a face from spanning the seam: pull corners across so they stay together.
fn unseam(uvs: &mut [[f32; 2]]) {
    let lo = uvs.iter().map(|u| u[0]).fold(f32::MAX, f32::min);
    let hi = uvs.iter().map(|u| u[0]).fold(f32::MIN, f32::max);
    if hi - lo > 0.5 {
        for u in uvs.iter_mut() {
            if u[0] < 0.5 {
                u[0] += 1.0;
            }
        }
    }
}

/// Base (unadjusted) projected coordinates of one face's corners.
pub fn project(points: &[Vec3], normal: Vec3, b: &Bounds, mapping: Mapping, axis: Option<u8>) -> Vec<[f32; 2]> {
    let m = Material { axis, ..Default::default() };
    match mapping {
        Mapping::Box | Mapping::Uv => {
            let d = dominant(normal);
            points.iter().map(|p| side(b.unit(*p), d)).collect()
        }
        Mapping::Planar => {
            let a = m.axis_or(2);
            points.iter().map(|p| side(b.unit(*p), a * 2)).collect()
        }
        Mapping::Cylinder => {
            let a = m.axis_or(1);
            let up = to_y_up(normal, a);
            if up.y.abs() > 0.9 {
                // an end cap: seen from above / below
                return points
                    .iter()
                    .map(|p| side(to_y_up(b.unit(*p) - 0.5, a) + 0.5, if up.y > 0.0 { 2 } else { 3 }))
                    .collect();
            }
            let c = b.center();
            let mut out: Vec<[f32; 2]> = points
                .iter()
                .map(|p| {
                    let v = to_y_up(*p - c, a);
                    let t = to_y_up(b.unit(*p) - 0.5, a) + 0.5;
                    [around(v), 1.0 - t.y]
                })
                .collect();
            unseam(&mut out);
            out
        }
        Mapping::Sphere => {
            let c = b.center();
            let mut out: Vec<[f32; 2]> = points
                .iter()
                .map(|p| {
                    let v = (*p - c).normalize_or_zero();
                    [around(v), v.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI]
                })
                .collect();
            unseam(&mut out);
            out
        }
    }
}

fn face_points(m: &Mesh, f: &[u32]) -> Vec<Vec3> {
    f.iter().map(|&v| Vec3::from_array(m.positions[v as usize])).collect()
}

fn mesh_bounds(m: &Mesh) -> Bounds {
    Bounds::of(m.positions.iter().map(|p| Vec3::from_array(*p)))
}

/// Final texture coordinates of every face corner of `m` under `mat`.
pub fn face_uvs(m: &Mesh, mat: &Material) -> Vec<Vec<[f32; 2]>> {
    let b = mesh_bounds(m);
    let base: Vec<Vec<[f32; 2]>> = if mat.mapping == Mapping::Uv && m.has_uvs() {
        m.uvs.clone()
    } else {
        m.faces
            .iter()
            .map(|f| {
                let pts = face_points(m, f);
                project(&pts, crate::meshedit::face_normal(m, f), &b, mat.mapping, mat.axis)
            })
            .collect()
    };
    base.into_iter().map(|f| f.into_iter().map(|uv| mat.adjust(uv)).collect()).collect()
}

/// Texture coordinates for each index of a triangulated mesh (what the renderer draws).
pub fn tri_uvs(tri: &crate::mesh::TriMesh, mat: &Material) -> Vec<[f32; 2]> {
    let p = |i: u32| Vec3::from_array(tri.positions[i as usize]);
    if mat.mapping == Mapping::Uv && tri.uvs.len() == tri.positions.len() {
        return tri.indices.iter().map(|&i| mat.adjust(tri.uvs[i as usize])).collect();
    }
    let b = Bounds::of(tri.positions.iter().map(|q| Vec3::from_array(*q)));
    let mut out = Vec::with_capacity(tri.indices.len());
    for t in tri.indices.as_chunks::<3>().0 {
        let pts = [p(t[0]), p(t[1]), p(t[2])];
        let n = (pts[1] - pts[0]).cross(pts[2] - pts[0]).normalize_or_zero();
        // a smooth triangle's normal, for wrap seams and caps: the vertex normals' average
        let smooth: Vec3 = t.iter().map(|&i| Vec3::from_array(tri.normals[i as usize])).sum();
        let n = if mat.mapping == Mapping::Box || mat.mapping == Mapping::Uv { n } else { smooth.normalize_or(n) };
        out.extend(project(&pts, n, &b, mat.mapping, mat.axis).into_iter().map(|uv| mat.adjust(uv)));
    }
    out
}

/// How [`unwrap`] makes UVs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum UnwrapMethod {
    /// Split into flat-ish pieces (faces within `angle` of each other), lay each flat and pack
    /// them without overlap, keeping their relative size (Blender's Smart UV Project).
    #[default]
    Smart,
    /// Bake a box projection.
    Box,
    /// Bake a planar projection along the axis.
    Planar,
    /// Bake a cylinder wrap round the axis.
    Cylinder,
    /// Bake a sphere wrap.
    Sphere,
}

/// Give `m` a UV map: the `faces` given (all when empty) get new coordinates, the rest keep
/// theirs (or a box projection if the mesh had none). `angle`: smart project's limit in
/// degrees (default 66); `axis`: for planar / cylinder; `margin`: gap between smart pieces
/// as a fraction of the square (default 0.01).
pub fn unwrap(
    m: &Mesh,
    faces: &[u32],
    method: UnwrapMethod,
    angle: Option<f32>,
    axis: Option<u8>,
    margin: Option<f32>,
) -> Result<Mesh> {
    let nf = m.faces.len() as u32;
    if faces.iter().any(|&f| f >= nf) {
        return Err(CoreError::Invalid("a face to unwrap does not exist".into()));
    }
    if m.faces.is_empty() {
        return Err(CoreError::Invalid("the mesh has no faces".into()));
    }
    if axis.is_some_and(|a| a > 2) {
        return Err(CoreError::Invalid("axis must be 0, 1 or 2".into()));
    }
    let picked: BTreeSet<u32> = if faces.is_empty() { (0..nf).collect() } else { faces.iter().copied().collect() };
    let mut out = m.clone();
    if !out.has_uvs() {
        out.uvs = face_uvs(m, &Material::default());
    }
    let b = mesh_bounds(m);
    let mapping = match method {
        UnwrapMethod::Smart => None,
        UnwrapMethod::Box => Some(Mapping::Box),
        UnwrapMethod::Planar => Some(Mapping::Planar),
        UnwrapMethod::Cylinder => Some(Mapping::Cylinder),
        UnwrapMethod::Sphere => Some(Mapping::Sphere),
    };
    if let Some(mapping) = mapping {
        for &fi in &picked {
            let f = &m.faces[fi as usize];
            out.uvs[fi as usize] = project(&face_points(m, f), crate::meshedit::face_normal(m, f), &b, mapping, axis);
        }
        return Ok(out);
    }
    let angle = angle.unwrap_or(66.0);
    if !(angle.is_finite() && (1.0..=89.0).contains(&angle)) {
        return Err(CoreError::Invalid("angle must be 1..=89 degrees".into()));
    }
    let margin = margin.unwrap_or(0.01);
    if !(margin.is_finite() && (0.0..=0.2).contains(&margin)) {
        return Err(CoreError::Invalid("margin must be 0..=0.2".into()));
    }
    let cos = angle.to_radians().cos();
    let normals: Vec<Vec3> = m.faces.iter().map(|f| crate::meshedit::face_normal(m, f)).collect();
    let topo = crate::meshedit::Topology::new(m);
    // charts: grow from a seed while neighbours face about the same way as the seed
    let mut chart_of: HashMap<u32, usize> = HashMap::new();
    let mut charts: Vec<Vec<u32>> = Vec::new();
    for &seed in &picked {
        if chart_of.contains_key(&seed) {
            continue;
        }
        let n0 = normals[seed as usize];
        let id = charts.len();
        let mut chart = vec![seed];
        chart_of.insert(seed, id);
        let mut i = 0;
        while i < chart.len() {
            let f = &m.faces[chart[i] as usize];
            for k in 0..f.len() {
                let e = crate::meshedit::ek(f[k], f[(k + 1) % f.len()]);
                for &g in topo.faces_of(e) {
                    let g = g as u32;
                    if picked.contains(&g) && !chart_of.contains_key(&g) && normals[g as usize].dot(n0) >= cos {
                        chart_of.insert(g, id);
                        chart.push(g);
                    }
                }
            }
            i += 1;
        }
        charts.push(chart);
    }
    // lay each chart flat (metres), then pack them on shelves
    struct Piece {
        faces: Vec<u32>,
        uvs: Vec<Vec<Vec2>>,
        size: Vec2,
    }
    let mut pieces: Vec<Piece> = Vec::new();
    for chart in charts {
        let n: Vec3 = chart.iter().map(|&f| crate::meshedit::face_normal_raw(m, &m.faces[f as usize])).sum::<Vec3>();
        let n = if n.length_squared() < 1e-12 { normals[chart[0] as usize] } else { n.normalize() };
        let n = if n == Vec3::ZERO { Vec3::Z } else { n };
        // keep "up" up where the chart is not horizontal
        let up = if n.y.abs() < 0.9 { Vec3::Y } else { Vec3::NEG_Z };
        let ux = up.cross(n).normalize_or_zero();
        let uy = n.cross(ux);
        let mut uvs: Vec<Vec<Vec2>> = chart
            .iter()
            .map(|&f| {
                m.faces[f as usize]
                    .iter()
                    .map(|&v| {
                        let p = Vec3::from_array(m.positions[v as usize]);
                        Vec2::new(p.dot(ux), -p.dot(uy))
                    })
                    .collect()
            })
            .collect();
        let lo = uvs.iter().flatten().fold(Vec2::splat(f32::MAX), |a, b| a.min(*b));
        let hi = uvs.iter().flatten().fold(Vec2::splat(f32::MIN), |a, b| a.max(*b));
        for f in &mut uvs {
            for p in f.iter_mut() {
                *p -= lo;
            }
        }
        pieces.push(Piece { faces: chart, uvs, size: (hi - lo).max(Vec2::splat(1e-6)) });
    }
    pieces.sort_by(|a, b| b.size.y.total_cmp(&a.size.y));
    let sizes: Vec<Vec2> = pieces.iter().map(|p| p.size).collect();
    let (placed, side) = pack_rects(&sizes, margin);
    for (p, at) in pieces.iter().zip(&placed) {
        for (k, &f) in p.faces.iter().enumerate() {
            out.uvs[f as usize] = p.uvs[k].iter().map(|q| [(q.x + at.x) / side, (q.y + at.y) / side]).collect();
        }
    }
    Ok(out)
}

/// Shelf-pack rectangles (best sorted tallest first) into the smallest square found: their
/// corners and the square's side, both in the rectangles' units. `margin`: gap between them
/// as a fraction of the side.
pub fn pack_rects(sizes: &[Vec2], margin: f32) -> (Vec<Vec2>, f32) {
    let area: f32 = sizes.iter().map(|s| s.x * s.y).sum();
    let widest = sizes.iter().map(|s| s.x).fold(0.0f32, f32::max);
    let tallest = sizes.iter().map(|s| s.y).fold(0.0f32, f32::max);
    // try wider rows until everything fits in a square
    let mut side = area.sqrt().max(widest).max(tallest).max(1e-6);
    let mut placed: Vec<Vec2> = Vec::new();
    for _ in 0..64 {
        placed.clear();
        let gap = margin * side;
        let (mut x, mut y, mut row) = (gap, gap, 0.0f32);
        for s in sizes {
            if x + s.x + gap > side && x > gap {
                x = gap;
                y += row + gap;
                row = 0.0;
            }
            placed.push(Vec2::new(x, y));
            x += s.x + gap;
            row = row.max(s.y);
        }
        if y + row + gap <= side * 1.0001 && sizes.iter().zip(&placed).all(|(s, p)| p.x + s.x + gap <= side * 1.0001) {
            break;
        }
        side *= 1.08;
    }
    (placed, side.max(1e-6))
}

/// Move, turn (degrees) and scale the UVs of some faces (all when empty) about their centre.
pub fn transform_uvs(m: &Mesh, faces: &[u32], translate: [f32; 2], rotate: f32, scale: [f32; 2]) -> Result<Mesh> {
    let corners = crate::uvedit::pick(m, faces, &[])?;
    crate::uvedit::transform_corners(m, &corners, translate, rotate, scale, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Primitive;

    fn in_unit(uvs: &[Vec<[f32; 2]>]) -> bool {
        uvs.iter().flatten().all(|u| (-1e-4..=1.0001).contains(&u[0]) && (-1e-4..=1.0001).contains(&u[1]))
    }

    #[test]
    fn projections_fit_the_object() {
        let cube = Primitive::Cube { size: [2.0, 1.0, 0.5] }.build().unwrap();
        for mapping in [Mapping::Box, Mapping::Planar, Mapping::Cylinder, Mapping::Sphere] {
            let uvs = face_uvs(&cube, &Material { mapping, ..Default::default() });
            assert_eq!(uvs.len(), cube.faces.len());
            assert!(uvs.iter().flatten().all(|u| u[0].is_finite() && u[1].is_finite()));
            if mapping == Mapping::Box {
                assert!(in_unit(&uvs));
            }
        }
        // the front of a box sees the whole picture the right way up
        let front =
            (0..cube.faces.len()).find(|&f| crate::meshedit::face_normal(&cube, &cube.faces[f]).z > 0.9).unwrap();
        let uvs = face_uvs(&cube, &Material::default());
        let top_left = cube.faces[front]
            .iter()
            .position(|&v| cube.positions[v as usize][0] < 0.0 && cube.positions[v as usize][1] > 0.0)
            .unwrap();
        assert_eq!(uvs[front][top_left], [0.0, 0.0]);
        // a can's side wraps once round without a smeared seam face
        let can = Primitive::Cylinder { radius: 0.3, height: 1.0, segments: 16 }.build().unwrap();
        let uvs = face_uvs(&can, &Material { mapping: Mapping::Cylinder, ..Default::default() });
        for f in uvs.iter().filter(|f| f.len() == 4) {
            let lo = f.iter().map(|u| u[0]).fold(f32::MAX, f32::min);
            let hi = f.iter().map(|u| u[0]).fold(f32::MIN, f32::max);
            assert!(hi - lo < 0.5);
        }
        // repeat and offset
        let m = Material { repeat: [2.0, 2.0], offset: [0.25, 0.0], ..Default::default() };
        assert_eq!(m.adjust([0.5, 0.5]), [0.75, 0.5]);
        assert_eq!(m.adjust([1.0, 1.0]), [1.75, 1.5]);
    }

    #[test]
    fn smart_unwrap_packs_without_overlap() {
        let cube = Primitive::Cube { size: [1.0, 2.0, 1.0] }.build().unwrap();
        let out = unwrap(&cube, &[], UnwrapMethod::Smart, None, None, None).unwrap();
        assert!(out.has_uvs());
        assert!(in_unit(&out.uvs));
        // six charts, each a rectangle: their boxes do not overlap
        let boxes: Vec<(Vec2, Vec2)> = out
            .uvs
            .iter()
            .map(|f| {
                let lo = f.iter().fold(Vec2::splat(9.0), |a, b| a.min(Vec2::from_array(*b)));
                let hi = f.iter().fold(Vec2::splat(-9.0), |a, b| a.max(Vec2::from_array(*b)));
                (lo, hi)
            })
            .collect();
        for i in 0..boxes.len() {
            for j in i + 1..boxes.len() {
                let (a, b) = (boxes[i], boxes[j]);
                let overlap =
                    a.0.x < b.1.x - 1e-4 && b.0.x < a.1.x - 1e-4 && a.0.y < b.1.y - 1e-4 && b.0.y < a.1.y - 1e-4;
                assert!(!overlap, "pieces {i} and {j} overlap");
            }
        }
        // same texel density: the tall sides are twice as tall as the top is deep
        let h = |f: usize| boxes[f].1.y - boxes[f].0.y;
        let side = (0..6).find(|&f| crate::meshedit::face_normal(&cube, &cube.faces[f]).z > 0.9).unwrap();
        let top = (0..6).find(|&f| crate::meshedit::face_normal(&cube, &cube.faces[f]).y > 0.9).unwrap();
        assert!((h(side) / h(top) - 2.0).abs() < 1e-3);
        // a sphere unwraps into several pieces, all inside the square
        let ball = Primitive::Sphere { radius: 1.0, segments: 16, rings: 8 }.build().unwrap();
        let out = unwrap(&ball, &[], UnwrapMethod::Smart, None, None, None).unwrap();
        assert!(in_unit(&out.uvs));
        // only some faces: the others keep their coordinates
        let first = unwrap(&cube, &[], UnwrapMethod::Box, None, None, None).unwrap();
        let again = unwrap(&first, &[0], UnwrapMethod::Planar, None, Some(1), None).unwrap();
        assert_eq!(first.uvs[1], again.uvs[1]);
    }

    #[test]
    fn move_uvs_of_some_faces() {
        let plane = Primitive::Plane { size: [1.0, 1.0] }.build().unwrap();
        let out = transform_uvs(&plane, &[], [0.5, 0.0], 0.0, [0.5, 0.5]).unwrap();
        assert_eq!(out.uvs[0][0], [0.75, 0.75]);
        let bare = Primitive::Cube { size: [1.0; 3] }.build().unwrap();
        assert!(transform_uvs(&bare, &[], [0.0; 2], 0.0, [1.0; 2]).is_err());
    }
}
