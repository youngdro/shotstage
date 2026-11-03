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
    if p.snap.unwrap_or(hand) {
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
        assert!(s.scene().get("ph").unwrap().parent.is_none());
    }
}
