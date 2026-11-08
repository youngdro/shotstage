//! Boolean operations (union, difference, intersection) on closed polygon meshes, with a BSP
//! tree in the manner of Evan Wallace's csg.js. Works in f64; the trees are kept in an arena
//! and walked with explicit stacks so large meshes cannot overflow the (small wasm) stack.
//!
//! The result is welded by position. Cut faces can leave T-junctions where a split face meets
//! an unsplit neighbour; they render fine and `mesh.mergeByDistance` keeps them tidy.

use std::collections::HashMap;

use glam::{DMat4, DVec3, Mat4};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;
use crate::{CoreError, Result};

const EPS: f64 = 1e-5;
/// Largest input, in faces of both meshes together.
pub const MAX_BOOLEAN_FACES: usize = 40_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BoolOp {
    /// Both solids merged into one.
    Union,
    /// The first solid with the second carved out of it.
    #[default]
    Difference,
    /// Only where the two overlap.
    Intersect,
}

#[derive(Clone, Copy)]
struct Plane {
    n: DVec3,
    w: f64,
}

impl Plane {
    fn flip(self) -> Self {
        Self { n: -self.n, w: -self.w }
    }
}

#[derive(Clone)]
struct Poly {
    verts: Vec<DVec3>,
    plane: Plane,
}

impl Poly {
    fn new(verts: Vec<DVec3>) -> Option<Self> {
        let mut n = DVec3::ZERO;
        for i in 0..verts.len() {
            let a = verts[i];
            let b = verts[(i + 1) % verts.len()];
            n += DVec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
        }
        if verts.len() < 3 || n.length_squared() < 1e-20 {
            return None;
        }
        let n = n.normalize();
        let w = n.dot(verts[0]);
        Some(Self { verts, plane: Plane { n, w } })
    }
    fn flip(&mut self) {
        self.verts.reverse();
        self.plane = self.plane.flip();
    }
}

const COPLANAR: u8 = 0;
const FRONT: u8 = 1;
const BACK: u8 = 2;
const SPANNING: u8 = 3;

/// Sort `p` by `plane`: coplanar ones go to `cf`/`cb` by facing, the rest to `f`/`b`, split
/// if they straddle.
fn split(plane: Plane, p: Poly, cf: &mut Vec<Poly>, cb: &mut Vec<Poly>, f: &mut Vec<Poly>, b: &mut Vec<Poly>) {
    let mut all = 0u8;
    let types: Vec<u8> = p
        .verts
        .iter()
        .map(|v| {
            let d = plane.n.dot(*v) - plane.w;
            let t = if d < -EPS {
                BACK
            } else if d > EPS {
                FRONT
            } else {
                COPLANAR
            };
            all |= t;
            t
        })
        .collect();
    match all {
        COPLANAR => {
            if plane.n.dot(p.plane.n) > 0.0 {
                cf.push(p)
            } else {
                cb.push(p)
            }
        }
        FRONT => f.push(p),
        BACK => b.push(p),
        _ => {
            let n = p.verts.len();
            let (mut fv, mut bv) = (Vec::with_capacity(n + 1), Vec::with_capacity(n + 1));
            for i in 0..n {
                let j = (i + 1) % n;
                let (ti, tj) = (types[i], types[j]);
                let (vi, vj) = (p.verts[i], p.verts[j]);
                if ti != BACK {
                    fv.push(vi);
                }
                if ti != FRONT {
                    bv.push(vi);
                }
                if (ti | tj) == SPANNING {
                    let t = (plane.w - plane.n.dot(vi)) / plane.n.dot(vj - vi);
                    let v = vi.lerp(vj, t);
                    fv.push(v);
                    bv.push(v);
                }
            }
            if fv.len() >= 3 {
                f.push(Poly { verts: fv, plane: p.plane });
            }
            if bv.len() >= 3 {
                b.push(Poly { verts: bv, plane: p.plane });
            }
        }
    }
}

struct Node {
    plane: Plane,
    front: Option<usize>,
    back: Option<usize>,
    polys: Vec<Poly>,
}

#[derive(Default)]
struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    fn new(polys: Vec<Poly>) -> Self {
        let mut t = Self::default();
        t.build(polys);
        t
    }

    fn new_node(&mut self, polys: &[Poly]) -> usize {
        // a splitting plane from the middle of the list balances a little better than the first
        self.nodes.push(Node { plane: polys[polys.len() / 2].plane, front: None, back: None, polys: Vec::new() });
        self.nodes.len() - 1
    }

    fn build(&mut self, polys: Vec<Poly>) {
        if polys.is_empty() {
            return;
        }
        if self.nodes.is_empty() {
            self.new_node(&polys);
        }
        let mut stack = vec![(0usize, polys)];
        while let Some((ni, polys)) = stack.pop() {
            let plane = self.nodes[ni].plane;
            let (mut cf, mut cb, mut f, mut b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for p in polys {
                split(plane, p, &mut cf, &mut cb, &mut f, &mut b);
            }
            self.nodes[ni].polys.extend(cf);
            self.nodes[ni].polys.extend(cb);
            if !f.is_empty() {
                let c = match self.nodes[ni].front {
                    Some(c) => c,
                    None => {
                        let c = self.new_node(&f);
                        self.nodes[ni].front = Some(c);
                        c
                    }
                };
                stack.push((c, f));
            }
            if !b.is_empty() {
                let c = match self.nodes[ni].back {
                    Some(c) => c,
                    None => {
                        let c = self.new_node(&b);
                        self.nodes[ni].back = Some(c);
                        c
                    }
                };
                stack.push((c, b));
            }
        }
    }

    /// Solid space becomes empty space and the other way round.
    fn invert(&mut self) {
        for n in &mut self.nodes {
            for p in &mut n.polys {
                p.flip();
            }
            n.plane = n.plane.flip();
            std::mem::swap(&mut n.front, &mut n.back);
        }
    }

    /// Remove the parts of `polys` inside this tree's solid.
    fn clip_polygons(&self, polys: Vec<Poly>) -> Vec<Poly> {
        if self.nodes.is_empty() {
            return polys;
        }
        let mut out = Vec::new();
        let mut stack = vec![(0usize, polys)];
        while let Some((ni, polys)) = stack.pop() {
            let node = &self.nodes[ni];
            let (mut f, mut b) = (Vec::new(), Vec::new());
            let (mut cf, mut cb) = (Vec::new(), Vec::new());
            for p in polys {
                split(node.plane, p, &mut cf, &mut cb, &mut f, &mut b);
            }
            f.extend(cf);
            b.extend(cb);
            match node.front {
                Some(c) => stack.push((c, f)),
                None => out.extend(f),
            }
            if let Some(c) = node.back {
                stack.push((c, b));
            }
        }
        out
    }

    fn clip_to(&mut self, other: &Tree) {
        for i in 0..self.nodes.len() {
            let polys = std::mem::take(&mut self.nodes[i].polys);
            self.nodes[i].polys = other.clip_polygons(polys);
        }
    }

    fn all(&self) -> Vec<Poly> {
        self.nodes.iter().flat_map(|n| n.polys.iter().cloned()).collect()
    }
}

fn to_polys(m: &Mesh, xf: DMat4) -> Vec<Poly> {
    let pts: Vec<DVec3> =
        m.positions.iter().map(|p| xf.transform_point3(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))).collect();
    m.faces.iter().filter_map(|f| Poly::new(f.iter().map(|&v| pts[v as usize]).collect())).collect()
}

fn to_mesh(polys: Vec<Poly>) -> Mesh {
    let mut m = Mesh::default();
    let mut index: HashMap<[i64; 3], u32> = HashMap::new();
    let q = 1.0 / (EPS * 10.0);
    for p in polys {
        let mut face: Vec<u32> = Vec::with_capacity(p.verts.len());
        for v in p.verts {
            let key = [(v.x * q).round() as i64, (v.y * q).round() as i64, (v.z * q).round() as i64];
            let i = *index.entry(key).or_insert_with(|| {
                m.positions.push([v.x as f32, v.y as f32, v.z as f32]);
                m.positions.len() as u32 - 1
            });
            if face.last() != Some(&i) {
                face.push(i);
            }
        }
        while face.len() > 1 && face.first() == face.last() {
            face.pop();
        }
        let mut uniq = face.clone();
        uniq.sort_unstable();
        uniq.dedup();
        if uniq.len() >= 3 {
            m.faces.push(face);
        }
    }
    m
}

/// Insert the vertices that lie on the middle of an edge into the faces using that edge, so
/// split faces and their unsplit neighbours share vertices (no T-junctions).
fn fix_t_junctions(m: &mut Mesh) {
    if m.positions.is_empty() {
        return;
    }
    let pts: Vec<DVec3> = m.positions.iter().map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect();
    let (mut lo, mut hi) = (pts[0], pts[0]);
    for p in &pts {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    // a grid with about one vertex per cell
    let ext = (hi - lo).max(DVec3::splat(1e-6));
    let cell = (ext.x * ext.y * ext.z / pts.len() as f64).cbrt().max(ext.max_element() / 256.0).max(1e-6);
    let key = |p: DVec3| -> [i64; 3] {
        let c = ((p - lo) / cell).floor();
        [c.x as i64, c.y as i64, c.z as i64]
    };
    let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    for (i, p) in pts.iter().enumerate() {
        grid.entry(key(*p)).or_default().push(i as u32);
    }
    let tol = EPS * 10.0;
    let mut cache: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for f in &mut m.faces {
        let n = f.len();
        let mut g = Vec::with_capacity(n);
        for i in 0..n {
            let (a, b) = (f[i], f[(i + 1) % n]);
            g.push(a);
            let k = (a.min(b), a.max(b));
            let on = cache.entry(k).or_insert_with(|| {
                let (pa, pb) = (pts[k.0 as usize], pts[k.1 as usize]);
                let d = pb - pa;
                let len2 = d.length_squared();
                if len2 < 1e-18 {
                    return Vec::new();
                }
                let (c0, c1) = (key(pa.min(pb) - DVec3::splat(tol)), key(pa.max(pb) + DVec3::splat(tol)));
                let cells = (c1[0] - c0[0] + 1) * (c1[1] - c0[1] + 1) * (c1[2] - c0[2] + 1);
                if cells > 4096 {
                    return Vec::new();
                }
                let mut hits: Vec<(f64, u32)> = Vec::new();
                for x in c0[0]..=c1[0] {
                    for y in c0[1]..=c1[1] {
                        for z in c0[2]..=c1[2] {
                            for &v in grid.get(&[x, y, z]).map(Vec::as_slice).unwrap_or(&[]) {
                                if v == k.0 || v == k.1 {
                                    continue;
                                }
                                let t = (pts[v as usize] - pa).dot(d) / len2;
                                if t <= 1e-9 || t >= 1.0 - 1e-9 {
                                    continue;
                                }
                                if (pa + d * t).distance_squared(pts[v as usize]) < tol * tol {
                                    hits.push((t, v));
                                }
                            }
                        }
                    }
                }
                hits.sort_by(|x, y| x.0.total_cmp(&y.0));
                hits.into_iter().map(|h| h.1).collect()
            });
            if a == k.0 {
                g.extend(on.iter().copied());
            } else {
                g.extend(on.iter().rev().copied());
            }
        }
        *f = g;
    }
}

/// `a` combined with `b`, where `b_to_a` places `b` in `a`'s space. Both should be closed
/// solids; the result is in `a`'s space.
pub fn boolean(a: &Mesh, b: &Mesh, b_to_a: Mat4, op: BoolOp) -> Result<Mesh> {
    let total = a.faces.len() + b.faces.len();
    if total > MAX_BOOLEAN_FACES {
        return Err(CoreError::Invalid(format!(
            "the two meshes have {total} faces together; booleans take at most {MAX_BOOLEAN_FACES}"
        )));
    }
    if !b_to_a.is_finite() {
        return Err(CoreError::Invalid("the second mesh's placement is not finite".into()));
    }
    let mut ta = Tree::new(to_polys(a, DMat4::IDENTITY));
    let mut tb = Tree::new(to_polys(b, b_to_a.as_dmat4()));
    match op {
        BoolOp::Union => {
            ta.clip_to(&tb);
            tb.clip_to(&ta);
            tb.invert();
            tb.clip_to(&ta);
            tb.invert();
            ta.build(tb.all());
        }
        BoolOp::Difference => {
            ta.invert();
            ta.clip_to(&tb);
            tb.clip_to(&ta);
            tb.invert();
            tb.clip_to(&ta);
            tb.invert();
            ta.build(tb.all());
            ta.invert();
        }
        BoolOp::Intersect => {
            ta.invert();
            tb.clip_to(&ta);
            tb.invert();
            ta.clip_to(&tb);
            tb.clip_to(&ta);
            ta.build(tb.all());
            ta.invert();
        }
    }
    let mut out = to_mesh(ta.all());
    fix_t_junctions(&mut out);
    if out.faces.is_empty() {
        return Err(CoreError::Invalid(match op {
            BoolOp::Intersect => "the meshes do not overlap: the intersection is empty".into(),
            _ => "the result is empty".into(),
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Primitive;

    fn cube(s: f32) -> Mesh {
        Primitive::Cube { size: [s; 3] }.build().unwrap()
    }
    fn bounds(m: &Mesh) -> (DVec3, DVec3) {
        let mut lo = DVec3::splat(f64::MAX);
        let mut hi = DVec3::splat(f64::MIN);
        for p in &m.positions {
            let v = DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        (lo, hi)
    }
    fn closed(m: &Mesh) -> bool {
        let mut half: HashMap<(u32, u32), u32> = HashMap::new();
        for f in &m.faces {
            for i in 0..f.len() {
                *half.entry((f[i], f[(i + 1) % f.len()])).or_default() += 1;
            }
        }
        half.iter().all(|(&(a, b), &c)| c == 1 && half.get(&(b, a)) == Some(&1))
    }
    /// Signed volume by the divergence theorem (fan triangulation).
    fn volume(m: &Mesh) -> f64 {
        let p = |v: u32| {
            DVec3::new(
                m.positions[v as usize][0] as f64,
                m.positions[v as usize][1] as f64,
                m.positions[v as usize][2] as f64,
            )
        };
        let mut vol = 0.0;
        for f in &m.faces {
            for i in 1..f.len() - 1 {
                vol += p(f[0]).dot(p(f[i]).cross(p(f[i + 1])));
            }
        }
        vol / 6.0
    }

    #[test]
    fn cube_booleans() {
        let a = cube(2.0); // -1..1
        let b = cube(2.0);
        let shift = Mat4::from_translation(glam::Vec3::new(1.0, 0.0, 0.0));
        let u = boolean(&a, &b, shift, BoolOp::Union).unwrap();
        assert!(closed(&u), "union is not watertight");
        let (lo, hi) = bounds(&u);
        assert!((lo.x + 1.0).abs() < 1e-4 && (hi.x - 2.0).abs() < 1e-4);
        assert!((volume(&u) - 12.0).abs() < 1e-3, "union volume {}", volume(&u));
        let d = boolean(&a, &b, shift, BoolOp::Difference).unwrap();
        assert!((volume(&d) - 4.0).abs() < 1e-3, "difference volume {}", volume(&d));
        let i = boolean(&a, &b, shift, BoolOp::Intersect).unwrap();
        assert!((volume(&i) - 4.0).abs() < 1e-3, "intersection volume {}", volume(&i));
        let far = Mat4::from_translation(glam::Vec3::new(5.0, 0.0, 0.0));
        assert!(boolean(&a, &b, far, BoolOp::Intersect).is_err());
        // a hole through the middle
        let rod = Primitive::Cylinder { radius: 0.4, height: 4.0, segments: 16 }.build().unwrap();
        let h = boolean(&a, &rod, Mat4::IDENTITY, BoolOp::Difference).unwrap();
        assert!((volume(&h) - 7.02).abs() < 0.01, "hole volume {}", volume(&h));
        h.validate().unwrap();
        assert!(closed(&h), "hole is not watertight");
    }
}
