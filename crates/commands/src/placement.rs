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
    CameraData, Entity, EntityKind, Interp, Key, KeyValue, MeshSource, Scene, TrackProperty, Transform,
};

use crate::builtin::{Target, color, finite3, frame_ok, summary_of, target_point};
use crate::prefabs::{self, PrefabKind};
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
    /// Stretch the group to this world size `[width, height, depth]` in metres instead (e.g. to
    /// match a real product). Overrides `scale`.
    size: Option<[f32; 3]>,
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

/// Store an already computed local transform (keyed like [`edit_pose`]).
pub(crate) fn store_pose(
    scene: &mut Scene,
    id: &str,
    frame: f32,
    keyframe_at: Option<f32>,
    t: Transform,
) -> CmdResult<Transform> {
    edit_pose(scene, id, frame, keyframe_at, |cur| *cur = t)
}

/// Let a group fall onto whatever is below it.
pub(crate) fn drop_down(scene: &mut Scene, id: &str, frame: f32, keyframe_at: Option<f32>) -> CmdResult<Value> {
    place(
        scene,
        PlaceParams {
            id: id.to_string(),
            ray: None,
            on: None,
            at: None,
            next_to: None,
            side: None,
            view: None,
            gap: None,
            offset: None,
            snap: None,
            frame: Some(frame),
            keyframe_at,
        },
    )
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
                    let opts = RayOptions { ignore: &ignore, only: &only, ground: false, eye: None };
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

fn prefab_create(scene: &mut Scene, p: PrefabParams) -> CmdResult<Value> {
    let info = prefabs::info(p.kind);
    let id = match p.id {
        Some(id) => {
            shotstage_core::scene::validate_id(&id)?;
            id
        }
        None => scene.unique_id(info.stem),
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
    for part in prefabs::parts(p.kind) {
        let pid = format!("{id}_{}", part.name);
        let mut t = Transform { translation: part.pos, scale: part.scale, ..Default::default() };
        t.set_euler_degrees(part.rot);
        let kind = match (part.light, part.prim) {
            (Some(l), _) => EntityKind::Light(l),
            (None, Some(prim)) => EntityKind::Mesh {
                source: MeshSource::Primitive { primitive: prim },
                modifiers: part.modifiers,
                color: color(part.color)?,
                material: None,
            },
            (None, None) => EntityKind::Empty,
        };
        let parent = part.parent.map(|q| format!("{id}_{q}")).unwrap_or_else(|| id.clone());
        scene.insert(Entity {
            id: pid.clone(),
            name: part.name.to_string(),
            parent: Some(parent),
            transform: t,
            visible: true,
            locked: false,
            kind,
        })?;
        parts.push(pid);
    }
    if let Some(size) = p.size {
        let want = finite3(size, "size")?;
        if want.min_element() <= 0.0 {
            return Err(CommandError::invalid("size must be positive"));
        }
        let frame = scene.timeline.start as f32;
        if let Some(b) = spatial::bounds(scene, &id, frame)? {
            // bounds are measured with the yaw applied; stretch in the group's own axes
            let have = b.size().max(Vec3::splat(1e-4));
            let (sx, sz) = if (yaw.rem_euclid(180.0) - 90.0).abs() < 45.0 {
                (want.z / have.x, want.x / have.z)
            } else {
                (want.x / have.x, want.z / have.z)
            };
            let e = scene.get_mut(&id)?;
            e.transform.scale = [sx, want.y / have.y, sz].map(|v| v * scale);
        }
    }
    if p.drop.unwrap_or(true) {
        drop_down(scene, &id, scene.timeline.start as f32, None)?;
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
        command!("prefab.create", "Add a ready-made, real-size block as a group standing on whatever is below: rigged people (man, woman), furniture, fresh-food retail fixtures and produce, product-shoot stand-ins and sets. See prefab.list.", mutates: true, PrefabParams, prefab_create),
        command!("prefab.list", "The prefab kinds prefab.create accepts, with their sizes.", mutates: false, crate::builtin::NoParams,
            |_, _| Ok(json!(prefabs::PREFABS.iter().map(|p| json!({ "kind": p.key, "category": p.category.key(), "description": p.description })).collect::<Vec<_>>()))),
        Command {
            id: crate::snapshot::SNAPSHOT_COMMAND,
            summary: "Render the view through a camera to a PNG (clay, depth or normal pass) so you can see the shot. Returns {mimeType, data (base64), width, height}. Needs a GPU host (editor, CLI, MCP).",
            mutates: false,
            schema: crate::schema_of::<crate::snapshot::SnapshotParams>,
            run: crate::snapshot::unsupported,
        },
    ]
}
