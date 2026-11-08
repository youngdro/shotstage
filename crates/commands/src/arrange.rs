//! Working with several objects at once (group, ungroup, align, distribute) and making a person
//! look at something.

use glam::{Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::eval::{local_at, world_at};
use shotstage_core::spatial;
use shotstage_core::{Entity, EntityKind, Scene, Transform};

use crate::builtin::{Target, target_point};
use crate::rig;
use crate::{CmdResult, Command, CommandError, command};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
enum Edge {
    Min,
    Center,
    Max,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GroupParams {
    /// Entities to put in one group (their children come along).
    ids: Vec<String>,
    name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdOnly {
    id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AlignParams {
    ids: Vec<String>,
    /// x, y or z (world).
    axis: Axis,
    /// Line up their min edges, centres or max edges (on the group's extent).
    to: Edge,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DistributeParams {
    /// Three or more entities: the outer two stay, the others are spaced evenly between them.
    ids: Vec<String>,
    axis: Axis,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShotSetParams {
    /// Shot id.
    id: String,
    name: Option<String>,
    /// Camera the shot looks through.
    camera: Option<String>,
    /// First frame.
    start: Option<u32>,
    /// Last frame.
    end: Option<u32>,
    /// Notes for the crew (action, dialogue, camera move); "" clears them.
    notes: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CutParams {
    /// Camera to cut to.
    camera: String,
    /// Frame where the new shot starts.
    frame: u32,
    /// Name of a new shot (default: its id); a shot whose camera is switched keeps its name.
    name: Option<String>,
}

/// Cut to `camera` at `frame`: split the shot playing there (the new shot keeps its end), switch
/// the camera of a shot that starts exactly there, or fill the gap up to the next shot.
fn cut(scene: &mut Scene, p: CutParams) -> CmdResult<Value> {
    scene.camera(&p.camera)?;
    let f = p.frame;
    let t = &mut scene.timeline;
    let fresh = |t: &shotstage_core::Timeline| {
        (1..).map(|n| format!("shot_{n}")).find(|i| !t.shots.iter().any(|x| &x.id == i)).unwrap_or_default()
    };
    if let Some(sh) = t.shots.iter_mut().find(|sh| sh.start == f) {
        // switching keeps the shot's name
        sh.camera = p.camera;
        return Ok(json!({ "id": sh.id, "start": sh.start, "end": sh.end, "split": false }));
    }
    let id = fresh(t);
    let end = match t.shots.iter_mut().find(|sh| sh.start < f && f <= sh.end) {
        Some(sh) => {
            let end = sh.end;
            sh.end = f - 1;
            end
        }
        None => {
            let next = t.shots.iter().map(|sh| sh.start).filter(|s| *s > f).min();
            // past the last shot: to the end of the timeline, or three seconds beyond it
            next.map(|n| n - 1).unwrap_or(if f < t.end { t.end } else { f + (t.fps * 3.0).round() as u32 })
        }
    };
    t.shots.push(shotstage_core::Shot {
        id: id.clone(),
        name: p.name.unwrap_or_else(|| id.clone()),
        camera: p.camera,
        start: f,
        end,
        notes: String::new(),
    });
    t.shots.sort_by_key(|x| x.start);
    if end > t.end {
        t.end = end;
    }
    Ok(json!({ "id": id, "start": f, "end": end, "split": true }))
}

fn shot_set(scene: &mut Scene, p: ShotSetParams) -> CmdResult<Value> {
    if let Some(c) = &p.camera {
        scene.camera(c)?;
    }
    let i = scene
        .timeline
        .shots
        .iter()
        .position(|s| s.id == p.id)
        .ok_or_else(|| CommandError::new("failed", format!("no shot `{}`", p.id)))?;
    let mut shot = scene.timeline.shots[i].clone();
    if let Some(n) = p.name {
        if n.is_empty() || n.len() > 200 {
            return Err(CommandError::invalid("name must be 1-200 bytes"));
        }
        shot.name = n;
    }
    if let Some(c) = p.camera {
        shot.camera = c;
    }
    if let Some(n) = p.notes {
        if n.len() > 4000 {
            return Err(CommandError::invalid("notes must be at most 4000 bytes"));
        }
        shot.notes = n;
    }
    shot.start = p.start.unwrap_or(shot.start);
    shot.end = p.end.unwrap_or(shot.end);
    if shot.end < shot.start || shot.end - shot.start > 100_000 {
        return Err(CommandError::invalid("need start <= end"));
    }
    scene.timeline.shots[i] = shot.clone();
    scene.timeline.shots.sort_by_key(|s| s.start);
    // the timeline grows to fit
    if shot.end > scene.timeline.end {
        scene.timeline.end = shot.end;
    }
    Ok(serde_json::to_value(shot).unwrap_or(Value::Null))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AttachParams {
    /// The prop (its group) to attach.
    id: String,
    /// What it follows: a person's hand (`<person>_wrist_r`, or the person with `hand`), or any
    /// entity. Null detaches it (it stays where it is).
    to: Option<String>,
    /// With a person as `to`: `right` (default) or `left`.
    hand: Option<String>,
    /// Move the prop into the hand (its centre at the palm). Default true for hands.
    snap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LookParams {
    /// The person (root, joint or part).
    id: String,
    /// Entity id (e.g. the camera) or world point to look at.
    target: Target,
    frame: Option<f32>,
    keyframe_at: Option<f32>,
}

fn unique(ids: &[String]) -> CmdResult<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for id in ids {
        if out.contains(id) {
            return Err(CommandError::invalid(format!("`{id}` is listed twice")));
        }
        out.push(id.clone());
    }
    Ok(out)
}

/// Set an entity's local transform so its world matrix stays `world` under its (new) parent.
fn keep_world(scene: &mut Scene, id: &str, world: Mat4) -> CmdResult<()> {
    let parent = match scene.get(id)?.parent.clone() {
        Some(p) => scene.world_matrix(&p)?,
        None => Mat4::IDENTITY,
    };
    let (s, r, t) = (parent.inverse() * world).to_scale_rotation_translation();
    scene.get_mut(id)?.transform = Transform::from_parts(t, r, s);
    Ok(())
}

fn group(scene: &mut Scene, p: GroupParams) -> CmdResult<Value> {
    let ids = unique(&p.ids)?;
    if ids.is_empty() {
        return Err(CommandError::invalid("give at least one id"));
    }
    for id in &ids {
        scene.get(id)?;
        // no entity inside another listed one
        if ids.iter().any(|o| o != id && spatial::subtree(scene, o).contains(id)) {
            return Err(CommandError::invalid(format!("`{id}` is already inside another listed entity")));
        }
    }
    let frame = scene.timeline.start as f32;
    let b =
        ids.iter().map(|id| spatial::bounds(scene, id, frame).ok().flatten()).reduce(spatial::Aabb::union).flatten();
    let origin = b.map(|b| Vec3::new(b.center().x, b.min.y, b.center().z)).unwrap_or(Vec3::ZERO);
    let name = p.name.unwrap_or_else(|| "group".into());
    let gid = scene.unique_id(&name.to_lowercase());
    scene.insert(Entity {
        id: gid.clone(),
        name,
        parent: None,
        transform: Transform::from_parts(origin, Quat::IDENTITY, Vec3::ONE),
        visible: true,
        locked: false,
        kind: EntityKind::Empty,
    })?;
    for id in &ids {
        let w = scene.world_matrix(id)?;
        scene.set_parent(id, Some(&gid))?;
        keep_world(scene, id, w)?;
    }
    Ok(json!({ "id": gid, "children": ids }))
}

fn ungroup(scene: &mut Scene, p: IdOnly) -> CmdResult<Value> {
    let g = scene.get(&p.id)?.clone();
    if !matches!(g.kind, EntityKind::Empty) {
        return Err(CommandError::invalid(format!("`{}` is not a group", p.id)));
    }
    if rig::person_root(scene, &p.id).is_ok_and(|r| r == p.id) || rig::is_joint(scene, &p.id) {
        return Err(CommandError::invalid("a person's rig cannot be ungrouped"));
    }
    let kids: Vec<String> = scene.children(Some(&p.id)).map(|e| e.id.clone()).collect();
    for k in &kids {
        let w = scene.world_matrix(k)?;
        scene.set_parent(k, g.parent.as_deref())?;
        keep_world(scene, k, w)?;
    }
    scene.timeline.tracks.retain(|t| t.entity != p.id);
    scene.entities.retain(|e| e.id != p.id);
    Ok(json!({ "children": kids }))
}

/// Move `id` by a world-space delta (at the timeline start, keyed when animated).
fn shift(scene: &mut Scene, id: &str, delta: Vec3) -> CmdResult<()> {
    let frame = scene.timeline.start as f32;
    let world = world_at(scene, id, frame)?.w_axis.truncate();
    let local = match scene.get(id)?.parent.clone() {
        Some(par) => world_at(scene, &par, frame)?.inverse().transform_point3(world + delta),
        None => world + delta,
    };
    let mut t = local_at(scene, id, frame)?;
    t.translation = local.to_array();
    crate::placement::store_pose(scene, id, frame, None, t)?;
    Ok(())
}

fn boxes(scene: &Scene, ids: &[String]) -> CmdResult<Vec<(String, spatial::Aabb)>> {
    let frame = scene.timeline.start as f32;
    ids.iter()
        .map(|id| {
            let b = spatial::bounds(scene, id, frame)?
                .ok_or_else(|| CommandError::new("failed", format!("`{id}` has no size")))?;
            Ok((id.clone(), b))
        })
        .collect()
}

fn align(scene: &mut Scene, p: AlignParams) -> CmdResult<Value> {
    let ids = unique(&p.ids)?;
    if ids.len() < 2 {
        return Err(CommandError::invalid("align needs two or more ids"));
    }
    let bs = boxes(scene, &ids)?;
    let i = p.axis.index();
    let lo = bs.iter().map(|(_, b)| b.min[i]).fold(f32::MAX, f32::min);
    let hi = bs.iter().map(|(_, b)| b.max[i]).fold(f32::MIN, f32::max);
    for (id, b) in &bs {
        let d = match p.to {
            Edge::Min => lo - b.min[i],
            Edge::Max => hi - b.max[i],
            Edge::Center => (lo + hi) / 2.0 - b.center()[i],
        };
        let mut delta = Vec3::ZERO;
        delta[i] = d;
        shift(scene, id, delta)?;
    }
    Ok(json!({ "moved": ids.len() }))
}

fn distribute(scene: &mut Scene, p: DistributeParams) -> CmdResult<Value> {
    let ids = unique(&p.ids)?;
    if ids.len() < 3 {
        return Err(CommandError::invalid("distribute needs three or more ids"));
    }
    let i = p.axis.index();
    let mut bs = boxes(scene, &ids)?;
    bs.sort_by(|a, b| a.1.center()[i].total_cmp(&b.1.center()[i]));
    let (a, z) = (bs[0].1.center()[i], bs[bs.len() - 1].1.center()[i]);
    let n = bs.len() - 1;
    for (k, (id, b)) in bs.iter().enumerate().skip(1).take(n - 1) {
        let mut delta = Vec3::ZERO;
        delta[i] = a + (z - a) * k as f32 / n as f32 - b.center()[i];
        shift(scene, id, delta)?;
    }
    Ok(json!({ "moved": n - 1 }))
}

fn attach(scene: &mut Scene, p: AttachParams) -> CmdResult<Value> {
    scene.get(&p.id)?;
    let frame = scene.timeline.start as f32;
    let Some(to) = p.to else {
        let w = scene.world_matrix(&p.id)?;
        scene.set_parent(&p.id, None)?;
        keep_world(scene, &p.id, w)?;
        return Ok(json!({ "id": p.id, "parent": Value::Null }));
    };
    // a person means one of their hands
    let to = match rig::person_root(scene, &to) {
        Ok(root) if rig::joint_name(scene, &to).is_none() => {
            let side = match p.hand.as_deref().unwrap_or("right") {
                "left" | "l" => "l",
                "right" | "r" => "r",
                other => return Err(CommandError::invalid(format!("hand must be left or right, not `{other}`"))),
            };
            format!("{root}_wrist_{side}")
        }
        _ => to,
    };
    scene.get(&to)?;
    if spatial::subtree(scene, &p.id).contains(&to) {
        return Err(CommandError::invalid("cannot attach something to itself or its child"));
    }
    let hand = rig::joint_name(scene, &to).is_some_and(|j| j.starts_with("wrist"));
    let w = scene.world_matrix(&p.id)?;
    scene.set_parent(&p.id, Some(&to))?;
    keep_world(scene, &p.id, w)?;
    let human = rig::person_root(scene, &to).ok().filter(|r| crate::humans::human_of(scene, r).is_some());
    if let (true, Some(root)) = (hand && p.snap.unwrap_or(true), human) {
        // a realistic hand: rest the prop against the palm and close the fingers round it
        let side = if to.ends_with("_l") { "l" } else { "r" };
        let (centre, normal) = crate::humans::palm(scene, &root, side, frame)?;
        if let Some(b) = spatial::bounds(scene, &p.id, frame)? {
            let size = b.size();
            let thin = size.min_element();
            let reach = (thin * 0.5).clamp(0.008, 0.05);
            shift(scene, &p.id, centre + normal * reach - b.center())?;
            let shape = if size.max_element() < 0.05 { "pinch" } else { "grip" };
            let side = if side == "l" { crate::humans::HandSide::Left } else { crate::humans::HandSide::Right };
            crate::humans::set_hands(scene, &root, shape, side, frame, None)?;
        }
    } else if p.snap.unwrap_or(hand) {
        // put the prop's centre at the palm: the middle of the hand mesh
        let palm = scene
            .children(Some(&to))
            .find(|e| e.name.starts_with("hand"))
            .map(|e| e.id.clone())
            .and_then(|h| spatial::bounds(scene, &h, frame).ok().flatten())
            .map(|b| b.center())
            .unwrap_or(world_at(scene, &to, frame)?.w_axis.truncate());
        if let Some(b) = spatial::bounds(scene, &p.id, frame)? {
            shift(scene, &p.id, palm - b.center())?;
        }
    }
    Ok(json!({ "id": p.id, "parent": to }))
}

fn look(scene: &mut Scene, p: LookParams) -> CmdResult<Value> {
    let root = rig::person_root(scene, &p.id)?;
    let frame = crate::builtin::frame_ok(p.frame.unwrap_or(scene.timeline.start as f32))?;
    let target = target_point(scene, &p.target, frame)?;
    let head = world_at(scene, &format!("{root}_head"), frame)?.w_axis.truncate();
    // the direction in the chest's frame (spine joint), so the head turns relative to the body
    let spine_rot = world_at(scene, &format!("{root}_spine"), frame)?.to_scale_rotation_translation().1;
    let d = (spine_rot.inverse() * (target - head)).normalize_or_zero();
    if d == Vec3::ZERO {
        return Err(CommandError::invalid("the target is at the head"));
    }
    let yaw = d.x.atan2(d.z).to_degrees().clamp(-80.0, 80.0);
    let pitch = (-d.y.clamp(-1.0, 1.0).asin().to_degrees()).clamp(-40.0, 45.0);
    let mut req = json!({
        "id": root, "frame": frame, "drop": false,
        "joints": { "neck": [pitch * 0.35, yaw * 0.35, 0.0], "head": [pitch * 0.65, yaw * 0.65, 0.0] },
    });
    if let Some(k) = p.keyframe_at {
        req["keyframeAt"] = json!(k);
    }
    let set = crate::registry().into_iter().find(|c| c.id == "pose.set").expect("registered");
    (set.run)(scene, req)?;
    Ok(json!({ "yawDegrees": yaw, "pitchDegrees": pitch }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("entity.group", "Put several entities in a new group (an empty at their bottom centre); they keep their place.", mutates: true, GroupParams, group),
        command!("entity.ungroup", "Dissolve a group: its children move up a level and keep their place.", mutates: true, IdOnly, ungroup),
        command!("entity.align", "Line up entities' bounds on a world axis: min edges, centres or max edges.", mutates: true, AlignParams, align),
        command!("entity.distribute", "Space three or more entities evenly along a world axis (the outer two stay).", mutates: true, DistributeParams, distribute),
        command!("entity.attach", "Make a prop follow something: a person's hand (it moves with their poses and actions), or any entity; null detaches it. Hands snap the prop to the palm.", mutates: true, AttachParams, attach),
        command!("shot.cut", "Cut to a camera at a frame: splits the shot playing there (or switches the camera of a shot starting there, or fills the gap to the next shot).", mutates: true, CutParams, cut),
        command!("shot.set", "Change a shot: its name, camera, first and last frame (retime it), notes for the crew.", mutates: true, ShotSetParams, shot_set),
        command!("person.lookAt", "Turn a person's head and neck towards an entity (e.g. the camera) or a point.", mutates: true, LookParams, look),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_align_distribute_look() {
        let mut s = crate::Session::default();
        for (id, x, z) in [("a", 0.0, 0.0), ("b", 2.0, 0.7), ("c", 7.0, -0.4)] {
            s.dispatch("prefab.create", json!({"kind": "crate", "id": id, "position": [x, 0, z]})).unwrap();
        }
        let r = s.dispatch("entity.group", json!({"ids": ["a", "b"], "name": "Pair"})).unwrap().result;
        let g = r["id"].as_str().unwrap().to_string();
        let before = world_at(s.scene(), "b", 0.0).unwrap();
        s.dispatch("entity.translate", json!({"id": g, "delta": [1, 0, 0]})).unwrap();
        assert!(
            (world_at(s.scene(), "b", 0.0).unwrap().w_axis.x - before.w_axis.x - 1.0).abs() < 1e-4,
            "children move with the group"
        );
        s.dispatch("entity.ungroup", json!({"id": g})).unwrap();
        assert!(s.scene().get("b").unwrap().parent.is_none());
        assert!(
            (world_at(s.scene(), "b", 0.0).unwrap().w_axis.x - before.w_axis.x - 1.0).abs() < 1e-4,
            "keeps its place"
        );
        s.dispatch("entity.align", json!({"ids": ["a", "b", "c"], "axis": "z", "to": "center"})).unwrap();
        let z = |s: &crate::Session, id: &str| spatial::bounds(s.scene(), id, 0.0).unwrap().unwrap().center().z;
        assert!((z(&s, "a") - z(&s, "c")).abs() < 1e-4 && (z(&s, "b") - z(&s, "c")).abs() < 1e-4);
        s.dispatch("entity.distribute", json!({"ids": ["a", "b", "c"], "axis": "x"})).unwrap();
        let x = |s: &crate::Session, id: &str| spatial::bounds(s.scene(), id, 0.0).unwrap().unwrap().center().x;
        assert!(((x(&s, "b") - x(&s, "a")) - (x(&s, "c") - x(&s, "b"))).abs() < 1e-3);
        // a person looks to their left (+X) at a point
        s.dispatch("prefab.create", json!({"kind": "man", "id": "m", "position": [0, 0, 5]})).unwrap();
        let r = s.dispatch("person.lookAt", json!({"id": "m", "target": [5, 1.6, 5]})).unwrap().result;
        assert!(r["yawDegrees"].as_f64().unwrap() > 60.0, "{r}");
        assert!(s.dispatch("entity.ungroup", json!({"id": "m"})).is_err());
        // a phone in the right hand follows the hand when the arm moves
        s.dispatch("prefab.create", json!({"kind": "phone", "id": "ph", "position": [3, 0, 3]})).unwrap();
        let r = s.dispatch("entity.attach", json!({"id": "ph", "to": "m"})).unwrap().result;
        assert_eq!(r["parent"], "m_wrist_r");
        let before = spatial::bounds(s.scene(), "ph", 0.0).unwrap().unwrap().center();
        s.dispatch("pose.set", json!({"id": "m", "pose": "phone"})).unwrap();
        let after = spatial::bounds(s.scene(), "ph", 0.0).unwrap().unwrap().center();
        assert!(after.y > before.y + 0.3, "the phone rose with the hand: {before} -> {after}");
        s.dispatch("entity.attach", json!({"id": "ph", "to": null})).unwrap();
        // shots can be retimed, renamed and moved to another camera
        s.dispatch("entity.create", json!({"type": "camera", "id": "c1"})).unwrap();
        s.dispatch("entity.create", json!({"type": "camera", "id": "c2"})).unwrap();
        s.dispatch("shot.create", json!({"camera": "c1", "start": 0, "end": 47, "id": "s1"})).unwrap();
        s.dispatch("shot.set", json!({"id": "s1", "end": 200, "camera": "c2", "name": "wide"})).unwrap();
        // cutting: split inside a shot, switch at its first frame, fill a gap
        {
            let mut c = crate::Session::default();
            for cam in ["c1", "c2"] {
                c.dispatch("entity.create", json!({"type": "camera", "id": cam})).unwrap();
            }
            c.dispatch("shot.create", json!({"camera": "c2", "start": 0, "end": 200, "id": "s1"})).unwrap();
            c.dispatch("shot.cut", json!({"camera": "c1", "frame": 100})).unwrap();
            let shots = &c.scene().timeline.shots;
            assert_eq!((shots[0].end, shots[1].start, shots[1].end, shots[1].camera.as_str()), (99, 100, 200, "c1"));
            let r = c.dispatch("shot.cut", json!({"camera": "c2", "frame": 100})).unwrap().result;
            assert_eq!(r["split"], false);
            assert_eq!(c.scene().timeline.shots[1].camera, "c2");
            c.dispatch("shot.cut", json!({"camera": "c1", "frame": 250})).unwrap();
            assert_eq!(c.scene().timeline.shots.len(), 3);
        }
        let sh = &s.scene().timeline.shots[0];
        assert_eq!((sh.end, sh.camera.as_str(), sh.name.as_str()), (200, "c2", "wide"));
        assert!(s.scene().timeline.end >= 200, "the timeline grows");
        assert!(s.dispatch("shot.set", json!({"id": "s1", "start": 300})).is_err());
        assert!(s.dispatch("shot.set", json!({"id": "s1", "camera": "m"})).is_err());
        assert!(s.scene().get("ph").unwrap().parent.is_none());
    }
}
