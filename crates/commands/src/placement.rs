//! Block-style placement and shot framing: the commands behind drag-and-drop in the editor,
//! also used directly by agents. Objects rest on surfaces, stand next to each other and get
//! framed by shot size instead of by raw coordinates.

use glam::{Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::eval::{describe_shot, local_at, world_at};
use shotstage_core::spatial::{
    self, Facing, RayOptions, ShotAngle, ShotSize, Side, beside_position, frame_subject, raycast, rest_position,
};
use shotstage_core::{
    CameraData, Entity, EntityKind, Interp, Key, KeyValue, MeshSource, Modifier, Primitive, Scene, TrackProperty,
    Transform,
};

use crate::builtin::{Target, color, finite3, frame_ok, summary_of, target_point};
use crate::{CmdResult, Command, CommandError, command};

/// A world-space ray.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct Ray {
    origin: [f32; 3],
    direction: [f32; 3],
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PlaceParams {
    /// Entity to move (its children move with it).
    id: String,
    /// Drop onto whatever this ray hits first (the entity itself is ignored; the ground y=0
    /// catches rays that miss everything). This is what dragging in the viewport sends.
    ray: Option<Ray>,
    /// Rest on top of this entity, or `"ground"`.
    on: Option<String>,
    /// With `on` or a plain drop: world `[x, z]` to put the footprint centre at.
    at: Option<[f32; 2]>,
    /// Stand next to this entity, bottoms level.
    next_to: Option<String>,
    /// With `nextTo`: which side. Default `right`.
    side: Option<Side>,
    /// With `nextTo`: read `side` as seen through this camera (left/right on screen,
    /// front = towards the camera). Default: world axes (+X right, +Z front).
    view: Option<String>,
    /// With `nextTo`: metres between the two bounds. Default 0.05.
    gap: Option<f32>,
    /// Metres added to the contact point (along the surface); keeps a grab offset while dragging.
    offset: Option<[f32; 3]>,
    /// Grid step in metres for the footprint centre, e.g. 0.25. Default: no snapping.
    snap: Option<f32>,
    /// Frame to evaluate the scene at. Default: the timeline start.
    frame: Option<f32>,
    /// Write a transform keyframe at this frame. Animated entities are always keyed (at
    /// `keyframeAt`, else `frame`) so the move shows.
    keyframe_at: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MoveToParams {
    id: String,
    /// World position for the entity's origin, metres (parenting is taken care of).
    position: [f32; 3],
    frame: Option<f32>,
    keyframe_at: Option<f32>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Default)]
#[serde(rename_all = "camelCase")]
enum Axis {
    X,
    #[default]
    Y,
    Z,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RotateParams {
    id: String,
    /// Degrees to turn (positive = counter-clockwise looking down the axis).
    degrees: f32,
    /// World axis to turn around, through the entity's origin. Default `y`.
    axis: Option<Axis>,
    frame: Option<f32>,
    keyframe_at: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FaceParams {
    /// Entity to turn about the vertical axis so its front (+Z) faces the target.
    id: String,
    target: Target,
    frame: Option<f32>,
    keyframe_at: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BoundsParams {
    id: String,
    /// Default: the timeline start.
    frame: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RaycastParams {
    origin: [f32; 3],
    direction: [f32; 3],
    frame: Option<f32>,
    /// Entities (with their children) the ray passes through.
    ignore: Option<Vec<String>>,
    /// Report a hit on the ground plane y=0 when nothing else is hit. Default true.
    ground: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FrameParams {
    /// What to frame (an actor, a prop, a group).
    subject: String,
    /// Camera to move. Default: the active camera; one is created if the scene has none.
    camera: Option<String>,
    /// Shot size, from `extremeCloseUp` to `extremeWide`.
    size: ShotSize,
    /// Camera height: `eyeLevel` (default), `high`, `low`, `birdsEye`, `wormsEye`.
    angle: Option<ShotAngle>,
    /// Side of the subject the camera stands on, relative to the subject's front (+Z).
    /// Default `front`.
    side: Option<Facing>,
    /// Exact azimuth in degrees from the subject's front (positive = its left); overrides `side`.
    azimuth_degrees: Option<f32>,
    /// Change the lens first (wide lenses stand closer for the same shot size).
    focal_length_mm: Option<f32>,
    frame: Option<f32>,
    /// Write a transform keyframe at this frame (animated cameras are always keyed).
    keyframe_at: Option<f32>,
    /// Make it the active camera. Default true.
    make_active: Option<bool>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
enum PrefabKind {
    Person,
    Table,
    Chair,
    Sofa,
    Bed,
    Stairs,
    Wall,
    Doorway,
    Car,
    Tree,
    Lamp,
    Crate,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PrefabParams {
    kind: PrefabKind,
    /// Id of the group (parts get `<id>_<part>`). Default: generated from the kind.
    id: Option<String>,
    name: Option<String>,
    /// Where the bottom centre goes, metres. Default `[0, 0, 0]`.
    position: Option<[f32; 3]>,
    /// Turn about the vertical axis, degrees. The front of every prefab is +Z.
    rotation_y: Option<f32>,
    /// Uniform scale. Default 1 (real-world size).
    scale: Option<f32>,
    /// Then drop it onto whatever is below `position` (a table, the floor). Default true.
    drop: Option<bool>,
}

// ---------------------------------------------------------------- helpers

fn eval_frame(scene: &Scene, f: Option<f32>) -> CmdResult<f32> {
    frame_ok(f.unwrap_or(scene.timeline.start as f32))
}

/// Change the pose of `id` at `frame` and store it: in the rest transform, and as a key when
/// asked or when the entity is animated (otherwise the change would not show).
fn edit_pose(
    scene: &mut Scene,
    id: &str,
    frame: f32,
    keyframe_at: Option<f32>,
    f: impl FnOnce(&mut Transform),
) -> CmdResult<Transform> {
    let mut t = local_at(scene, id, frame)?;
    f(&mut t);
    if !t.translation().is_finite() || !t.rotation().is_finite() {
        return Err(CommandError::invalid("the result is not finite"));
    }
    let animated = scene.timeline.track(id, TrackProperty::Transform).is_some();
    scene.get_mut(id)?.transform = t;
    if keyframe_at.is_some() || animated {
        let at = frame_ok(keyframe_at.unwrap_or(frame))?;
        scene.timeline.track_mut_or_insert(id, TrackProperty::Transform).set_key(Key {
            frame: at,
            value: KeyValue::Transform(t),
            interp: Interp::default(),
        });
    }
    Ok(t)
}

fn parent_world(scene: &Scene, id: &str, frame: f32) -> CmdResult<glam::Mat4> {
    Ok(match scene.get(id)?.parent.clone() {
        Some(p) => world_at(scene, &p, frame)?,
        None => glam::Mat4::IDENTITY,
    })
}

fn move_to_world(scene: &mut Scene, id: &str, world: Vec3, frame: f32, key: Option<f32>) -> CmdResult<Transform> {
    let local = parent_world(scene, id, frame)?.inverse().transform_point3(world);
    edit_pose(scene, id, frame, key, |t| t.translation = local.to_array())
}

fn set_world_rotation(scene: &mut Scene, id: &str, rot: Quat, frame: f32, key: Option<f32>) -> CmdResult<Transform> {
    let parent_rot = parent_world(scene, id, frame)?.to_scale_rotation_translation().1;
    edit_pose(scene, id, frame, key, |t| t.rotation = (parent_rot.inverse() * rot).normalize().to_array())
}

fn bounds_json(b: Option<spatial::Aabb>) -> Value {
    match b {
        Some(b) => json!({
            "min": b.min.to_array(), "max": b.max.to_array(),
            "center": b.center().to_array(), "size": b.size().to_array(),
        }),
        None => Value::Null,
    }
}

fn snap_ok(s: Option<f32>) -> CmdResult<Option<f32>> {
    match s {
        Some(v) if !(v.is_finite() && (0.0..=100.0).contains(&v)) => Err(CommandError::invalid("snap must be 0..=100")),
        s => Ok(s.filter(|v| *v > 0.0)),
    }
}

// ---------------------------------------------------------------- handlers

fn place(scene: &mut Scene, p: PlaceParams) -> CmdResult<Value> {
    let frame = eval_frame(scene, p.frame)?;
    scene.get(&p.id)?;
    let snap = snap_ok(p.snap)?;
    let modes = [p.ray.is_some(), p.on.is_some(), p.next_to.is_some()].iter().filter(|m| **m).count();
    if modes > 1 {
        return Err(CommandError::invalid("give at most one of `ray`, `on` and `nextTo`"));
    }
    if p.next_to.is_none() && (p.side.is_some() || p.view.is_some() || p.gap.is_some()) {
        return Err(CommandError::invalid("`side`, `view` and `gap` only apply with `nextTo`"));
    }
    let offset = finite3(p.offset.unwrap_or([0.0; 3]), "offset")?;
    let ignore = [p.id.clone()];
    let me = spatial::bounds(scene, &p.id, frame)?;
    let my_origin = world_at(scene, &p.id, frame)?.w_axis.truncate();
    let (world, resting_on) = if let Some(target) = &p.next_to {
        let dir = spatial::side_direction(scene, p.side.unwrap_or(Side::Right), p.view.as_deref(), frame)?;
        let gap = p.gap.unwrap_or(0.05);
        if !gap.is_finite() || gap.abs() > 1000.0 {
            return Err(CommandError::invalid("gap must be within ±1000"));
        }
        let mut w = beside_position(scene, &p.id, target, dir, gap, frame)?;
        w += offset;
        if let Some(s) = snap {
            let (mid_x, mid_z) =
                me.map(|b| (b.center().x - my_origin.x, b.center().z - my_origin.z)).unwrap_or((0.0, 0.0));
            w.x = ((w.x + mid_x) / s).round() * s - mid_x;
            w.z = ((w.z + mid_z) / s).round() * s - mid_z;
        }
        (w, None)
    } else {
        let hit = if let Some(r) = p.ray {
            let (o, d) = (finite3(r.origin, "ray.origin")?, finite3(r.direction, "ray.direction")?);
            raycast(scene, frame, o, d, &RayOptions { ignore: &ignore, ground: true, ..Default::default() })
                .ok_or_else(|| CommandError::new("failed", "the ray hits nothing (not even the ground)"))?
        } else {
            let on = p.on.as_deref().filter(|o| *o != "ground");
            if let Some(t) = on
                && spatial::subtree(scene, &p.id).iter().any(|e| e == t)
            {
                return Err(CommandError::invalid("cannot place an entity on itself or its child"));
            }
            let base = match (on, p.at) {
                (_, Some([x, z])) => finite3([x, 0.0, z], "at")?,
                (Some(t), None) => {
                    let o = world_at(scene, t, frame)?.w_axis.truncate();
                    spatial::bounds(scene, t, frame)?.map(|b| b.center()).unwrap_or(o)
                }
                (None, None) => me.map(|b| b.center()).unwrap_or(my_origin),
            };
            let flat =
                |p: Vec3| spatial::Hit { id: None, point: [p.x, 0.0, p.z], normal: [0.0, 1.0, 0.0], distance: 0.0 };
            match (p.on.as_deref(), on) {
                (Some(_), None) => flat(base),
                (_, Some(t)) => {
                    let tb = spatial::bounds(scene, t, frame)?
                        .ok_or_else(|| CommandError::new("failed", format!("`{t}` has no geometry to rest on")))?;
                    let only = spatial::subtree(scene, t);
                    let from = Vec3::new(base.x, tb.max.y + 1.0, base.z);
                    let opts = RayOptions { ignore: &ignore, only: &only, ground: false };
                    // a ray that slips past (a hole, a torus centre) rests on the bounds' top
                    raycast(scene, frame, from, Vec3::NEG_Y, &opts).unwrap_or(spatial::Hit {
                        id: Some(t.to_string()),
                        point: [base.x, tb.max.y, base.z],
                        normal: [0.0, 1.0, 0.0],
                        distance: 0.0,
                    })
                }
                (None, None) => {
                    // drop: fall from the entity's own centre onto what is below it
                    let y = me.map(|b| b.center().y).unwrap_or(my_origin.y).max(0.0);
                    let opts = RayOptions { ignore: &ignore, ground: true, ..Default::default() };
                    raycast(scene, frame, Vec3::new(base.x, y, base.z), Vec3::NEG_Y, &opts).unwrap_or(flat(base))
                }
            }
        };
        let n = Vec3::from_array(hit.normal);
        let contact = Vec3::from_array(hit.point) + (offset - n * offset.dot(n));
        (rest_position(scene, &p.id, frame, contact, n, snap)?, hit.id)
    };
    let t = move_to_world(scene, &p.id, world, frame, p.keyframe_at)?;
    Ok(json!({ "id": p.id, "position": world.to_array(), "local": t.translation, "restingOn": resting_on }))
}

fn rotate(scene: &mut Scene, p: RotateParams) -> CmdResult<Value> {
    if !p.degrees.is_finite() || p.degrees.abs() > 36_000.0 {
        return Err(CommandError::invalid("degrees must be finite"));
    }
    let frame = eval_frame(scene, p.frame)?;
    let axis = match p.axis.unwrap_or_default() {
        Axis::X => Vec3::X,
        Axis::Y => Vec3::Y,
        Axis::Z => Vec3::Z,
    };
    let world_rot = world_at(scene, &p.id, frame)?.to_scale_rotation_translation().1;
    let r = Quat::from_axis_angle(axis, p.degrees.to_radians()) * world_rot;
    set_world_rotation(scene, &p.id, r, frame, p.keyframe_at)?;
    Ok(summary_of(scene.get(&p.id)?))
}

fn face(scene: &mut Scene, p: FaceParams) -> CmdResult<Value> {
    let frame = eval_frame(scene, p.frame)?;
    let me = world_at(scene, &p.id, frame)?.w_axis.truncate();
    let to = target_point(scene, &p.target, frame)? - me;
    if Vec3::new(to.x, 0.0, to.z).length() < 1e-4 {
        return Err(CommandError::new("failed", "the target is straight above or below"));
    }
    let yaw = to.x.atan2(to.z);
    set_world_rotation(scene, &p.id, Quat::from_rotation_y(yaw), frame, p.keyframe_at)?;
    Ok(json!({ "yawDegrees": yaw.to_degrees() }))
}

fn frame_shot(scene: &mut Scene, p: FrameParams) -> CmdResult<Value> {
    let frame = eval_frame(scene, p.frame)?;
    scene.get(&p.subject)?;
    let cam = match p.camera.or_else(|| scene.active_camera.clone()) {
        Some(c) => {
            scene.camera(&c)?;
            c
        }
        None => {
            let id = scene.unique_id("camera");
            scene.insert(Entity {
                id: id.clone(),
                name: id.clone(),
                parent: None,
                transform: Transform::default(),
                visible: true,
                locked: false,
                kind: EntityKind::Camera(CameraData::default()),
            })?;
            id
        }
    };
    if cam == p.subject {
        return Err(CommandError::invalid("a camera cannot frame itself"));
    }
    if let Some(f) = p.focal_length_mm {
        let EntityKind::Camera(c) = &mut scene.get_mut(&cam)?.kind else { unreachable!() };
        let mut n = *c;
        n.focal_length_mm = f;
        n.validate()?;
        *c = n;
        if let Some(tr) =
            scene.timeline.tracks.iter_mut().find(|t| t.entity == cam && t.property == TrackProperty::FocalLength)
        {
            tr.set_key(Key { frame, value: KeyValue::Scalar(f), interp: Interp::default() });
        }
    }
    let lens = shotstage_core::eval::camera_data_at(scene, &cam, frame)?;
    let az = match p.azimuth_degrees {
        Some(a) if a.is_finite() => a,
        Some(_) => return Err(CommandError::invalid("azimuthDegrees must be finite")),
        None => p.side.unwrap_or_default().azimuth_degrees(),
    };
    let f = frame_subject(scene, &p.subject, frame, &lens, p.size, p.angle.unwrap_or_default(), az)?;
    move_to_world(scene, &cam, f.eye, frame, p.keyframe_at)?;
    set_world_rotation(scene, &cam, f.rotation, frame, p.keyframe_at)?;
    if p.make_active.unwrap_or(true) {
        scene.active_camera = Some(cam.clone());
    }
    let d = describe_shot(scene, &cam, frame, Some(&p.subject))?;
    Ok(json!({ "camera": cam, "position": f.eye.to_array(), "distance": f.distance, "describe": d }))
}

struct Part {
    name: &'static str,
    prim: Primitive,
    pos: [f32; 3],
    rot: [f32; 3],
    color: [f32; 3],
    modifiers: Vec<Modifier>,
}

fn part(name: &'static str, prim: Primitive, pos: [f32; 3], color: [f32; 3]) -> Part {
    Part { name, prim, pos, rot: [0.0; 3], color, modifiers: vec![] }
}

fn cube(x: f32, y: f32, z: f32) -> Primitive {
    Primitive::Cube { size: [x, y, z] }
}

fn cyl(r: f32, h: f32) -> Primitive {
    Primitive::Cylinder { radius: r, height: h, segments: 24 }
}

const WOOD: [f32; 3] = [0.62, 0.46, 0.32];
const FABRIC: [f32; 3] = [0.42, 0.5, 0.62];
const SKIN: [f32; 3] = [0.86, 0.66, 0.52];
const WALL: [f32; 3] = [0.8, 0.78, 0.74];
const METAL: [f32; 3] = [0.35, 0.36, 0.4];

/// Prefab parts with real-world sizes, bottom centre at the origin, front facing +Z.
fn prefab_parts(kind: PrefabKind) -> Vec<Part> {
    use PrefabKind::*;
    match kind {
        Person => vec![
            part("legs", cube(0.34, 0.86, 0.2), [0.0, 0.43, 0.0], [0.3, 0.32, 0.4]),
            part("torso", cube(0.42, 0.6, 0.24), [0.0, 1.16, 0.0], SKIN),
            part("arm_l", cube(0.09, 0.62, 0.1), [0.265, 1.15, 0.0], SKIN),
            part("arm_r", cube(0.09, 0.62, 0.1), [-0.265, 1.15, 0.0], SKIN),
            part("head", Primitive::Sphere { radius: 0.12, segments: 20, rings: 10 }, [0.0, 1.6, 0.0], SKIN),
            part("nose", cube(0.04, 0.04, 0.05), [0.0, 1.6, 0.13], SKIN),
        ],
        Table => {
            let mut v = vec![part("top", cube(1.2, 0.04, 0.8), [0.0, 0.73, 0.0], WOOD)];
            for (n, x, z) in
                [("leg_1", 0.55, 0.35), ("leg_2", -0.55, 0.35), ("leg_3", 0.55, -0.35), ("leg_4", -0.55, -0.35)]
            {
                v.push(part(n, cube(0.05, 0.71, 0.05), [x, 0.355, z], WOOD));
            }
            v
        }
        Chair => {
            let mut v = vec![
                part("seat", cube(0.45, 0.05, 0.45), [0.0, 0.45, 0.0], WOOD),
                part("back", cube(0.45, 0.5, 0.04), [0.0, 0.72, -0.205], WOOD),
            ];
            for (n, x, z) in
                [("leg_1", 0.19, 0.19), ("leg_2", -0.19, 0.19), ("leg_3", 0.19, -0.19), ("leg_4", -0.19, -0.19)]
            {
                v.push(part(n, cube(0.04, 0.43, 0.04), [x, 0.215, z], WOOD));
            }
            v
        }
        Sofa => vec![
            part("base", cube(2.0, 0.42, 0.9), [0.0, 0.21, 0.0], FABRIC),
            part("back", cube(2.0, 0.45, 0.2), [0.0, 0.645, -0.35], FABRIC),
            part("arm_l", cube(0.2, 0.62, 0.9), [0.9, 0.31, 0.0], FABRIC),
            part("arm_r", cube(0.2, 0.62, 0.9), [-0.9, 0.31, 0.0], FABRIC),
        ],
        Bed => vec![
            part("frame", cube(1.6, 0.3, 2.1), [0.0, 0.15, 0.0], WOOD),
            part("mattress", cube(1.5, 0.22, 2.0), [0.0, 0.41, 0.02], [0.92, 0.92, 0.9]),
            part("headboard", cube(1.6, 0.7, 0.08), [0.0, 0.65, -1.01], WOOD),
        ],
        Stairs => vec![Part {
            modifiers: vec![Modifier::Array { count: 10, offset: [0.0, 0.18, -0.28] }],
            ..part("steps", cube(1.0, 0.18, 0.28), [0.0, 0.09, 0.0], WALL)
        }],
        Wall => vec![part("wall", cube(4.0, 2.6, 0.12), [0.0, 1.3, 0.0], WALL)],
        Doorway => vec![
            part("left", cube(1.55, 2.6, 0.12), [1.225, 1.3, 0.0], WALL),
            part("right", cube(1.55, 2.6, 0.12), [-1.225, 1.3, 0.0], WALL),
            part("lintel", cube(0.9, 0.5, 0.12), [0.0, 2.35, 0.0], WALL),
        ],
        Car => {
            let mut v = vec![
                part("body", cube(1.8, 0.7, 4.4), [0.0, 0.6, 0.0], [0.7, 0.2, 0.18]),
                part("cabin", cube(1.6, 0.55, 2.2), [0.0, 1.225, -0.25], [0.55, 0.62, 0.7]),
            ];
            for (n, x, z) in
                [("wheel_1", 0.85, 1.4), ("wheel_2", -0.85, 1.4), ("wheel_3", 0.85, -1.4), ("wheel_4", -0.85, -1.4)]
            {
                v.push(Part { rot: [0.0, 0.0, 90.0], ..part(n, cyl(0.33, 0.25), [x, 0.33, z], [0.12, 0.12, 0.13]) });
            }
            v
        }
        Tree => vec![
            part("trunk", cyl(0.15, 2.0), [0.0, 1.0, 0.0], [0.45, 0.32, 0.22]),
            part(
                "crown",
                Primitive::Sphere { radius: 1.1, segments: 20, rings: 12 },
                [0.0, 2.8, 0.0],
                [0.36, 0.56, 0.3],
            ),
        ],
        Lamp => vec![
            part("base", cyl(0.15, 0.03), [0.0, 0.015, 0.0], METAL),
            part("pole", cyl(0.02, 1.5), [0.0, 0.78, 0.0], METAL),
            part(
                "shade",
                Primitive::Cone { radius: 0.22, height: 0.3, segments: 24 },
                [0.0, 1.6, 0.0],
                [0.95, 0.9, 0.7],
            ),
        ],
        Crate => vec![part("crate", cube(0.6, 0.6, 0.6), [0.0, 0.3, 0.0], WOOD)],
    }
}

fn prefab_name(kind: PrefabKind) -> &'static str {
    use PrefabKind::*;
    match kind {
        Person => "person",
        Table => "table",
        Chair => "chair",
        Sofa => "sofa",
        Bed => "bed",
        Stairs => "stairs",
        Wall => "wall",
        Doorway => "doorway",
        Car => "car",
        Tree => "tree",
        Lamp => "lamp",
        Crate => "crate",
    }
}

pub const PREFABS: &[(&str, &str)] = &[
    ("person", "Standing figure, 1.72 m, front = +Z (the nose)"),
    ("table", "Dining table 1.2 × 0.8 m, 0.75 m high"),
    ("chair", "Dining chair, seat at 0.45 m"),
    ("sofa", "Three-seat sofa, 2 m wide"),
    ("bed", "Double bed 1.6 × 2.1 m"),
    ("stairs", "Straight stairs, 10 steps of 18 cm"),
    ("wall", "Wall 4 m wide, 2.6 m high"),
    ("doorway", "Wall 4 m wide with a 0.9 × 2.1 m opening"),
    ("car", "Car 4.4 m long"),
    ("tree", "Tree, about 3.9 m tall"),
    ("lamp", "Floor lamp, 1.75 m"),
    ("crate", "Box 0.6 m"),
];

fn prefab_create(scene: &mut Scene, p: PrefabParams) -> CmdResult<Value> {
    let stem = prefab_name(p.kind);
    let id = match p.id {
        Some(id) => {
            shotstage_core::scene::validate_id(&id)?;
            id
        }
        None => scene.unique_id(stem),
    };
    let pos = finite3(p.position.unwrap_or([0.0; 3]), "position")?;
    let scale = p.scale.unwrap_or(1.0);
    if !(scale.is_finite() && (0.01..=100.0).contains(&scale)) {
        return Err(CommandError::invalid("scale must be 0.01..=100"));
    }
    let yaw = p.rotation_y.unwrap_or(0.0);
    if !yaw.is_finite() {
        return Err(CommandError::invalid("rotationY must be finite"));
    }
    let root = Transform::from_parts(pos, Quat::from_rotation_y(yaw.to_radians()), Vec3::splat(scale));
    scene.insert(Entity {
        id: id.clone(),
        name: p.name.unwrap_or_else(|| id.clone()),
        parent: None,
        transform: root,
        visible: true,
        locked: false,
        kind: EntityKind::Empty,
    })?;
    let mut parts = vec![];
    for part in prefab_parts(p.kind) {
        let pid = format!("{id}_{}", part.name);
        let mut t = Transform { translation: part.pos, ..Default::default() };
        t.set_euler_degrees(part.rot);
        scene.insert(Entity {
            id: pid.clone(),
            name: part.name.to_string(),
            parent: Some(id.clone()),
            transform: t,
            visible: true,
            locked: false,
            kind: EntityKind::Mesh {
                source: MeshSource::Primitive { primitive: part.prim },
                modifiers: part.modifiers,
                color: color(part.color)?,
            },
        })?;
        parts.push(pid);
    }
    if p.drop.unwrap_or(true) {
        let at = [pos.x, pos.z];
        place(
            scene,
            PlaceParams {
                id: id.clone(),
                ray: None,
                on: None,
                at: Some(at),
                next_to: None,
                side: None,
                view: None,
                gap: None,
                offset: None,
                snap: None,
                frame: None,
                keyframe_at: None,
            },
        )?;
        // keep the requested origin; only the height comes from the surface below
        let e = scene.get_mut(&id)?;
        (e.transform.translation[0], e.transform.translation[2]) = (pos.x, pos.z);
    }
    Ok(json!({ "id": id, "parts": parts }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("entity.place", "Put an entity down like a block: onto whatever a ray hits, on top of another entity, next to one (left/right/front/back, optionally as seen by a camera), or dropped onto what is below it.", mutates: true, PlaceParams, place),
        command!("entity.moveTo", "Move an entity's origin to a world position (works for parented and animated entities).", mutates: true, MoveToParams,
        |s, p| {
            let f = eval_frame(s, p.frame)?;
            let w = finite3(p.position, "position")?;
            let t = move_to_world(s, &p.id, w, f, p.keyframe_at)?;
            Ok(json!({ "local": t.translation }))
        }),
        command!("entity.rotate", "Turn an entity by some degrees around a world axis (default: vertical) through its origin.", mutates: true, RotateParams, rotate),
        command!("entity.face", "Turn an entity about the vertical axis so its front (+Z) faces a target entity or point.", mutates: true, FaceParams, face),
        command!("entity.bounds", "World-space bounding box of an entity and its children (min, max, center, size).", mutates: false, BoundsParams,
            |s, p| { let f = eval_frame(s, p.frame)?; s.get(&p.id)?; Ok(bounds_json(spatial::bounds(s, &p.id, f)?)) }),
        command!("scene.raycast", "First surface a world-space ray hits (entity id or null for the ground, point, normal, distance).", mutates: false, RaycastParams,
        |s, p| {
            let f = eval_frame(s, p.frame)?;
            let ignore = p.ignore.unwrap_or_default();
            let hit = raycast(s, f, finite3(p.origin, "origin")?, finite3(p.direction, "direction")?,
                &RayOptions { ignore: &ignore, ground: p.ground.unwrap_or(true), ..Default::default() });
            Ok(serde_json::to_value(hit).unwrap_or(Value::Null))
        }),
        command!("camera.frame", "Point a camera at a subject by shot size (close-up, medium, full, wide...), angle (eye level, high, low...) and side (front, left, back...). Creates a camera if there is none.", mutates: true, FrameParams, frame_shot),
        command!("prefab.create", "Add a ready-made, real-size block (person, table, chair, sofa, bed, stairs, wall, doorway, car, tree, lamp, crate) as a group, standing on whatever is below.", mutates: true, PrefabParams, prefab_create),
        command!("prefab.list", "The prefab kinds prefab.create accepts, with their sizes.", mutates: false, crate::builtin::NoParams,
            |_, _| Ok(json!(PREFABS.iter().map(|(k, d)| json!({ "kind": k, "description": d })).collect::<Vec<_>>()))),
        Command {
            id: crate::snapshot::SNAPSHOT_COMMAND,
            summary: "Render the view through a camera to a PNG (clay, depth or normal pass) so you can see the shot. Returns {mimeType, data (base64), width, height}. Needs a GPU host (editor, CLI, MCP).",
            mutates: false,
            schema: crate::schema_of::<crate::snapshot::SnapshotParams>,
            run: crate::snapshot::unsupported,
        },
    ]
}
