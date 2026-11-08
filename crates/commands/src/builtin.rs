//! Built-in commands. Each takes a typed, documented params struct (its doc comments become
//! the JSON Schema descriptions agents read) and returns JSON.

use glam::{Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::camera::look_rotation;
use shotstage_core::eval::{camera_trajectory, describe_shot, world_at};
use shotstage_core::mesh::evaluate_mesh;
use shotstage_core::scene::validate_id;
use shotstage_core::{
    CameraData, Entity, EntityKind, Interp, Key, KeyValue, Mesh, MeshSource, Modifier, Primitive, Scene, Shot,
    Timeline, TrackProperty, Transform, evaluate,
};

use crate::{CmdResult, Command, CommandError, command};

pub(crate) fn finite3(v: [f32; 3], what: &str) -> CmdResult<Vec3> {
    if v.iter().all(|x| x.is_finite() && x.abs() < 1e6) {
        Ok(Vec3::from_array(v))
    } else {
        Err(CommandError::invalid(format!("{what} must be finite and under 1e6")))
    }
}

pub(crate) fn color(v: [f32; 3]) -> CmdResult<[f32; 3]> {
    if v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)) {
        Ok(v)
    } else {
        Err(CommandError::invalid("color components must be 0..=1"))
    }
}

pub(crate) fn frame_ok(f: f32) -> CmdResult<f32> {
    if f.is_finite() && (-100_000.0..=1_000_000.0).contains(&f) {
        Ok(f)
    } else {
        Err(CommandError::invalid(format!("bad frame {f}")))
    }
}

pub(crate) fn summary_of(e: &Entity) -> Value {
    json!({
        "id": e.id, "name": e.name, "type": e.kind.type_name(), "parent": e.parent,
        "visible": e.visible,
        "position": e.transform.translation,
        "rotationEuler": e.transform.euler_degrees(),
        "scale": e.transform.scale,
    })
}

fn mesh_parts<'a>(
    scene: &'a mut Scene,
    id: &str,
) -> CmdResult<(&'a mut MeshSource, &'a mut Vec<Modifier>, &'a mut [f32; 3])> {
    match &mut scene.get_mut(id)?.kind {
        EntityKind::Mesh { source, modifiers, color, .. } => Ok((source, modifiers, color)),
        _ => Err(CommandError::new("failed", format!("entity `{id}` is not a mesh"))),
    }
}

fn camera_mut<'a>(scene: &'a mut Scene, id: &str) -> CmdResult<&'a mut CameraData> {
    match &mut scene.get_mut(id)?.kind {
        EntityKind::Camera(c) => Ok(c),
        _ => Err(CommandError::new("failed", format!("entity `{id}` is not a camera"))),
    }
}

pub(crate) fn pick_camera(scene: &Scene, id: Option<String>, frame: f32) -> CmdResult<String> {
    id.or_else(|| shotstage_core::eval::camera_id_at(scene, frame))
        .ok_or_else(|| CommandError::new("failed", "no camera given and the scene has no active camera"))
}

// ---------------------------------------------------------------- params

#[derive(Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoParams {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdParams {
    /// Entity id.
    id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LoadParams {
    /// A complete scene document (as returned by `scene.inspect`).
    scene: Scene,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FrameParams {
    /// Frame number.
    frame: f32,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
enum CreateType {
    Mesh,
    Camera,
    Empty,
    Light,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CreateParams {
    /// What to create.
    #[serde(rename = "type")]
    kind: CreateType,
    /// For meshes: the parametric primitive. Default: a 1 m cube.
    primitive: Option<Primitive>,
    /// For meshes: explicit geometry instead of a primitive.
    mesh: Option<Mesh>,
    /// For cameras: lens settings.
    camera: Option<CameraData>,
    /// For lights: kind (sun, point, spot, area), colour, intensity, cone, size. A light shines
    /// along its -Z: aim it with `camera.lookAt`.
    light: Option<shotstage_core::LightData>,
    /// Id to use (1-64 of A-Za-z0-9_-). Default: generated from the name or type.
    id: Option<String>,
    name: Option<String>,
    parent: Option<String>,
    /// Metres.
    position: Option<[f32; 3]>,
    /// Degrees, `[pitchX, yawY, rollZ]`.
    rotation_euler: Option<[f32; 3]>,
    scale: Option<[f32; 3]>,
    /// Linear RGB 0..1.
    color: Option<[f32; 3]>,
    /// For cameras: make it the active camera. Default: true when no camera is active.
    make_active: Option<bool>,
    /// Can't be dragged in the viewport (floors, backdrops). Default false.
    locked: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DuplicateParams {
    id: String,
    new_id: Option<String>,
    /// Offset of the copy from the original, metres. Default `[0.5, 0, 0]`.
    offset: Option<[f32; 3]>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenameParams {
    id: String,
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetTransformParams {
    id: String,
    /// Metres, local to the parent.
    position: Option<[f32; 3]>,
    /// Degrees, `[pitchX, yawY, rollZ]` (applied yaw, then pitch, then roll).
    rotation_euler: Option<[f32; 3]>,
    /// Quaternion `[x, y, z, w]`; overrides `rotationEuler`.
    rotation: Option<[f32; 4]>,
    scale: Option<[f32; 3]>,
    /// Also write a transform keyframe at this frame.
    keyframe_at: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TranslateParams {
    id: String,
    /// Metres to move, in the parent's space.
    delta: [f32; 3],
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ParentParams {
    id: String,
    /// New parent id, or null for the scene root.
    parent: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LockedParams {
    id: String,
    locked: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct VisibleParams {
    id: String,
    visible: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PrimitiveParams {
    id: String,
    primitive: Primitive,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ColorParams {
    id: String,
    /// Linear RGB 0..1.
    color: [f32; 3],
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddModifierParams {
    id: String,
    modifier: Modifier,
    /// Position in the stack. Default: the end.
    index: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IndexParams {
    id: String,
    /// Modifier index (0 = first).
    index: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct JoinParams {
    /// Meshes to merge; the first one receives the geometry, the rest are deleted.
    ids: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetMeshParams {
    id: String,
    /// New explicit geometry (replaces the primitive; modifiers are kept).
    mesh: Mesh,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExportObjParams {
    id: String,
    /// Apply modifiers. Default true.
    evaluated: Option<bool>,
    /// Bake the world transform. Default false.
    world: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LensParams {
    id: String,
    focal_length_mm: Option<f32>,
    sensor_width_mm: Option<f32>,
    /// Width / height, e.g. 1.7778 for 16:9.
    aspect: Option<f32>,
    near: Option<f32>,
    far: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
pub(crate) enum Target {
    /// Another entity's id.
    Entity(String),
    /// A world-space point, metres.
    Point([f32; 3]),
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LookAtParams {
    /// Entity to rotate (usually a camera).
    id: String,
    target: Target,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ActiveParams {
    /// Camera id, or null to clear.
    id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DescribeParams {
    /// Camera id. Default: the camera in use at `frame`.
    id: Option<String>,
    /// Default: the timeline start.
    frame: Option<f32>,
    /// Entity to measure the shot size against (e.g. the actor). Default: the person nearest
    /// the centre of the frame, else the nearest-to-centre object.
    subject: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrajectoryParams {
    /// Camera id. Default: the active camera.
    id: Option<String>,
    /// Default: timeline start.
    start: Option<u32>,
    /// Default: timeline end.
    end: Option<u32>,
    /// Output resolution for the intrinsics. Default 1280x720.
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TimelineParams {
    fps: Option<f32>,
    start: Option<u32>,
    end: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyframeParams {
    id: String,
    /// `transform` (any entity) or `focalLength` (cameras).
    property: TrackProperty,
    frame: f32,
    /// `ease` (default), `linear` or `step`: how to move from this key to the next.
    interp: Option<Interp>,
    /// Value to key. Default: the entity's current value.
    value: Option<KeyValue>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyframeDeleteParams {
    id: String,
    property: TrackProperty,
    frame: f32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyframeListParams {
    /// Only this entity. Default: all tracks.
    id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShotParams {
    id: Option<String>,
    name: Option<String>,
    camera: String,
    start: u32,
    end: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OrbitMoveParams {
    /// Camera to animate.
    id: String,
    /// What to orbit around (entity id or point).
    target: Target,
    /// Degrees to travel around the target (positive = counter-clockwise seen from above).
    degrees: f32,
    start: u32,
    end: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DollyMoveParams {
    /// Camera to animate.
    id: String,
    /// Metres to move along the view direction (positive = push in).
    distance: f32,
    start: u32,
    end: u32,
    interp: Option<Interp>,
}

// ---------------------------------------------------------------- handlers

pub(crate) fn target_point(scene: &Scene, t: &Target, frame: f32) -> CmdResult<Vec3> {
    Ok(match t {
        Target::Entity(id) => world_at(scene, id, frame)?.w_axis.truncate(),
        Target::Point(p) => finite3(*p, "target")?,
    })
}

fn create(scene: &mut Scene, p: CreateParams) -> CmdResult<Value> {
    if p.kind != CreateType::Mesh && (p.primitive.is_some() || p.color.is_some() || p.mesh.is_some()) {
        return Err(CommandError::invalid("`primitive`, `mesh` and `color` only apply to type mesh"));
    }
    let kind = match p.kind {
        CreateType::Mesh => {
            if p.camera.is_some() {
                return Err(CommandError::invalid("`camera` only applies to type camera"));
            }
            let source = match (p.mesh, p.primitive) {
                (Some(_), Some(_)) => return Err(CommandError::invalid("give `mesh` or `primitive`, not both")),
                (Some(mesh), None) => MeshSource::Data { mesh },
                (None, prim) => MeshSource::Primitive { primitive: prim.unwrap_or(Primitive::Cube { size: [1.0; 3] }) },
            };
            source.base_mesh()?;
            EntityKind::Mesh {
                source,
                modifiers: vec![],
                color: color(p.color.unwrap_or([0.72, 0.70, 0.66]))?,
                material: None,
            }
        }
        CreateType::Camera => {
            let c = p.camera.unwrap_or_default();
            c.validate()?;
            EntityKind::Camera(c)
        }
        CreateType::Empty => EntityKind::Empty,
        CreateType::Light => {
            let l = p.light.unwrap_or_default();
            l.validate()?;
            EntityKind::Light(l)
        }
    };
    let stem = match (&p.name, &kind) {
        (Some(n), _) => n.to_lowercase(),
        (None, EntityKind::Mesh { source: MeshSource::Primitive { primitive }, .. }) => serde_json::to_value(primitive)
            .ok()
            .and_then(|v| v.get("shape").and_then(|s| s.as_str().map(str::to_string)))
            .unwrap_or_else(|| "mesh".into()),
        (None, k) => k.type_name().into(),
    };
    let id = match p.id {
        Some(id) => {
            validate_id(&id)?;
            id
        }
        None => scene.unique_id(&stem),
    };
    let mut t = Transform::default();
    if let Some(v) = p.position {
        t.translation = finite3(v, "position")?.to_array();
    }
    if let Some(e) = p.rotation_euler {
        t.set_euler_degrees(finite3(e, "rotationEuler")?.to_array());
    }
    if let Some(s) = p.scale {
        t.scale = finite3(s, "scale")?.to_array();
    }
    let is_cam = matches!(kind, EntityKind::Camera(_));
    let name = p.name.unwrap_or_else(|| id.clone());
    let locked = p.locked.unwrap_or(false);
    scene.insert(Entity { id: id.clone(), name, parent: p.parent, transform: t, visible: true, locked, kind })?;
    if is_cam && p.make_active.unwrap_or(scene.active_camera.is_none()) {
        scene.active_camera = Some(id.clone());
    }
    Ok(json!({ "id": id }))
}

fn set_transform(scene: &mut Scene, p: SetTransformParams) -> CmdResult<Value> {
    let e = scene.get_mut(&p.id)?;
    if let Some(v) = p.position {
        e.transform.translation = finite3(v, "position")?.to_array();
    }
    if let Some(q) = p.rotation {
        let q = Quat::from_array(q);
        if !q.is_finite() || q.length_squared() < 1e-8 {
            return Err(CommandError::invalid("rotation must be a non-zero finite quaternion"));
        }
        e.transform.rotation = q.normalize().to_array();
    } else if let Some(r) = p.rotation_euler {
        e.transform.set_euler_degrees(finite3(r, "rotationEuler")?.to_array());
    }
    if let Some(s) = p.scale {
        e.transform.scale = finite3(s, "scale")?.to_array();
    }
    let t = e.transform;
    let s = summary_of(e);
    if let Some(f) = p.keyframe_at {
        scene.timeline.track_mut_or_insert(&p.id, TrackProperty::Transform).set_key(Key {
            frame: frame_ok(f)?,
            value: KeyValue::Transform(t),
            interp: Interp::default(),
        });
    }
    Ok(s)
}

fn keyframe_set(scene: &mut Scene, p: KeyframeParams) -> CmdResult<Value> {
    let frame = frame_ok(p.frame)?;
    // expressions live on the person's root, whichever part was named
    let id =
        if p.property == TrackProperty::Expression { crate::humans::human_root(scene, &p.id)? } else { p.id.clone() };
    let value = match (p.property, p.value) {
        (TrackProperty::Transform, None) => KeyValue::Transform(scene.get(&p.id)?.transform),
        (TrackProperty::FocalLength, None) => KeyValue::Scalar(scene.camera(&p.id)?.focal_length_mm),
        (TrackProperty::Transform, Some(v @ KeyValue::Transform(t))) => {
            scene.get(&p.id)?;
            finite3(t.translation, "translation")?;
            finite3(t.scale, "scale")?;
            v
        }
        (TrackProperty::FocalLength, Some(KeyValue::Scalar(f))) => {
            scene.camera(&p.id)?;
            if !(1.0..=2000.0).contains(&f) {
                return Err(CommandError::invalid("focal length must be 1..=2000 mm"));
            }
            KeyValue::Scalar(f)
        }
        (TrackProperty::Expression, None) => {
            // re-key what the face shows here (dominant expression of the current mix)
            let m = scene
                .timeline
                .track(&id, TrackProperty::Expression)
                .and_then(|t| t.sample(frame))
                .and_then(|v| if let KeyValue::Expression(m) = v { Some(m) } else { None })
                .ok_or_else(|| {
                    CommandError::invalid("no expression keys yet: use person.setExpression with keyframeAt")
                })?;
            let (i, w) = if m.a_amount >= m.b_amount { (m.a, m.a_amount) } else { (m.b, m.b_amount) };
            KeyValue::Expression(shotstage_core::ExpressionMix::single(i, w.max(m.a_amount + m.b_amount).min(1.5)))
        }
        (TrackProperty::Expression, Some(KeyValue::Expression(m))) => {
            let n = shotstage_core::human::EXPRESSIONS.len() as u8;
            let ok = |w: f32| w.is_finite() && (0.0..=1.5).contains(&w);
            if m.a >= n || m.b >= n || !ok(m.a_amount) || !ok(m.b_amount) {
                return Err(CommandError::invalid("expression index or amount out of range"));
            }
            KeyValue::Expression(m)
        }
        _ => return Err(CommandError::invalid("value kind does not match the property")),
    };
    let track = scene.timeline.track_mut_or_insert(&id, p.property);
    track.set_key(Key { frame, value, interp: p.interp.unwrap_or_default() });
    Ok(json!({ "keys": track.keys.len() }))
}

fn join(scene: &mut Scene, p: JoinParams) -> CmdResult<Value> {
    let Some((first, rest)) = p.ids.split_first() else {
        return Err(CommandError::invalid("`ids` must list at least two meshes"));
    };
    if rest.is_empty() {
        return Err(CommandError::invalid("`ids` must list at least two meshes"));
    }
    let to_local = scene.world_matrix(first)?.inverse();
    let mut merged = {
        let e = scene.get(first)?;
        match &e.kind {
            EntityKind::Mesh { source, modifiers, .. } => evaluate_mesh(source, modifiers)?,
            _ => return Err(CommandError::new("failed", format!("entity `{first}` is not a mesh"))),
        }
    };
    for id in rest {
        if id == first {
            return Err(CommandError::invalid("an id is listed twice"));
        }
        let e = scene.get(id)?;
        let EntityKind::Mesh { source, modifiers, .. } = &e.kind else {
            return Err(CommandError::new("failed", format!("entity `{id}` is not a mesh")));
        };
        let m = evaluate_mesh(source, modifiers)?.transformed(to_local * scene.world_matrix(id)?);
        merged.append(&m);
    }
    for id in rest {
        scene.remove(id)?;
    }
    let (source, modifiers, _) = mesh_parts(scene, first)?;
    *source = MeshSource::Data { mesh: merged };
    modifiers.clear();
    Ok(json!({ "id": first }))
}

fn orbit_move(scene: &mut Scene, p: OrbitMoveParams) -> CmdResult<Value> {
    if p.end <= p.start {
        return Err(CommandError::invalid("end must be after start"));
    }
    if !p.degrees.is_finite() || p.degrees.abs() > 3600.0 {
        return Err(CommandError::invalid("degrees must be within ±3600"));
    }
    if scene.get(&p.id)?.parent.is_some() {
        return Err(CommandError::invalid("orbit moves need an unparented camera"));
    }
    let center = target_point(scene, &p.target, p.start as f32)?;
    // start from wherever the camera is at `start` (it may already be animated)
    let start = shotstage_core::eval::local_at(scene, &p.id, p.start as f32)?;
    let offset = start.translation() - center;
    // one key every ~15 degrees keeps the arc round under interpolation
    let steps = ((p.degrees.abs() / 15.0).ceil() as u32).max(2);
    let track = scene.timeline.track_mut_or_insert(&p.id, TrackProperty::Transform);
    track.keys.retain(|k| k.frame < p.start as f32 || k.frame > p.end as f32);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let rot = Quat::from_rotation_y((p.degrees * t).to_radians());
        let eye = center + rot * offset;
        let look = look_rotation(eye, center)?;
        let frame = p.start as f32 + (p.end - p.start) as f32 * t;
        track.set_key(Key {
            frame,
            value: KeyValue::Transform(Transform::from_parts(eye, look, start.scale())),
            interp: Interp::Linear,
        });
    }
    Ok(json!({ "keys": steps + 1 }))
}

fn dolly_move(scene: &mut Scene, p: DollyMoveParams) -> CmdResult<Value> {
    if p.end <= p.start || !p.distance.is_finite() {
        return Err(CommandError::invalid("need end > start and a finite distance"));
    }
    let a = shotstage_core::eval::local_at(scene, &p.id, p.start as f32)?;
    let dir = a.rotation() * Vec3::NEG_Z;
    let b = Transform::from_parts(a.translation() + dir * p.distance, a.rotation(), a.scale());
    let interp = p.interp.unwrap_or_default();
    let track = scene.timeline.track_mut_or_insert(&p.id, TrackProperty::Transform);
    track.set_key(Key { frame: p.start as f32, value: KeyValue::Transform(a), interp });
    track.set_key(Key { frame: p.end as f32, value: KeyValue::Transform(b), interp });
    Ok(json!({ "from": a.translation, "to": b.translation }))
}

// ---------------------------------------------------------------- registry

pub fn registry() -> Vec<Command> {
    vec![
        // scene
        command!("scene.inspect", "Return the whole scene document as JSON.", mutates: false, NoParams,
            |s, _| Ok(serde_json::to_value(&*s).unwrap_or(Value::Null))),
        command!("scene.summary", "Short list of entities (id, name, type, parent, transform), timeline and shots.", mutates: false, NoParams,
        |s, _| Ok(json!({
            "entities": s.entities.iter().map(summary_of).collect::<Vec<_>>(),
            "activeCamera": s.active_camera,
            "timeline": { "fps": s.timeline.fps, "start": s.timeline.start, "end": s.timeline.end,
                "durationSeconds": s.timeline.duration_seconds(), "tracks": s.timeline.tracks.len() },
            "shots": s.timeline.shots,
        }))),
        command!("scene.new", "Clear everything and start an empty scene.", mutates: true, NoParams,
            |s, _| { *s = Scene::default(); Ok(Value::Null) }),
        command!("scene.load", "Replace the scene with a full document.", mutates: true, LoadParams,
        |s, p| {
            for e in &p.scene.entities { validate_id(&e.id)?; }
            *s = p.scene;
            Ok(json!({ "entities": s.entities.len() }))
        }),
        command!("scene.evaluate", "Animated transforms of every entity at a frame, and the camera in use.", mutates: false, FrameParams,
            |s, p| Ok(serde_json::to_value(evaluate(s, frame_ok(p.frame)?)?).unwrap_or(Value::Null))),
        // entities
        command!("entity.create", "Create a mesh (primitive or explicit geometry), camera or empty.", mutates: true, CreateParams, create),
        command!("entity.delete", "Delete an entity and its children.", mutates: true, IdParams,
            |s, p| Ok(json!({ "deleted": s.remove(&p.id)? }))),
        command!("entity.duplicate", "Copy an entity with its children (a whole prefab group) and their animation.", mutates: true, DuplicateParams,
        |s, p| {
            let off = finite3(p.offset.unwrap_or([0.5, 0.0, 0.0]), "offset")?;
            let ids = shotstage_core::spatial::subtree(s, &p.id);
            let new_root = match p.new_id { Some(id) => { validate_id(&id)?; id } None => s.unique_id(&p.id) };
            let mut map = std::collections::HashMap::new();
            map.insert(p.id.clone(), new_root.clone());
            for id in &ids[1..] {
                // `<root>_<part>` becomes `<newRoot>_<part>` so groups stay readable
                let stem = id.strip_prefix(&format!("{}_", p.id)).map(|rest| format!("{new_root}_{rest}")).unwrap_or_else(|| id.clone());
                let fresh = if s.contains(&stem) || map.values().any(|v| *v == stem) { s.unique_id(&stem) } else { stem };
                map.insert(id.clone(), fresh);
            }
            for id in &ids {
                let mut e = s.get(id)?.clone();
                e.id = map[id].clone();
                if id == &p.id {
                    e.transform.translation = (e.transform.translation() + off).to_array();
                    let parent = e.parent.take();
                    crate::clipboard::remap_refs(&mut e, &map, &Default::default());
                    e.parent = parent;
                } else {
                    // parent and skin joints point at the copies
                    crate::clipboard::remap_refs(&mut e, &map, &Default::default());
                }
                s.insert(e)?;
            }
            let tracks: Vec<_> = s.timeline.tracks.iter().filter(|t| map.contains_key(&t.entity)).cloned().collect();
            for mut t in tracks {
                if t.entity == p.id && t.property == TrackProperty::Transform {
                    for k in &mut t.keys {
                        if let KeyValue::Transform(tr) = &mut k.value { tr.translation = (tr.translation() + off).to_array(); }
                    }
                }
                t.entity = map[&t.entity].clone();
                s.timeline.tracks.push(t);
            }
            Ok(json!({ "id": new_root, "entities": ids.len() }))
        }),
        command!("entity.rename", "Change an entity's display name (the id stays).", mutates: true, RenameParams,
        |s, p| {
            if p.name.is_empty() || p.name.len() > 200 { return Err(CommandError::invalid("name must be 1-200 bytes")); }
            s.get_mut(&p.id)?.name = p.name; Ok(Value::Null)
        }),
        command!("entity.get", "One entity in full.", mutates: false, IdParams,
            |s, p| Ok(serde_json::to_value(s.get(&p.id)?).unwrap_or(Value::Null))),
        command!("entity.setTransform", "Set position, rotation and/or scale; optionally keyframe it.", mutates: true, SetTransformParams, set_transform),
        command!("entity.translate", "Move an entity by a delta.", mutates: true, TranslateParams,
        |s, p| {
            let d = finite3(p.delta, "delta")?;
            let e = s.get_mut(&p.id)?;
            e.transform.translation = (e.transform.translation() + d).to_array();
            Ok(summary_of(e))
        }),
        command!("entity.setParent", "Re-parent an entity (null = scene root). Keeps the local transform.", mutates: true, ParentParams,
            |s, p| { s.set_parent(&p.id, p.parent.as_deref())?; Ok(Value::Null) }),
        command!("entity.setVisible", "Show or hide an entity.", mutates: true, VisibleParams,
            |s, p| { s.get_mut(&p.id)?.visible = p.visible; Ok(Value::Null) }),
        command!("entity.setLocked", "Lock an entity so viewport drags pass through it (floors, backdrops); commands still edit it.", mutates: true, LockedParams,
            |s, p| { s.get_mut(&p.id)?.locked = p.locked; Ok(Value::Null) }),
        // meshes
        command!("mesh.setPrimitive", "Change a mesh's primitive parameters (size, segments...).", mutates: true, PrimitiveParams,
            |s, p| { p.primitive.build()?; let (src, _, _) = mesh_parts(s, &p.id)?; *src = MeshSource::Primitive { primitive: p.primitive }; Ok(Value::Null) }),
        command!("mesh.setData", "Replace a mesh's geometry with explicit vertices and faces.", mutates: true, SetMeshParams,
            |s, p| { p.mesh.validate()?; let (src, _, _) = mesh_parts(s, &p.id)?; *src = MeshSource::Data { mesh: p.mesh }; Ok(Value::Null) }),
        command!("mesh.setColor", "Set a mesh's display colour.", mutates: true, ColorParams,
            |s, p| { let c = color(p.color)?; let (_, _, col) = mesh_parts(s, &p.id)?; *col = c; Ok(Value::Null) }),
        command!("mesh.addModifier", "Add a non-destructive modifier (mirror, array, subdivide).", mutates: true, AddModifierParams,
        |s, p| {
            p.modifier.validate()?;
            let (src, mods, _) = mesh_parts(s, &p.id)?;
            let i = p.index.unwrap_or(mods.len()).min(mods.len());
            mods.insert(i, p.modifier);
            let (src, mods) = (src.clone(), mods.clone());
            evaluate_mesh(&src, &mods)?;
            Ok(json!({ "index": i, "modifiers": mods.len() }))
        }),
        command!("mesh.removeModifier", "Remove a modifier from the stack.", mutates: true, IndexParams,
        |s, p| {
            let (_, mods, _) = mesh_parts(s, &p.id)?;
            if p.index >= mods.len() { return Err(CommandError::invalid(format!("no modifier {} (stack has {})", p.index, mods.len()))); }
            mods.remove(p.index); Ok(Value::Null)
        }),
        command!("mesh.applyModifiers", "Bake the modifier stack into explicit geometry.", mutates: true, IdParams,
        |s, p| {
            let (src, mods, _) = mesh_parts(s, &p.id)?;
            let m = evaluate_mesh(src, mods)?;
            let faces = m.faces.len();
            *src = MeshSource::Data { mesh: m }; mods.clear();
            Ok(json!({ "faces": faces }))
        }),
        command!("mesh.join", "Merge several meshes into the first one (world positions kept).", mutates: true, JoinParams, join),
        command!("mesh.setOriginToCenter", "Move the origin to the centre of the geometry's bounds, keeping it in place.", mutates: true, IdParams,
        |s, p| {
            let e = s.get(&p.id)?.clone();
            let EntityKind::Mesh { source, .. } = &e.kind else { return Err(CommandError::new("failed", "not a mesh")) };
            let base = source.base_mesh()?;
            let Some((lo, hi)) = base.bounds() else { return Ok(Value::Null) };
            let c = (lo + hi) / 2.0;
            let moved = base.transformed(Mat4::from_translation(-c));
            let shift = e.transform.matrix().transform_vector3(c);
            let (src, _, _) = mesh_parts(s, &p.id)?;
            *src = MeshSource::Data { mesh: moved };
            let ent = s.get_mut(&p.id)?;
            ent.transform.translation = (ent.transform.translation() + shift).to_array();
            Ok(json!({ "shift": shift.to_array() }))
        }),
        command!("mesh.stats", "Vertex, edge, face and triangle counts and bounds (after modifiers).", mutates: false, IdParams,
        |s, p| {
            let e = s.get(&p.id)?;
            let EntityKind::Mesh { source, modifiers, .. } = &e.kind else { return Err(CommandError::new("failed", "not a mesh")) };
            let m = evaluate_mesh(source, modifiers)?;
            let tris: usize = m.faces.iter().map(|f| f.len() - 2).sum();
            let b = m.bounds().map(|(lo, hi)| json!({ "min": lo.to_array(), "max": hi.to_array() }));
            Ok(json!({ "vertices": m.positions.len(), "edges": m.edge_count(), "faces": m.faces.len(), "triangles": tris, "bounds": b, "modifiers": modifiers }))
        }),
        command!("mesh.exportObj", "Wavefront OBJ text of a mesh.", mutates: false, ExportObjParams,
        |s, p| {
            let e = s.get(&p.id)?;
            let EntityKind::Mesh { source, modifiers, .. } = &e.kind else { return Err(CommandError::new("failed", "not a mesh")) };
            let mut m = if p.evaluated.unwrap_or(true) { evaluate_mesh(source, modifiers)? } else { source.base_mesh()? };
            if p.world.unwrap_or(false) { m = m.transformed(s.world_matrix(&p.id)?); }
            Ok(json!({ "obj": m.to_obj(&e.name) }))
        }),
        // cameras
        command!("camera.setLens", "Change focal length, sensor width, aspect or clip planes.", mutates: true, LensParams,
        |s, p| {
            let c = camera_mut(s, &p.id)?;
            let mut n = *c;
            if let Some(v) = p.focal_length_mm { n.focal_length_mm = v; }
            if let Some(v) = p.sensor_width_mm { n.sensor_width_mm = v; }
            if let Some(v) = p.aspect { n.aspect = v; }
            if let Some(v) = p.near { n.near = v; }
            if let Some(v) = p.far { n.far = v; }
            n.validate()?;
            *c = n;
            Ok(json!({ "hfovDegrees": n.hfov().to_degrees(), "vfovDegrees": n.vfov().to_degrees() }))
        }),
        command!("camera.lookAt", "Rotate an entity so its -Z axis faces a target entity or point.", mutates: true, LookAtParams,
        |s, p| {
            let eye = s.world_matrix(&p.id)?.w_axis.truncate();
            let target = target_point(s, &p.target, s.timeline.start as f32)?;
            let world_rot = look_rotation(eye, target)?;
            let parent_rot = match s.get(&p.id)?.parent.clone() {
                Some(par) => s.world_matrix(&par)?.to_scale_rotation_translation().1,
                None => Quat::IDENTITY,
            };
            s.get_mut(&p.id)?.transform.rotation = (parent_rot.inverse() * world_rot).normalize().to_array();
            Ok(Value::Null)
        }),
        command!("camera.setActive", "Choose the camera the viewport and exports look through.", mutates: true, ActiveParams,
            |s, p| { if let Some(id) = &p.id { s.camera(id)?; } s.active_camera = p.id; Ok(Value::Null) }),
        command!("camera.describe", "Describe a shot in film terms: shot size, angle, lens.", mutates: false, DescribeParams,
        |s, p| {
            let frame = frame_ok(p.frame.unwrap_or(s.timeline.start as f32))?;
            let cam = pick_camera(s, p.id, frame)?;
            let subject = p.subject.or_else(|| crate::host::auto_subject(s, &cam, frame));
            Ok(serde_json::to_value(describe_shot(s, &cam, frame, subject.as_deref())?).unwrap_or(Value::Null))
        }),
        command!("camera.trajectory", "Per-frame intrinsics and world-to-camera matrices (OpenCV axes) for camera-controlled video models.", mutates: false, TrajectoryParams,
        |s, p| {
            let cam = pick_camera(s, p.id, s.timeline.start as f32)?;
            let frames = camera_trajectory(s, &cam, p.start.unwrap_or(s.timeline.start), p.end.unwrap_or(s.timeline.end),
                p.width.unwrap_or(1280), p.height.unwrap_or(720))?;
            Ok(json!({ "camera": cam, "fps": s.timeline.fps, "frames": frames }))
        }),
        command!("camera.orbitMove", "Keyframe an orbit of a camera around a target between two frames.", mutates: true, OrbitMoveParams, orbit_move),
        command!("camera.dollyMove", "Keyframe a push-in / pull-out along the camera's view direction.", mutates: true, DollyMoveParams, dolly_move),
        // timeline
        command!("timeline.set", "Set frame rate and frame range.", mutates: true, TimelineParams,
        |s, p| {
            let t = &mut s.timeline;
            let (fps, start, end) = (p.fps.unwrap_or(t.fps), p.start.unwrap_or(t.start), p.end.unwrap_or(t.end));
            Timeline::validate_range(fps, start, end)?;
            (t.fps, t.start, t.end) = (fps, start, end);
            Ok(json!({ "durationSeconds": t.duration_seconds() }))
        }),
        command!("keyframe.set", "Set a keyframe (default value: the entity's current transform / focal length).", mutates: true, KeyframeParams, keyframe_set),
        command!("keyframe.delete", "Delete the keyframe at a frame.", mutates: true, KeyframeDeleteParams,
        |s, p| {
            let tracks = &mut s.timeline.tracks;
            let Some(t) = tracks.iter_mut().find(|t| t.entity == p.id && t.property == p.property) else {
                return Err(CommandError::new("failed", "no such track"));
            };
            let n = t.keys.len();
            t.keys.retain(|k| (k.frame - p.frame).abs() >= 1e-4);
            if t.keys.len() == n { return Err(CommandError::new("failed", format!("no key at frame {}", p.frame))); }
            tracks.retain(|t| !t.keys.is_empty());
            Ok(Value::Null)
        }),
        command!("keyframe.list", "List animation tracks and their key frames.", mutates: false, KeyframeListParams,
            |s, p| Ok(json!(s.timeline.tracks.iter().filter(|t| p.id.as_ref().is_none_or(|id| &t.entity == id))
                .map(|t| json!({ "entity": t.entity, "property": t.property, "frames": t.keys.iter().map(|k| k.frame).collect::<Vec<_>>() }))
                .collect::<Vec<_>>()))),
        command!("shot.create", "Create a shot: a frame range seen through one camera.", mutates: true, ShotParams,
        |s, p| {
            s.camera(&p.camera)?;
            if p.end < p.start { return Err(CommandError::invalid("end must be >= start")); }
            let id = match p.id { Some(id) => { validate_id(&id)?; id } None => (1..).map(|n| format!("shot_{n}")).find(|i| !s.timeline.shots.iter().any(|x| &x.id == i)).unwrap_or_default() };
            if s.timeline.shots.iter().any(|x| x.id == id) { return Err(CommandError::invalid(format!("shot `{id}` exists"))); }
            s.timeline.shots.push(Shot { id: id.clone(), name: p.name.unwrap_or_else(|| id.clone()), camera: p.camera, start: p.start, end: p.end });
            s.timeline.shots.sort_by_key(|x| x.start);
            Ok(json!({ "id": id }))
        }),
        command!("shot.delete", "Delete a shot.", mutates: true, IdParams,
        |s, p| {
            let n = s.timeline.shots.len();
            s.timeline.shots.retain(|x| x.id != p.id);
            if s.timeline.shots.len() == n { return Err(CommandError::new("failed", format!("no shot `{}`", p.id))); }
            Ok(Value::Null)
        }),
    ]
    .into_iter()
    .chain(crate::placement::commands())
    .chain(crate::rig::commands())
    .chain(crate::humans::commands())
    .chain(crate::lights::commands())
    .chain(crate::environment::commands())
    .chain(crate::model::commands())
    .chain(crate::images::commands())
    .chain(crate::motion::commands())
    .chain(crate::templates::commands())
    .chain(crate::arrange::commands())
    .chain(crate::clipboard::commands())
    .chain(crate::meshedit::commands())
    .chain(crate::texture::commands())
    .collect()
}
