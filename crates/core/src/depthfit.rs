//! How the depth pass turns distance into grey.
//!
//! A plain near..far ramp over everything in frame leaves small things on a big set (fruit on
//! a table, a cup on a counter) a few grey levels apart: they span centimetres of a range that
//! runs metres to the floor's far edge. The auto fit here draws a small depth image on the CPU,
//! works out which things in frame are the set (the floor, a wall, a table top running off the
//! edges of the frame) and which are the subjects, and gives the subjects most of the grey
//! range. The set keeps the rest, so the order of everything stays right (near is lighter).

use glam::{Mat4, Vec3, Vec4};

use crate::eval::{posed_tris, world_at};
use crate::spatial::is_hidden;
use crate::{EntityKind, Scene};

/// Share of the grey range the subjects get when the fit finds them.
const FOCUS_SHARE: f32 = 0.7;

/// Distance (metres in front of the camera) to grey: white at `near`, black at `far`. With a
/// `focus` range, those distances get most of the greys and the rest of the range shares what
/// is left, in proportion to its length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthMap {
    pub near: f32,
    pub far: f32,
    pub focus: Option<[f32; 2]>,
}

impl DepthMap {
    pub fn linear(near: f32, far: f32) -> Self {
        Self { near, far, focus: None }
    }

    /// `[near, far]` or `[near, focusNear, focusFar, far]`, all increasing and >= 0.
    pub fn from_slice(v: &[f32]) -> Option<Self> {
        let ok = v.iter().all(|x| x.is_finite() && *x >= 0.0);
        match *v {
            [n, f] if ok && f > n => Some(Self::linear(n, f)),
            [n, a, b, f] if ok && n <= a && a < b && b <= f && f > n => {
                Some(Self { near: n, far: f, focus: Some([a, b]) }.tidy())
            }
            _ => None,
        }
    }

    /// The JSON form: `[near, far]`, or `[near, focusNear, focusFar, far]`.
    pub fn to_vec(&self) -> Vec<f32> {
        match self.focus {
            Some([a, b]) => vec![self.near, a, b, self.far],
            None => vec![self.near, self.far],
        }
    }

    /// The four numbers the shader's curve takes besides near and far: focus start, focus end,
    /// grey lost before the focus starts and grey left after it ends.
    pub fn curve(&self) -> [f32; 4] {
        match self.focus {
            Some([a, b]) => {
                let (before, after) = ((a - self.near).max(0.0), (self.far - b).max(0.0));
                let rest = 1.0 - FOCUS_SHARE;
                let total = before + after;
                let (lo, hi) = if total > 1e-6 { (rest * before / total, rest * after / total) } else { (0.0, 0.0) };
                [a, b, lo, hi]
            }
            None => [self.near, self.far, 0.0, 0.0],
        }
    }

    /// Grey for a distance, 1 = white (near) .. 0 = black (far). The shader does the same.
    pub fn grey(&self, d: f32) -> f32 {
        let [a, b, lo, hi] = self.curve();
        let t = |x: f32, y: f32| ((d - x) / (y - x).max(1e-5)).clamp(0.0, 1.0);
        if d <= a {
            1.0 - lo * t(self.near, a)
        } else if d <= b {
            let s = t(a, b);
            (1.0 - lo) * (1.0 - s) + hi * s
        } else {
            hi * (1.0 - t(b, self.far))
        }
    }

    /// One map that covers both (a sequence shares one, so its greys do not flicker).
    pub fn union(self, o: Self) -> Self {
        let focus = match (self.focus, o.focus) {
            (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].max(b[1])]),
            (a, b) => a.or(b),
        };
        Self { near: self.near.min(o.near), far: self.far.max(o.far), focus }.tidy()
    }

    /// Drop a focus that covers most of the range anyway (plain linear reads better then).
    fn tidy(mut self) -> Self {
        if let Some([a, b]) = self.focus
            && (b - a) > 0.6 * (self.far - self.near)
        {
            self.focus = None;
        }
        self
    }
}

impl serde::Serialize for DepthMap {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_vec().serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for DepthMap {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Vec::<f32>::deserialize(d)?;
        Self::from_slice(&v).ok_or_else(|| {
            serde::de::Error::custom(
                "a depth range is [near, far] or [near, focusNear, focusFar, far], increasing, from 0",
            )
        })
    }
}

impl Default for DepthMap {
    fn default() -> Self {
        Self::linear(0.0, 10.0)
    }
}

/// Width of the CPU depth image the fit looks at.
const FIT_W: usize = 160;

/// Fit a depth map to what a camera sees. `tris` are world-space triangles, each with a key
/// that groups the parts of one thing (a table's top and legs share it). Nothing in frame
/// gives the default 0..10 m.
pub fn fit(tris: impl IntoIterator<Item = (u32, [Vec3; 3])>, view: Mat4, proj: Mat4) -> DepthMap {
    // keep the picture's shape: proj's x scale is its y scale over the aspect
    let aspect = (proj.y_axis.y / proj.x_axis.x).abs();
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 };
    let (w, h) = (FIT_W, ((FIT_W as f32 / aspect).round() as usize).clamp(8, FIT_W * 2));
    let mut depth = vec![f32::INFINITY; w * h];
    let mut key = vec![0u32; w * h];
    let vp = proj * view;
    for (k, tri) in tris {
        // clip space, and distance in front of the camera
        let pts: Vec<(Vec4, f32)> = tri.iter().map(|p| (vp * p.extend(1.0), -view.transform_point3(*p).z)).collect();
        // drop what lies wholly outside one side of the frame
        let out = |f: &dyn Fn(Vec4) -> bool| pts.iter().all(|(c, _)| f(*c));
        if out(&|c| c.x > c.w)
            || out(&|c| c.x < -c.w)
            || out(&|c| c.y > c.w)
            || out(&|c| c.y < -c.w)
            || out(&|c| c.z < 0.0)
        {
            continue;
        }
        let poly = clip_near(&pts);
        for i in 1..poly.len().saturating_sub(1) {
            raster(&[poly[0], poly[i], poly[i + 1]], (w, h), k, &mut depth, &mut key);
        }
    }
    from_image(&depth, &key, (w, h))
}

/// Cut a polygon to the part in front of the near plane (clip z >= 0).
fn clip_near(pts: &[(Vec4, f32)]) -> Vec<(Vec4, f32)> {
    let mut out = Vec::with_capacity(4);
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        let (da, db) = (a.0.z, b.0.z);
        if da >= 0.0 {
            out.push(a);
        }
        if (da >= 0.0) != (db >= 0.0) {
            let t = da / (da - db);
            out.push((a.0.lerp(b.0, t), a.1 + (b.1 - a.1) * t));
        }
    }
    out
}

/// Draw one triangle into the depth image (nearest wins), perspective-correct.
fn raster(tri: &[(Vec4, f32); 3], (w, h): (usize, usize), k: u32, depth: &mut [f32], key: &mut [u32]) {
    let s: Vec<(f32, f32, f32, f32)> = tri
        .iter()
        .map(|(c, d)| {
            let iw = 1.0 / c.w.max(1e-6);
            ((c.x * iw * 0.5 + 0.5) * w as f32, (0.5 - c.y * iw * 0.5) * h as f32, iw, d * iw)
        })
        .collect();
    let area = (s[1].0 - s[0].0) * (s[2].1 - s[0].1) - (s[1].1 - s[0].1) * (s[2].0 - s[0].0);
    if !area.is_finite() || area.abs() < 1e-9 {
        return;
    }
    let lo = |f: fn(&(f32, f32, f32, f32)) -> f32, n: usize| {
        (s.iter().map(f).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize).min(n)
    };
    let hi = |f: fn(&(f32, f32, f32, f32)) -> f32, n: usize| {
        (s.iter().map(f).fold(f32::NEG_INFINITY, f32::max).ceil().max(0.0) as usize).min(n)
    };
    let (x0, x1, y0, y1) = (lo(|p| p.0, w), hi(|p| p.0, w), lo(|p| p.1, h), hi(|p| p.1, h));
    let edge = |a: usize, b: usize, x: f32, y: f32| (s[b].0 - s[a].0) * (y - s[a].1) - (s[b].1 - s[a].1) * (x - s[a].0);
    for py in y0..y1 {
        for px in x0..x1 {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let (w0, w1, w2) = (edge(1, 2, x, y) / area, edge(2, 0, x, y) / area, edge(0, 1, x, y) / area);
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let iw = w0 * s[0].2 + w1 * s[1].2 + w2 * s[2].2;
            let d = (w0 * s[0].3 + w1 * s[1].3 + w2 * s[2].3) / iw.max(1e-12);
            let i = py * w + px;
            if d.is_finite() && d < depth[i] {
                depth[i] = d;
                key[i] = k;
            }
        }
    }
}

/// From the depth image to a map: the whole range of what is seen, and the range of the
/// subjects when the set (things running off two edges of the frame, or filling a big part of
/// it) leaves any.
fn from_image(depth: &[f32], key: &[u32], (w, h): (usize, usize)) -> DepthMap {
    use std::collections::HashMap;
    let mut seen: HashMap<u32, (usize, u8)> = HashMap::new();
    let (mut near, mut far) = (f32::INFINITY, 0.0f32);
    for (i, d) in depth.iter().enumerate() {
        if !d.is_finite() {
            continue;
        }
        (near, far) = (near.min(*d), far.max(*d));
        let (x, y) = (i % w, i / w);
        let e = seen.entry(key[i]).or_default();
        e.0 += 1;
        e.1 |= u8::from(x == 0) | u8::from(x == w - 1) << 1 | u8::from(y == 0) << 2 | u8::from(y == h - 1) << 3;
    }
    if !near.is_finite() || far - near < 1e-4 {
        return if near.is_finite() {
            DepthMap::linear((near - 0.5).max(0.0), near + 0.5)
        } else {
            DepthMap::default()
        };
    }
    let set = |k: &u32| seen.get(k).is_some_and(|(n, edges)| edges.count_ones() >= 2 || *n * 100 > w * h * 35);
    let (mut a, mut b, mut count) = (f32::INFINITY, 0.0f32, 0usize);
    for (d, k) in depth.iter().zip(key) {
        if d.is_finite() && !set(k) {
            (a, b, count) = (a.min(*d), b.max(*d), count + 1);
        }
    }
    let mut map = DepthMap::linear(near, far);
    if count >= (w * h / 2000).max(4) && b > a {
        // a margin, so the subjects' own front and back are not pressed to the ends
        let pad = ((b - a) * 0.15).max(0.01);
        map.focus = Some([(a - pad).max(near), (b + pad).min(far)]);
        map = map.tidy();
    }
    map
}

/// The top-level thing each entity belongs to (its root ancestor's index), by entity index.
pub fn root_keys(scene: &Scene) -> Vec<u32> {
    let index: std::collections::HashMap<&str, usize> =
        scene.entities.iter().enumerate().map(|(i, e)| (e.id.as_str(), i)).collect();
    (0..scene.entities.len())
        .map(|mut i| {
            for _ in 0..10_000 {
                match scene.entities[i].parent.as_deref().and_then(|p| index.get(p)) {
                    Some(&p) => i = p,
                    None => break,
                }
            }
            i as u32
        })
        .collect()
}

/// Every visible mesh and picture of `scene` at `frame` as world triangles keyed by their root
/// (for hosts without the renderer's geometry at hand, such as a sequence plan).
pub fn scene_tris(scene: &Scene, frame: f32) -> Vec<(u32, [Vec3; 3])> {
    let roots = root_keys(scene);
    let mut out = vec![];
    for (i, e) in scene.entities.iter().enumerate() {
        if is_hidden(scene, &e.id) {
            continue;
        }
        let Ok(world) = world_at(scene, &e.id, frame) else { continue };
        let k = roots[i];
        match &e.kind {
            EntityKind::Mesh { .. } => {
                let Ok(t) = posed_tris(scene, &e.id, frame) else { continue };
                for c in t.indices.chunks_exact(3) {
                    let p = |j: u32| world.transform_point3(Vec3::from_array(t.positions[j as usize]));
                    out.push((k, [p(c[0]), p(c[1]), p(c[2])]));
                }
            }
            EntityKind::Image(img) => {
                let (x, y) = (img.size[0] / 2.0, img.size[1] / 2.0);
                let p = |a: f32, b: f32| world.transform_point3(Vec3::new(a, b, 0.0));
                out.push((k, [p(-x, -y), p(x, -y), p(x, y)]));
                out.push((k, [p(-x, -y), p(x, y), p(-x, y)]));
            }
            _ => {}
        }
    }
    out
}

/// The auto depth map for a camera at `frame`.
pub fn scene_fit(scene: &Scene, frame: f32, view: Mat4, proj: Mat4) -> DepthMap {
    fit(scene_tris(scene, frame), view, proj)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(k: u32, y: f32, half: f32, cx: f32, cz: f32) -> [(u32, [Vec3; 3]); 2] {
        let p = |x: f32, z: f32| Vec3::new(cx + x, y, cz + z);
        [(k, [p(-half, -half), p(half, -half), p(half, half)]), (k, [p(-half, -half), p(half, half), p(-half, half)])]
    }

    fn cube(k: u32, c: Vec3, s: f32) -> Vec<(u32, [Vec3; 3])> {
        // a box as its six faces
        let mut out = vec![];
        let corners: Vec<Vec3> = (0..8)
            .map(|i| {
                c + Vec3::new(
                    if i & 1 == 0 { -s } else { s },
                    if i & 2 == 0 { -s } else { s },
                    if i & 4 == 0 { -s } else { s },
                )
            })
            .collect();
        for f in [[0, 1, 3, 2], [4, 5, 7, 6], [0, 1, 5, 4], [2, 3, 7, 6], [0, 2, 6, 4], [1, 3, 7, 5]] {
            out.push((k, [corners[f[0]], corners[f[1]], corners[f[2]]]));
            out.push((k, [corners[f[0]], corners[f[2]], corners[f[3]]]));
        }
        out
    }

    fn cam() -> (Mat4, Mat4) {
        // 3 m back and 1.5 m up, looking at the origin
        let view = Mat4::look_at_rh(Vec3::new(0.0, 1.5, 3.0), Vec3::ZERO, Vec3::Y);
        (view, Mat4::perspective_rh(40f32.to_radians(), 16.0 / 9.0, 0.05, 500.0))
    }

    #[test]
    fn small_things_on_a_big_floor_get_most_of_the_greys() {
        let (view, proj) = cam();
        let mut tris: Vec<_> = quad(0, 0.0, 20.0, 0.0, 0.0).to_vec();
        tris.extend(cube(1, Vec3::new(-0.15, 0.04, 0.0), 0.04));
        tris.extend(cube(2, Vec3::new(0.15, 0.04, 0.12), 0.04));
        let m = fit(tris, view, proj);
        let [a, b] = m.focus.expect("the boxes are the subjects");
        let dist = |p: Vec3| -view.transform_point3(p).z;
        assert!(a < dist(Vec3::new(0.15, 0.04, 0.12)) && b > dist(Vec3::new(-0.15, 0.04, 0.0)), "{m:?}");
        assert!(b - a < 1.0, "focus {a}..{b}");
        // the two boxes, ~12 cm apart in depth, now differ by many grey levels
        let (g1, g2) = (m.grey(dist(Vec3::new(-0.15, 0.04, 0.04))), m.grey(dist(Vec3::new(0.15, 0.04, 0.16))));
        assert!((g2 - g1) * 255.0 > 20.0, "{g1} vs {g2}");
        let plain = DepthMap::linear(0.0, m.far);
        let (p1, p2) = (plain.grey(dist(Vec3::new(-0.15, 0.04, 0.04))), plain.grey(dist(Vec3::new(0.15, 0.04, 0.16))));
        assert!((g2 - g1) > 4.0 * (p2 - p1));
        // still in order front to back, white near
        assert!(m.grey(m.near) > 0.99 && m.grey(m.far) < 0.01);
        let mut last = 2.0;
        for i in 0..=100 {
            let g = m.grey(m.near + (m.far - m.near) * i as f32 / 100.0);
            assert!(g <= last + 1e-6);
            last = g;
        }
    }

    #[test]
    fn one_thing_alone_is_a_plain_ramp() {
        let (view, proj) = cam();
        let m = fit(cube(0, Vec3::ZERO, 0.5), view, proj);
        assert!(m.focus.is_none());
        assert!(m.near > 2.0 && m.far < 4.0, "{m:?}");
        assert_eq!(fit(Vec::new(), view, proj), DepthMap::default());
    }

    #[test]
    fn slices_and_unions() {
        assert_eq!(DepthMap::from_slice(&[1.0, 5.0]), Some(DepthMap::linear(1.0, 5.0)));
        let m = DepthMap::from_slice(&[1.0, 2.0, 2.5, 9.0]).unwrap();
        assert_eq!(m.to_vec(), vec![1.0, 2.0, 2.5, 9.0]);
        assert!(DepthMap::from_slice(&[5.0, 1.0]).is_none());
        assert!(DepthMap::from_slice(&[1.0, 3.0, 2.0, 9.0]).is_none());
        // a focus over most of the range is dropped
        assert!(DepthMap::from_slice(&[0.0, 0.5, 9.5, 10.0]).unwrap().focus.is_none());
        let u = m.union(DepthMap::from_slice(&[0.5, 2.2, 3.0, 8.0]).unwrap());
        assert_eq!(u.to_vec(), vec![0.5, 2.0, 3.0, 9.0]);
        assert!((DepthMap::linear(2.0, 4.0).grey(3.0) - 0.5).abs() < 1e-6);
    }
}
