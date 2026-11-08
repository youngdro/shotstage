//! Editing a mesh's UV map piece by piece, like Blender's UV editor.
//!
//! A UV map stores one coordinate per face corner ([`Corner`] = `[face, index in the face]`).
//! Corners of the same mesh vertex at the same spot are one *UV vertex* (moving one moves the
//! others when "sticky"); faces joined through UV edges whose ends are shared UV vertices form
//! an *island* (one flat piece of the unwrap).

use std::collections::{BTreeSet, HashMap};

use glam::Vec2;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;
use crate::{CoreError, Result};

/// A face corner: `[face, index in the face]`.
pub type Corner = [u32; 2];

/// How the UV map hangs together: UV vertices and islands.
#[derive(Clone, Debug, Default)]
pub struct UvTopology {
    /// UV vertex of each corner: `group[face][k]`.
    pub group: Vec<Vec<u32>>,
    /// Corners of each UV vertex.
    pub groups: Vec<Vec<Corner>>,
    /// Island of each face.
    pub island: Vec<u32>,
    /// Faces of each island.
    pub islands: Vec<Vec<u32>>,
}

/// Two coordinates closer than this are the same spot.
const WELD: f32 = 1e-5;

fn key(v: u32, uv: [f32; 2]) -> (u32, i64, i64) {
    (v, (uv[0] / WELD).round() as i64, (uv[1] / WELD).round() as i64)
}

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        parent[x as usize] = parent[parent[x as usize] as usize];
        x = parent[x as usize];
    }
    x
}

impl UvTopology {
    /// Empty when the mesh has no UV map.
    pub fn new(m: &Mesh) -> Self {
        if !m.has_uvs() {
            return Self::default();
        }
        let mut ids: HashMap<(u32, i64, i64), u32> = HashMap::new();
        let mut groups: Vec<Vec<Corner>> = Vec::new();
        let group: Vec<Vec<u32>> = m
            .faces
            .iter()
            .enumerate()
            .map(|(f, face)| {
                face.iter()
                    .enumerate()
                    .map(|(k, &v)| {
                        let g = *ids.entry(key(v, m.uvs[f][k])).or_insert_with(|| {
                            groups.push(Vec::new());
                            groups.len() as u32 - 1
                        });
                        groups[g as usize].push([f as u32, k as u32]);
                        g
                    })
                    .collect()
            })
            .collect();
        // faces sharing a UV edge (the same two UV vertices) are in one island
        let mut parent: Vec<u32> = (0..m.faces.len() as u32).collect();
        let mut edge_face: HashMap<(u32, u32), u32> = HashMap::new();
        for (f, g) in group.iter().enumerate() {
            for k in 0..g.len() {
                let (a, b) = (g[k], g[(k + 1) % g.len()]);
                match edge_face.entry((a.min(b), a.max(b))) {
                    std::collections::hash_map::Entry::Occupied(o) => {
                        let (x, y) = (find(&mut parent, *o.get()), find(&mut parent, f as u32));
                        parent[x.max(y) as usize] = x.min(y);
                    }
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(f as u32);
                    }
                }
            }
        }
        let mut island = vec![0u32; m.faces.len()];
        let mut islands: Vec<Vec<u32>> = Vec::new();
        let mut of_root: HashMap<u32, u32> = HashMap::new();
        for f in 0..m.faces.len() as u32 {
            let r = find(&mut parent, f);
            let i = *of_root.entry(r).or_insert_with(|| {
                islands.push(Vec::new());
                islands.len() as u32 - 1
            });
            island[f as usize] = i;
            islands[i as usize].push(f);
        }
        Self { group, groups, island, islands }
    }

    /// The corners plus every corner sharing their UV vertex.
    pub fn sticky(&self, corners: &[Corner]) -> Vec<Corner> {
        let gs: BTreeSet<u32> =
            corners.iter().filter_map(|c| self.group.get(c[0] as usize)?.get(c[1] as usize).copied()).collect();
        let mut out: Vec<Corner> = gs.iter().flat_map(|&g| self.groups[g as usize].iter().copied()).collect();
        out.sort_unstable();
        out
    }

    /// Every face of the islands the faces are in.
    pub fn island_faces(&self, faces: &[u32]) -> Vec<u32> {
        let picked: BTreeSet<u32> = faces.iter().filter_map(|&f| self.island.get(f as usize).copied()).collect();
        let mut out: Vec<u32> = picked.iter().flat_map(|&i| self.islands[i as usize].iter().copied()).collect();
        out.sort_unstable();
        out
    }
}

/// Every corner of the faces (all faces when empty).
pub fn face_corners(m: &Mesh, faces: &[u32]) -> Vec<Corner> {
    let all: Vec<u32>;
    let faces = if faces.is_empty() {
        all = (0..m.faces.len() as u32).collect();
        &all
    } else {
        faces
    };
    faces
        .iter()
        .filter(|&&f| (f as usize) < m.faces.len())
        .flat_map(|&f| (0..m.faces[f as usize].len() as u32).map(move |k| [f, k]))
        .collect()
}

/// The corners of `faces` and `corners` together, checked, sorted, without repeats. Both
/// empty: every corner.
pub fn pick(m: &Mesh, faces: &[u32], corners: &[Corner]) -> Result<Vec<Corner>> {
    if !m.has_uvs() {
        return Err(CoreError::Invalid("the mesh has no UV map yet: unwrap it first".into()));
    }
    if faces.iter().any(|&f| f as usize >= m.faces.len()) {
        return Err(CoreError::Invalid("a face does not exist".into()));
    }
    if corners.iter().any(|c| m.faces.get(c[0] as usize).is_none_or(|f| c[1] as usize >= f.len())) {
        return Err(CoreError::Invalid("a corner does not exist (corners are [face, index in the face])".into()));
    }
    let mut out: BTreeSet<Corner> = corners.iter().copied().collect();
    if !faces.is_empty() || corners.is_empty() {
        out.extend(face_corners(m, faces));
    }
    Ok(out.into_iter().collect())
}

fn at(m: &Mesh, c: Corner) -> Vec2 {
    Vec2::from_array(m.uvs[c[0] as usize][c[1] as usize])
}

fn put(m: &mut Mesh, c: Corner, p: Vec2) {
    m.uvs[c[0] as usize][c[1] as usize] = p.to_array();
}

/// Middle of the corners' bounding box.
pub fn bounds_center(m: &Mesh, corners: &[Corner]) -> Vec2 {
    let (lo, hi) = bounds(m, corners);
    (lo + hi) / 2.0
}

/// Bounding box of the corners' coordinates.
pub fn bounds(m: &Mesh, corners: &[Corner]) -> (Vec2, Vec2) {
    corners.iter().fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(lo, hi), &c| {
        let p = at(m, c);
        (lo.min(p), hi.max(p))
    })
}

fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() <= 1e6)
}

/// Move, turn (degrees, counter-clockwise as seen in the picture) and scale the corners about
/// `pivot` (default: their mean). Scale is applied before the turn.
pub fn transform_corners(
    m: &Mesh,
    corners: &[Corner],
    translate: [f32; 2],
    rotate: f32,
    scale: [f32; 2],
    pivot: Option<[f32; 2]>,
) -> Result<Mesh> {
    if !(finite(&translate) && finite(&scale) && rotate.is_finite() && pivot.is_none_or(|p| finite(&p))) {
        return Err(CoreError::Invalid("translate, rotate, scale and pivot must be finite".into()));
    }
    let c = match pivot {
        Some(p) => Vec2::from_array(p),
        None => corners.iter().map(|&c| at(m, c)).sum::<Vec2>() / corners.len().max(1) as f32,
    };
    // v points down in the picture, so a visually counter-clockwise turn is clockwise in (u, v)
    let (s, co) = (-rotate).to_radians().sin_cos();
    let mut out = m.clone();
    for &k in corners {
        let d = (at(m, k) - c) * Vec2::from_array(scale);
        let r = Vec2::new(d.x * co - d.y * s, d.x * s + d.y * co);
        put(&mut out, k, c + r + Vec2::from_array(translate));
    }
    Ok(out)
}

/// Put each corner at its own coordinate.
pub fn set_corners(m: &Mesh, corners: &[Corner], uvs: &[[f32; 2]]) -> Result<Mesh> {
    if corners.len() != uvs.len() {
        return Err(CoreError::Invalid("give one coordinate per corner".into()));
    }
    if !uvs.iter().all(|u| finite(u)) {
        return Err(CoreError::Invalid("coordinates must be finite".into()));
    }
    let mut out = m.clone();
    for (c, u) in corners.iter().zip(uvs) {
        put(&mut out, *c, Vec2::from_array(*u));
    }
    Ok(out)
}

/// Which side [`align`] lines corners up on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AlignTo {
    /// Left (u) or top (v) of the selection.
    Min,
    /// Their middle.
    #[default]
    Center,
    /// Right (u) or bottom (v).
    Max,
}

/// Line the corners up: the same `u` (axis 0, a vertical line) or the same `v` (axis 1).
pub fn align(m: &Mesh, corners: &[Corner], axis: usize, to: AlignTo) -> Result<Mesh> {
    if axis > 1 {
        return Err(CoreError::Invalid("axis must be 0 (u) or 1 (v)".into()));
    }
    let (lo, hi) = bounds(m, corners);
    let x = match to {
        AlignTo::Min => lo[axis],
        AlignTo::Max => hi[axis],
        AlignTo::Center => (lo[axis] + hi[axis]) / 2.0,
    };
    let mut out = m.clone();
    for &c in corners {
        let mut p = at(m, c);
        p[axis] = x;
        put(&mut out, c, p);
    }
    Ok(out)
}

/// Bring corners together: all to their common centre, or (`by_vertex`) each mesh vertex's
/// corners to their own centre: stitches pieces that were cut apart back along a seam.
pub fn weld(m: &Mesh, corners: &[Corner], by_vertex: bool) -> Result<Mesh> {
    let mut sums: HashMap<u32, (Vec2, f32)> = HashMap::new();
    for &c in corners {
        let v = if by_vertex { m.faces[c[0] as usize][c[1] as usize] } else { 0 };
        let e = sums.entry(v).or_insert((Vec2::ZERO, 0.0));
        e.0 += at(m, c);
        e.1 += 1.0;
    }
    let mut out = m.clone();
    for &c in corners {
        let v = if by_vertex { m.faces[c[0] as usize][c[1] as usize] } else { 0 };
        let (s, n) = sums[&v];
        put(&mut out, c, s / n);
    }
    Ok(out)
}

/// Lay the islands containing `faces` (all when empty) out again inside the picture square
/// without overlap, keeping their sizes relative to each other (`margin`: gap as a fraction of
/// the square). With `rotate`, tall islands are turned to lie flat, which packs tighter.
pub fn pack_islands(m: &Mesh, faces: &[u32], margin: f32, rotate: bool) -> Result<Mesh> {
    if !m.has_uvs() {
        return Err(CoreError::Invalid("the mesh has no UV map yet: unwrap it first".into()));
    }
    if faces.iter().any(|&f| f as usize >= m.faces.len()) {
        return Err(CoreError::Invalid("a face does not exist".into()));
    }
    if !(margin.is_finite() && (0.0..=0.2).contains(&margin)) {
        return Err(CoreError::Invalid("margin must be 0..=0.2".into()));
    }
    let topo = UvTopology::new(m);
    let chosen: BTreeSet<u32> = if faces.is_empty() {
        (0..topo.islands.len() as u32).collect()
    } else {
        faces.iter().map(|&f| topo.island[f as usize]).collect()
    };
    struct Piece {
        corners: Vec<Corner>,
        lo: Vec2,
        size: Vec2,
        turned: bool,
    }
    let mut pieces: Vec<Piece> = chosen
        .iter()
        .map(|&i| {
            let corners = face_corners(m, &topo.islands[i as usize]);
            let (lo, hi) = bounds(m, &corners);
            let size = (hi - lo).max(Vec2::splat(1e-6));
            let turned = rotate && size.y > size.x * 1.001;
            Piece { corners, lo, size: if turned { Vec2::new(size.y, size.x) } else { size }, turned }
        })
        .collect();
    pieces.sort_by(|a, b| b.size.y.total_cmp(&a.size.y));
    let sizes: Vec<Vec2> = pieces.iter().map(|p| p.size).collect();
    let (placed, side) = crate::uv::pack_rects(&sizes, margin);
    let mut out = m.clone();
    for (p, to) in pieces.iter().zip(&placed) {
        for &c in &p.corners {
            let d = at(m, c) - p.lo;
            // a quarter turn that keeps the piece in its box: (x, y) -> (y, w - x)
            let d = if p.turned { Vec2::new(d.y, p.size.y - d.x) } else { d };
            put(&mut out, c, (d + *to) / side);
        }
    }
    Ok(out)
}

/// Facts about each island: its faces, bounding box and area in UV space.
pub fn island_info(m: &Mesh) -> Vec<serde_json::Value> {
    let topo = UvTopology::new(m);
    topo.islands
        .iter()
        .map(|faces| {
            let corners = face_corners(m, faces);
            let (lo, hi) = bounds(m, &corners);
            let area: f32 = faces
                .iter()
                .map(|&f| {
                    let p = &m.uvs[f as usize];
                    let mut a = 0.0;
                    for k in 0..p.len() {
                        let (x, y) = (p[k], p[(k + 1) % p.len()]);
                        a += x[0] * y[1] - y[0] * x[1];
                    }
                    a.abs() / 2.0
                })
                .sum();
            serde_json::json!({ "faces": faces, "min": lo.to_array(), "max": hi.to_array(), "area": area })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Primitive;
    use crate::uv::{UnwrapMethod, unwrap};

    fn cube_smart() -> Mesh {
        let cube = Primitive::Cube { size: [1.0, 1.0, 1.0] }.build().unwrap();
        unwrap(&cube, &[], UnwrapMethod::Smart, None, None, None).unwrap()
    }

    #[test]
    fn islands_and_uv_vertices() {
        // smart unwrap of a cube: six separate squares
        let m = cube_smart();
        let t = UvTopology::new(&m);
        assert_eq!(t.islands.len(), 6);
        assert_eq!(t.groups.len(), 24);
        // a plane grid: one island, shared corners are one UV vertex
        let grid = Primitive::Plane { size: [1.0, 1.0] }.build().unwrap().subdivide(false);
        let grid = unwrap(&grid, &[], UnwrapMethod::Planar, None, Some(1), None).unwrap();
        let t = UvTopology::new(&grid);
        assert_eq!(t.islands.len(), 1);
        assert_eq!(t.groups.len(), grid.positions.len());
        // sticky: the centre corner of one face drags the other three faces' corners along
        let centre = (0..grid.positions.len())
            .find(|&v| grid.faces.iter().filter(|f| f.contains(&(v as u32))).count() == 4)
            .unwrap();
        let f0 = grid.faces.iter().position(|f| f.contains(&(centre as u32))).unwrap();
        let k = grid.faces[f0].iter().position(|&v| v as usize == centre).unwrap();
        assert_eq!(t.sticky(&[[f0 as u32, k as u32]]).len(), 4);
        assert_eq!(t.island_faces(&[0]).len(), 4);
    }

    #[test]
    fn move_align_weld_pack() {
        let m = cube_smart();
        let one = pick(&m, &[2], &[]).unwrap();
        assert_eq!(one.len(), 4);
        // move one face, then line its left side up
        let moved = transform_corners(&m, &one, [0.1, 0.0], 0.0, [1.0, 1.0], None).unwrap();
        assert!((moved.uvs[2][0][0] - m.uvs[2][0][0] - 0.1).abs() < 1e-6);
        assert_eq!(moved.uvs[1], m.uvs[1]);
        let lined = align(&moved, &one, 0, AlignTo::Min).unwrap();
        let us: Vec<f32> = lined.uvs[2].iter().map(|u| u[0]).collect();
        assert!(us.iter().all(|u| (u - us[0]).abs() < 1e-6));
        // a quarter turn about the centre keeps the square in place
        let turned = transform_corners(&m, &one, [0.0; 2], 90.0, [1.0; 2], None).unwrap();
        let (a, b) = (bounds(&m, &one), bounds(&turned, &one));
        assert!((a.0 - b.0).length() < 1e-5 && (a.1 - b.1).length() < 1e-5);
        // weld all to one point
        let w = weld(&m, &one, false).unwrap();
        assert!(w.uvs[2].iter().all(|u| (Vec2::from_array(*u) - Vec2::from_array(w.uvs[2][0])).length() < 1e-6));
        // set exact coordinates
        let s = set_corners(&m, &[[0, 0]], &[[0.25, 0.75]]).unwrap();
        assert_eq!(s.uvs[0][0], [0.25, 0.75]);
        assert!(set_corners(&m, &[[0, 0]], &[]).is_err());
        assert!(pick(&m, &[], &[[0, 9]]).is_err());
        // pack: islands scattered far away come back inside the square without overlap
        let far = transform_corners(&m, &pick(&m, &[0], &[]).unwrap(), [3.0, 2.0], 0.0, [1.0; 2], None).unwrap();
        let packed = pack_islands(&far, &[], 0.02, true).unwrap();
        assert!(
            packed.uvs.iter().flatten().all(|u| (-1e-4..=1.0001).contains(&u[0]) && (-1e-4..=1.0001).contains(&u[1]))
        );
        let t = UvTopology::new(&packed);
        assert_eq!(t.islands.len(), 6);
        let boxes: Vec<(Vec2, Vec2)> = t.islands.iter().map(|f| bounds(&packed, &face_corners(&packed, f))).collect();
        for i in 0..boxes.len() {
            for j in i + 1..boxes.len() {
                let (a, b) = (boxes[i], boxes[j]);
                assert!(
                    !(a.0.x < b.1.x - 1e-4 && b.0.x < a.1.x - 1e-4 && a.0.y < b.1.y - 1e-4 && b.0.y < a.1.y - 1e-4)
                );
            }
        }
        assert_eq!(island_info(&packed).len(), 6);
    }

    #[test]
    fn stitch_by_vertex_joins_a_seam() {
        // two faces of a cube that share an edge, cut apart by smart unwrap: stitch them
        let m = cube_smart();
        let (f, g) =
            (0usize, (1..6).find(|&g| m.faces[g].iter().filter(|v| m.faces[0].contains(v)).count() == 2).unwrap());
        let shared: Vec<u32> = m.faces[g].iter().copied().filter(|v| m.faces[f].contains(v)).collect();
        let corners: Vec<Corner> = [f, g]
            .iter()
            .flat_map(|&x| {
                m.faces[x]
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| shared.contains(v))
                    .map(move |(k, _)| [x as u32, k as u32])
            })
            .collect();
        let st = weld(&m, &corners, true).unwrap();
        let t = UvTopology::new(&st);
        assert_eq!(t.island[f], t.island[g]);
    }
}
