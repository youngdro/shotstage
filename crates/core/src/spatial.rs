//! Spatial queries for placing things "like blocks": world bounds, ray casts against the
//! scene (with the ground plane as a fallback), resting an object on a surface, putting it
//! next to another one, and framing a subject with a camera by shot size and angle.
//!
//! Conventions: +Y is up, the ground is the plane y = 0, an object's front is its local +Z
//! and a camera looks down its local -Z.

use glam::{Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::camera::{CameraData, look_rotation};
use crate::eval::world_at;
use crate::scene::{EntityKind, Scene};
use crate::{CoreError, Result};

/// Axis-aligned box in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) / 2.0
    }
    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }
    pub fn half(&self) -> Vec3 {
        self.size() / 2.0
    }
    fn of_points(points: impl Iterator<Item = Vec3>) -> Option<Self> {
        let mut it = points.peekable();
        it.peek()?;
        let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for p in it {
            min = min.min(p);
            max = max.max(p);
        }
        Some(Self { min, max })
    }
    pub fn union(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(a), Some(b)) => Some(Self { min: a.min.min(b.min), max: a.max.max(b.max) }),
            (a, b) => a.or(b),
        }
    }
}

/// Is `id` hidden itself or through one of its ancestors?
pub fn is_hidden(scene: &Scene, id: &str) -> bool {
    let mut cur = Some(id);
    let mut guard = 0;
    while let Some(c) = cur {
        match scene.get(c) {
            Ok(e) if !e.visible => return true,
            Ok(e) => cur = e.parent.as_deref(),
            Err(_) => return true,
        }
        guard += 1;
        if guard > 10_000 {
            return true;
        }
    }
    false
}

/// `id` and every entity parented under it.
pub fn subtree(scene: &Scene, id: &str) -> Vec<String> {
    let mut out = vec![id.to_string()];
    let mut i = 0;
    while i < out.len() {
        let cur = out[i].clone();
        out.extend(scene.children(Some(&cur)).map(|e| e.id.clone()));
        i += 1;
        if out.len() > 100_000 {
            break;
        }
    }
    out
}

/// World bounds of one entity's own geometry (meshes only).
pub fn own_bounds(scene: &Scene, id: &str, frame: f32) -> Result<Option<Aabb>> {
    let e = scene.get(id)?;
    if !matches!(e.kind, EntityKind::Mesh { .. } | EntityKind::Image(_)) {
        return Ok(None);
    }
    let world = world_at(scene, id, frame)?;
    let tri = crate::eval::posed_tris(scene, id, frame)?;
    Ok(Aabb::of_points(tri.positions.iter().map(|p| world.transform_point3(Vec3::from_array(*p)))))
}

/// World bounds of an entity together with its children (what moves when it moves).
pub fn bounds(scene: &Scene, id: &str, frame: f32) -> Result<Option<Aabb>> {
    let mut acc = None;
    for e in subtree(scene, id) {
        acc = Aabb::union(acc, own_bounds(scene, &e, frame)?);
    }
    Ok(acc)
}

/// Where a ray met the scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    /// Entity hit, or `null` for the ground plane.
    pub id: Option<String>,
    pub point: [f32; 3],
    /// Surface normal facing the ray origin.
    pub normal: [f32; 3],
    pub distance: f32,
}

fn ray_triangle(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let (e1, e2) = (b - a, c - a);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 1e-6).then_some(t)
}

/// Slab test; `d` need not be normalised. Grown by a hair so faces on the box still count.
fn ray_hits_box(o: Vec3, d: Vec3, b: &Aabb) -> bool {
    let pad = Vec3::splat(1e-4 + 1e-4 * b.size().max_element());
    let (lo, hi) = (b.min - pad, b.max + pad);
    let (mut t0, mut t1) = (0.0f32, f32::INFINITY);
    for i in 0..3 {
        if d[i].abs() < 1e-12 {
            if o[i] < lo[i] || o[i] > hi[i] {
                return false;
            }
            continue;
        }
        let (a, b) = ((lo[i] - o[i]) / d[i], (hi[i] - o[i]) / d[i]);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
        if t0 > t1 {
            return false;
        }
    }
    true
}

/// Options for [`raycast`].
#[derive(Clone, Debug, Default)]
pub struct RayOptions<'a> {
    /// Entities (and their children) the ray passes through.
    pub ignore: &'a [String],
    /// Only these entities can be hit (empty = all visible meshes).
    pub only: &'a [String],
    /// Fall back to the ground plane y = 0 when no mesh is hit.
    pub ground: bool,
    /// The viewer, so billboard images are hit where they are drawn (turned to face it).
    pub eye: Option<Vec3>,
}

/// Closest visible mesh hit along a world-space ray.
pub fn raycast(scene: &Scene, frame: f32, origin: Vec3, dir: Vec3, opts: &RayOptions) -> Option<Hit> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO || !origin.is_finite() {
        return None;
    }
    let ignored: Vec<String> = opts.ignore.iter().flat_map(|i| subtree(scene, i)).collect();
    let mut best: Option<Hit> = None;
    for e in &scene.entities {
        if !matches!(e.kind, EntityKind::Mesh { .. } | EntityKind::Image(_)) {
            continue;
        }
        if ignored.contains(&e.id) || (!opts.only.is_empty() && !opts.only.contains(&e.id)) || is_hidden(scene, &e.id) {
            continue;
        }
        let (Ok(world), Ok(tri)) = (world_at(scene, &e.id, frame), crate::eval::posed_tris(scene, &e.id, frame)) else {
            continue;
        };
        let world = match &e.kind {
            EntityKind::Image(img) => crate::image::card_world(world, img.billboard, opts.eye),
            _ => world,
        };
        // test in the mesh's own space: no per-vertex transform, and a cheap box reject first
        let Some(local) = Aabb::of_points(tri.positions.iter().map(|p| Vec3::from_array(*p))) else { continue };
        let inv = world.inverse();
        let (lo, ld) = (inv.transform_point3(origin), inv.transform_vector3(dir));
        if !ray_hits_box(lo, ld, &local) {
            continue;
        }
        let normal_m = inv.transpose();
        for t in tri.indices.chunks(3) {
            let v = |k: usize| Vec3::from_array(tri.positions[t[k] as usize]);
            let (a, b, c) = (v(0), v(1), v(2));
            let Some(s) = ray_triangle(lo, ld, a, b, c) else { continue };
            let point = world.transform_point3(lo + ld * s);
            let dist = (point - origin).length();
            if best.as_ref().is_some_and(|h| h.distance <= dist) {
                continue;
            }
            let mut n = normal_m.transform_vector3((b - a).cross(c - a)).normalize_or_zero();
            if n.dot(dir) > 0.0 {
                n = -n;
            }
            best = Some(Hit { id: Some(e.id.clone()), point: point.to_array(), normal: n.to_array(), distance: dist });
        }
    }
    if best.is_none() && opts.ground && dir.y.abs() > 1e-6 {
        let t = -origin.y / dir.y;
        if t > 0.0 && t < 1e5 {
            let n = if origin.y >= 0.0 { Vec3::Y } else { Vec3::NEG_Y };
            best = Some(Hit { id: None, point: (origin + dir * t).to_array(), normal: n.to_array(), distance: t });
        }
    }
    best
}

fn snap(v: f32, step: Option<f32>) -> f32 {
    match step {
        Some(s) if s > 0.0 => (v / s).round() * s,
        _ => v,
    }
}

/// World position for `id`'s origin so its bounds rest against a surface at `contact` with
/// normal `normal`: the face of the bounds opposite the normal touches the contact point and
/// the footprint is centred on it (after `snap` on the other two axes).
pub fn rest_position(
    scene: &Scene,
    id: &str,
    frame: f32,
    contact: Vec3,
    normal: Vec3,
    snap_step: Option<f32>,
) -> Result<Vec3> {
    let origin = world_at(scene, id, frame)?.w_axis.truncate();
    let b = bounds(scene, id, frame)?.unwrap_or(Aabb { min: origin, max: origin });
    let (lo, hi) = (b.min - origin, b.max - origin);
    let mid = (lo + hi) / 2.0;
    let k = normal.abs().max_position();
    let mut out = Vec3::ZERO;
    for i in 0..3 {
        out[i] = if i == k {
            if normal[k] >= 0.0 { contact[i] - lo[i] } else { contact[i] - hi[i] }
        } else {
            snap(contact[i], snap_step) - mid[i]
        };
    }
    Ok(out)
}

/// Horizontal direction words for [`beside_position`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Side {
    Left,
    Right,
    Front,
    Back,
}

/// Unit horizontal direction for a side word. Without a view camera: right = +X, left = -X,
/// front = +Z, back = -Z. With one, left/right are as seen through it and front = towards it.
pub fn side_direction(scene: &Scene, side: Side, view: Option<&str>, frame: f32) -> Result<Vec3> {
    let (right, front) = match view {
        Some(cam) => {
            let w = world_at(scene, cam, frame)?;
            let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).normalize_or_zero();
            let r = flat(w.transform_vector3(Vec3::X));
            let f = flat(w.transform_vector3(Vec3::Z)); // camera looks down -Z, so +Z points back at it
            if r == Vec3::ZERO || f == Vec3::ZERO {
                return Err(CoreError::Invalid(format!("camera `{cam}` looks straight up or down")));
            }
            (r, f)
        }
        None => (Vec3::X, Vec3::Z),
    };
    Ok(match side {
        Side::Right => right,
        Side::Left => -right,
        Side::Front => front,
        Side::Back => -front,
    })
}

/// World position for `id`'s origin so it stands beside `target` on `side`, `gap` metres
/// apart, its bottom level with the target's bottom.
pub fn beside_position(scene: &Scene, id: &str, target: &str, dir: Vec3, gap: f32, frame: f32) -> Result<Vec3> {
    if subtree(scene, id).iter().any(|e| e == target) {
        return Err(CoreError::Invalid("cannot place an entity next to itself or its child".into()));
    }
    let origin = world_at(scene, id, frame)?.w_axis.truncate();
    let me = bounds(scene, id, frame)?.unwrap_or(Aabb { min: origin, max: origin });
    let t_origin = world_at(scene, target, frame)?.w_axis.truncate();
    let them = bounds(scene, target, frame)?.unwrap_or(Aabb { min: t_origin, max: t_origin });
    let support = |b: &Aabb| (b.half().x * dir.x).abs() + (b.half().z * dir.z).abs();
    let centre = them.center() + dir * (support(&them) + gap + support(&me));
    let rel_mid = me.center() - origin;
    Ok(Vec3::new(centre.x - rel_mid.x, them.min.y - (me.min.y - origin.y), centre.z - rel_mid.z))
}

/// Shot sizes, closest first. Values are the subject-height / frame-height ratio aimed for;
/// they sit inside the bands `describe_shot` uses, so framing then describing round-trips.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ShotSize {
    ExtremeCloseUp,
    CloseUp,
    MediumCloseUp,
    Medium,
    MediumFull,
    Full,
    Wide,
    ExtremeWide,
}

impl ShotSize {
    pub fn fraction(self) -> f32 {
        match self {
            ShotSize::ExtremeCloseUp => 9.0,
            ShotSize::CloseUp => 4.5,
            ShotSize::MediumCloseUp => 2.9,
            ShotSize::Medium => 2.0,
            ShotSize::MediumFull => 1.35,
            ShotSize::Full => 0.85,
            ShotSize::Wide => 0.4,
            ShotSize::ExtremeWide => 0.15,
        }
    }
}

/// Camera height relative to the subject.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ShotAngle {
    #[default]
    EyeLevel,
    High,
    Low,
    BirdsEye,
    WormsEye,
}

impl ShotAngle {
    /// Elevation of the camera above the aim point, degrees (pitch of the view is the negative).
    pub fn elevation_degrees(self) -> f32 {
        match self {
            ShotAngle::EyeLevel => 0.0,
            ShotAngle::High => 30.0,
            ShotAngle::Low => -25.0,
            ShotAngle::BirdsEye => 75.0,
            ShotAngle::WormsEye => -70.0,
        }
    }
}

/// Which side of the subject the camera stands on, relative to the subject's front (+Z).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Facing {
    #[default]
    Front,
    FrontLeft,
    Left,
    BackLeft,
    Back,
    BackRight,
    Right,
    FrontRight,
}

impl Facing {
    /// Degrees around +Y from the subject's front; positive turns towards the subject's left.
    pub fn azimuth_degrees(self) -> f32 {
        match self {
            Facing::Front => 0.0,
            Facing::FrontLeft => 45.0,
            Facing::Left => 90.0,
            Facing::BackLeft => 135.0,
            Facing::Back => 180.0,
            Facing::BackRight => -135.0,
            Facing::Right => -90.0,
            Facing::FrontRight => -45.0,
        }
    }
}

/// A camera pose that frames a subject.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Framing {
    pub eye: Vec3,
    pub rotation: Quat,
    pub aim: Vec3,
    pub distance: f32,
}

/// Camera pose that frames `subject` at `size` from `angle` and `azimuth_degrees` (relative to
/// the subject's front) with `lens`. Tight shots aim near the top of the subject (the head of
/// a standing figure); wide ones at its centre.
/// How big a subject reads in frame: its height, or its width across the view (seen along a
/// horizontal `right` axis) divided by the frame aspect, whichever is larger.
/// Share of the frame a box covers seen through a camera at `eye` with `rotation`: the larger
/// of its height share and its width share (1 = it just fills the frame).
pub fn frame_fraction(b: &Aabb, eye: Vec3, rotation: Quat, vfov: f32, aspect: f32) -> f32 {
    let inv = rotation.inverse();
    let t = (vfov / 2.0).tan().max(1e-4);
    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for i in 0..8 {
        let c = Vec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        );
        let p = inv * (c - eye);
        let z = (-p.z).max(1e-3);
        let (x, y) = (p.x / z / (t * aspect.max(0.1)), p.y / z / t);
        (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
    }
    ((x1 - x0) / 2.0).max((y1 - y0) / 2.0)
}

pub fn subject_extent(b: &Aabb, right: Vec3, aspect: f32) -> f32 {
    let s = b.size();
    let across = right.x.abs() * s.x + right.z.abs() * s.z;
    s.y.max(across / aspect.max(0.1)).max(0.01)
}

/// Top of a person's head (people have `<id>_pelvis` and `<id>_head` joints), if `id` is one.
pub fn head_top(scene: &Scene, id: &str, frame: f32) -> Option<f32> {
    let head = format!("{id}_head");
    if !scene.contains(&head) || !scene.contains(&format!("{id}_pelvis")) {
        return None;
    }
    match bounds(scene, &head, frame).ok().flatten() {
        // the head part (and hair) hangs off the joint
        Some(h) => Some(h.max.y),
        // a skinned body: the joint sits at the base of the skull
        None => Some(world_at(scene, &head, frame).ok()?.w_axis.y + 0.22),
    }
}

pub fn frame_subject(
    scene: &Scene,
    subject: &str,
    frame: f32,
    lens: &CameraData,
    size: ShotSize,
    angle: ShotAngle,
    azimuth_degrees: f32,
) -> Result<Framing> {
    let world = world_at(scene, subject, frame)?;
    let origin = world.w_axis.truncate();
    let mut b = bounds(scene, subject, frame)?
        .unwrap_or(Aabb { min: origin - Vec3::new(0.25, 0.85, 0.25), max: origin + Vec3::new(0.25, 0.85, 0.25) });
    // A person is framed on the head, not on a hand raised above it (reaching, waving).
    if let Some(top) = head_top(scene, subject, frame) {
        b.max.y = b.max.y.min(top).max(b.min.y + 0.3);
    }
    let frac = size.fraction();
    let front = {
        let f = world.transform_vector3(Vec3::Z);
        let f = Vec3::new(f.x, 0.0, f.z).normalize_or_zero();
        if f == Vec3::ZERO { Vec3::Z } else { f }
    };
    let dir_flat = Quat::from_rotation_y(azimuth_degrees.to_radians()) * front;
    // low, wide subjects (a podium, a car) are sized by their width across the frame
    let h = subject_extent(&b, Vec3::new(dir_flat.z, 0.0, -dir_flat.x), lens.aspect);
    let visible_h = h / frac;
    // Tight shots of people-sized subjects aim near the top (a close-up shows the head); small
    // products stay centred so a close-up shows the label.
    let aim_y = if frac > 1.0 && b.size().y > 1.0 {
        (b.max.y - visible_h * 0.42).max(b.min.y + visible_h * 0.5)
    } else {
        b.center().y
    };
    let center = b.center();
    let aim = Vec3::new(center.x, aim_y, center.z);
    let depth = visible_h / (2.0 * (lens.vfov() / 2.0).tan());
    let elev = angle.elevation_degrees().to_radians();
    let dir = dir_flat * elev.cos() + Vec3::Y * elev.sin();
    // Do not put the lens inside the subject: stay outside its bounds along the view line.
    let half = b.half();
    let support = (half.x * dir.x).abs() + (half.y * dir.y).abs() + (half.z * dir.z).abs();
    let look = |eye: Vec3| {
        look_rotation(eye, aim).unwrap_or_else(|_| {
            // Straight down / up: look along the subject's front axis as "up" on screen.
            Quat::from_mat4(&Mat4::look_at_rh(eye, aim, -dir_flat).inverse())
        })
    };
    // The view direction does not depend on the distance; find the distance at which the
    // subject's bounds cover `frac` of the frame (coverage shrinks as the camera backs off).
    let rotation0 = look(aim + dir);
    let cover = |d: f32| frame_fraction(&b, aim + dir * d, rotation0, lens.vfov(), lens.aspect);
    let near = (support + lens.near * 2.0).max(1e-3);
    let mut hi = (depth * 4.0).max(near * 2.0).max(1.0);
    while cover(hi) > frac && hi < 1e5 {
        hi *= 2.0;
    }
    let dist = if cover(near) <= frac {
        near
    } else {
        let mut lo = near;
        for _ in 0..60 {
            let mid = (lo * hi).sqrt();
            if cover(mid) > frac { lo = mid } else { hi = mid }
        }
        hi
    };
    let mut eye = aim + dir * dist;
    // Stay above what the subject stands on (the ground, a table, a board) for low angles: lift
    // the lens, keep the distance, re-aim. Below the table top the lens would see its underside.
    let ignore = [subject.to_string()];
    let opts = RayOptions { ignore: &ignore, ..Default::default() };
    let floor = raycast(scene, frame, Vec3::new(center.x, b.min.y + 0.01, center.z), Vec3::NEG_Y, &opts)
        .map(|h| h.point[1])
        .filter(|y| b.min.y - y < 0.05)
        .unwrap_or(0.0)
        .max(0.0);
    if eye.y < floor + 0.05 {
        eye.y = floor + 0.05;
    }
    if !eye.is_finite() {
        return Err(CoreError::Invalid("could not frame the subject".into()));
    }
    let rotation = look(eye);
    Ok(Framing { eye, rotation, aim, distance: (aim - eye).length() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::describe_shot;
    use crate::mesh::{MeshSource, Primitive};
    use crate::scene::{Entity, Transform};

    fn cube(id: &str, size: [f32; 3], pos: [f32; 3]) -> Entity {
        Entity {
            id: id.into(),
            name: id.into(),
            parent: None,
            transform: Transform { translation: pos, ..Default::default() },
            visible: true,
            locked: false,
            kind: EntityKind::Mesh {
                source: MeshSource::Primitive { primitive: Primitive::Cube { size } },
                modifiers: vec![],
                color: [0.5; 3],
                material: None,
            },
        }
    }

    fn scene() -> Scene {
        let mut s = Scene::default();
        s.insert(cube("table", [1.2, 0.75, 0.8], [2.0, 0.375, 0.0])).unwrap();
        s.insert(cube("box", [0.4, 0.4, 0.4], [0.0, 3.0, 0.0])).unwrap();
        s
    }

    #[test]
    fn ray_hits_mesh_then_ground() {
        let s = scene();
        let opts = RayOptions { ground: true, ..Default::default() };
        let hit = raycast(&s, 0.0, Vec3::new(2.0, 5.0, 0.0), Vec3::NEG_Y, &opts).unwrap();
        assert_eq!(hit.id.as_deref(), Some("table"));
        assert!((hit.point[1] - 0.75).abs() < 1e-4);
        assert_eq!(hit.normal, [0.0, 1.0, 0.0]);
        let ground = raycast(&s, 0.0, Vec3::new(-3.0, 5.0, 0.0), Vec3::NEG_Y, &opts).unwrap();
        assert_eq!(ground.id, None);
        let ignore = ["table".to_string()];
        let through = raycast(
            &s,
            0.0,
            Vec3::new(2.0, 5.0, 0.0),
            Vec3::NEG_Y,
            &RayOptions { ignore: &ignore, ground: true, ..opts },
        );
        assert_eq!(through.unwrap().id, None);
    }

    #[test]
    fn rests_on_surfaces_and_beside() {
        let s = scene();
        let p = rest_position(&s, "box", 0.0, Vec3::new(2.1, 0.75, 0.13), Vec3::Y, Some(0.25)).unwrap();
        assert!((p - Vec3::new(2.0, 0.95, 0.25)).length() < 1e-4, "{p}");
        let wall = rest_position(&s, "box", 0.0, Vec3::new(1.4, 0.3, 0.0), Vec3::NEG_X, None).unwrap();
        assert!((wall.x - 1.2).abs() < 1e-4, "{wall}");
        let side = beside_position(&s, "box", "table", Vec3::NEG_X, 0.1, 0.0).unwrap();
        assert!((side - Vec3::new(1.1, 0.2, 0.0)).length() < 1e-4, "{side}");
    }

    #[test]
    fn framing_round_trips_through_describe() {
        let mut s = Scene::default();
        s.insert(cube("actor", [0.5, 1.7, 0.3], [0.0, 0.85, 0.0])).unwrap();
        let lens = CameraData::default();
        for (size, word) in [
            (ShotSize::CloseUp, "close-up"),
            (ShotSize::Medium, "medium shot"),
            (ShotSize::Full, "full shot"),
            (ShotSize::Wide, "wide shot"),
        ] {
            let f = frame_subject(&s, "actor", 0.0, &lens, size, ShotAngle::EyeLevel, 0.0).unwrap();
            assert!(f.eye.z > 0.0, "camera in front of the subject");
            let mut cam = cube("cam", [1.0; 3], f.eye.to_array());
            cam.kind = EntityKind::Camera(lens);
            cam.transform.rotation = f.rotation.to_array();
            let mut s2 = s.clone();
            s2.insert(cam).unwrap();
            let d = describe_shot(&s2, "cam", 0.0, Some("actor")).unwrap();
            assert_eq!(d.shot_size.as_deref(), Some(word), "{size:?}");
            assert_eq!(d.angle, "eye level");
        }
        let high = frame_subject(&s, "actor", 0.0, &lens, ShotSize::Full, ShotAngle::High, 90.0).unwrap();
        assert!(high.eye.y > 1.0 && high.eye.x > 0.0, "{:?}", high.eye);
        let low = frame_subject(&s, "actor", 0.0, &lens, ShotSize::Wide, ShotAngle::WormsEye, 0.0).unwrap();
        assert!(low.eye.y >= 0.04);
    }

    #[test]
    fn low_angles_stay_above_the_table_the_subject_is_on() {
        let mut s = Scene::default();
        s.insert(cube("table", [1.2, 0.75, 0.8], [0.0, 0.375, 0.0])).unwrap();
        s.insert(cube("jar", [0.08, 0.1, 0.08], [0.0, 0.8, 0.0])).unwrap();
        let lens = CameraData::default();
        let low = frame_subject(&s, "jar", 0.0, &lens, ShotSize::Full, ShotAngle::Low, 0.0).unwrap();
        assert!(low.eye.y >= 0.79, "the lens is under the table top: {:?}", low.eye);
        // something floating over the table is not standing on it
        s.get_mut("jar").unwrap().transform.translation = [0.0, 1.6, 0.0];
        let low = frame_subject(&s, "jar", 0.0, &lens, ShotSize::Full, ShotAngle::Low, 0.0).unwrap();
        assert!(low.eye.y < 1.6, "{:?}", low.eye);
    }
}
