//! Realistic people (MakeHuman / MPFB): building them, and changing body shape, clothes,
//! hair, facial expression and hand shapes. The skeleton shares the mannequin's joint names
//! (pelvis, spine, neck, head, shoulder / elbow / wrist, hip / knee / ankle), so `pose.set`,
//! motions, `person.lookAt` and hand-held props work the same on both; humans add a chest,
//! clavicles, toes and three joints per finger.

use std::collections::BTreeMap;

use glam::{Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::eval::local_at;
use shotstage_core::human::{self, EXPRESSION_UNITS, EXPRESSIONS, HumanBody, HumanOutfit, HumanPart, PARTS};
use shotstage_core::{
    Entity, EntityKind, ExpressionMix, Interp, Key, KeyValue, MeshSource, Scene, TrackProperty, Transform,
};

use crate::builtin::{color, frame_ok};
use crate::rig::person_root;
use crate::{CmdResult, Command, CommandError, command};

pub const SKIN: [f32; 3] = [0.82, 0.63, 0.52];
pub const HAIR: [f32; 3] = [0.16, 0.11, 0.08];

/// Colours of a human's parts.
#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct HumanColors {
    pub skin: Option<[f32; 3]>,
    pub top: Option<[f32; 3]>,
    pub bottom: Option<[f32; 3]>,
    pub shoes: Option<[f32; 3]>,
    pub hair: Option<[f32; 3]>,
    pub eyes: Option<[f32; 3]>,
    pub apron: Option<[f32; 3]>,
}

/// A ready-made person.
pub struct HumanPreset {
    pub body: HumanBody,
    pub outfit: HumanOutfit,
    pub top: [f32; 3],
    pub bottom: [f32; 3],
    pub shoes: [f32; 3],
    pub hair: [f32; 3],
    pub apron: [f32; 3],
}

fn preset(gender: f32, age: f32) -> HumanPreset {
    HumanPreset {
        body: HumanBody { gender, age, ..Default::default() },
        outfit: HumanOutfit::default(),
        top: [0.33, 0.42, 0.56],
        bottom: [0.22, 0.24, 0.3],
        shoes: [0.14, 0.14, 0.16],
        hair: HAIR,
        apron: [0.14, 0.3, 0.22],
    }
}

/// The human presets behind the realistic people prefabs.
pub fn preset_for(kind: crate::prefabs::PrefabKind) -> Option<HumanPreset> {
    use crate::prefabs::PrefabKind::*;
    Some(match kind {
        HumanMan => preset(1.0, 30.0),
        HumanWoman => HumanPreset {
            outfit: HumanOutfit { hair: 1.15, ..Default::default() },
            top: [0.72, 0.38, 0.4],
            bottom: [0.24, 0.24, 0.3],
            ..preset(0.0, 28.0)
        },
        HumanBoy => HumanPreset {
            outfit: HumanOutfit { legs: 0.8, ..Default::default() },
            top: [0.85, 0.62, 0.2],
            bottom: [0.3, 0.38, 0.52],
            ..preset(1.0, 11.0)
        },
        HumanGirl => HumanPreset {
            outfit: HumanOutfit { hair: 1.0, ..Default::default() },
            top: [0.55, 0.72, 0.62],
            bottom: [0.35, 0.3, 0.45],
            ..preset(0.0, 11.0)
        },
        HumanOldMan => HumanPreset {
            body: HumanBody { gender: 1.0, age: 72.0, weight: 0.25, ..Default::default() },
            outfit: HumanOutfit { sleeves: 2.0, hair: 0.3, ..Default::default() },
            top: [0.55, 0.5, 0.42],
            bottom: [0.3, 0.28, 0.25],
            hair: [0.72, 0.72, 0.7],
            ..preset(1.0, 72.0)
        },
        HumanOldWoman => HumanPreset {
            body: HumanBody { gender: 0.0, age: 70.0, weight: 0.2, ..Default::default() },
            outfit: HumanOutfit { sleeves: 2.0, hair: 0.6, ..Default::default() },
            top: [0.48, 0.36, 0.46],
            bottom: [0.28, 0.27, 0.3],
            hair: [0.78, 0.77, 0.75],
            ..preset(0.0, 70.0)
        },
        HumanAthlete => HumanPreset {
            body: HumanBody { gender: 1.0, age: 26.0, muscle: 0.85, weight: -0.2, height: 0.3, ..Default::default() },
            outfit: HumanOutfit { sleeves: 0.0, legs: 0.6, hair: 0.3, ..Default::default() },
            top: [0.85, 0.85, 0.87],
            bottom: [0.12, 0.12, 0.14],
            ..preset(1.0, 26.0)
        },
        HumanShopper => HumanPreset {
            body: HumanBody { gender: 0.0, age: 38.0, weight: 0.15, ..Default::default() },
            outfit: HumanOutfit { sleeves: 2.0, hair: 0.8, ..Default::default() },
            top: [0.62, 0.55, 0.45],
            bottom: [0.2, 0.22, 0.28],
            ..preset(0.0, 38.0)
        },
        HumanStaff => HumanPreset {
            outfit: HumanOutfit { sleeves: 0.35, hair: 0.4, apron: true, ..Default::default() },
            top: [0.86, 0.84, 0.78],
            bottom: [0.16, 0.16, 0.18],
            apron: [0.12, 0.26, 0.18],
            ..preset(1.0, 32.0)
        },
        HumanCashier => HumanPreset {
            body: HumanBody { gender: 0.0, age: 24.0, ..Default::default() },
            outfit: HumanOutfit { sleeves: 0.35, hair: 0.42, ..Default::default() },
            top: [0.7, 0.16, 0.16],
            bottom: [0.14, 0.14, 0.17],
            ..preset(0.0, 24.0)
        },
        HumanDeli => HumanPreset {
            body: HumanBody { gender: 1.0, age: 45.0, weight: 0.35, ..Default::default() },
            outfit: HumanOutfit { sleeves: 2.0, hair: 0.3, apron: true, ..Default::default() },
            top: [0.9, 0.9, 0.88],
            bottom: [0.2, 0.2, 0.22],
            apron: [0.92, 0.92, 0.9],
            ..preset(1.0, 45.0)
        },
        HumanCasual => HumanPreset {
            body: HumanBody { gender: 1.0, age: 22.0, weight: -0.1, ..Default::default() },
            outfit: HumanOutfit { sleeves: 0.35, legs: 1.0, hair: 0.75, ..Default::default() },
            top: [0.92, 0.78, 0.3],
            bottom: [0.55, 0.5, 0.4],
            shoes: [0.85, 0.85, 0.85],
            ..preset(1.0, 22.0)
        },
        _ => return None,
    })
}

fn part_color(p: &HumanPreset, part: HumanPart) -> [f32; 3] {
    match part {
        HumanPart::Skin => SKIN,
        HumanPart::Top => p.top,
        HumanPart::Bottom => p.bottom,
        HumanPart::Shoes => p.shoes,
        HumanPart::Hair => p.hair,
        HumanPart::Eyes => [0.86, 0.85, 0.82],
        HumanPart::Irises => [0.12, 0.08, 0.06],
        HumanPart::Teeth => [0.9, 0.88, 0.82],
        HumanPart::Apron => p.apron,
    }
}

/// Build a human under the (already inserted) empty `root`: joint entities and part meshes.
pub fn build(scene: &mut Scene, root: &str, p: &HumanPreset) -> CmdResult<Vec<String>> {
    p.body.validate()?;
    p.outfit.validate()?;
    let rest = human::rest(&p.body);
    let locals = rest.joint_locals();
    let skel = human::skeleton();
    let mut ids = vec![];
    let mut joints = vec![];
    for (i, (name, parent)) in skel.iter().enumerate() {
        let id = format!("{root}_{name}");
        let parent = parent.map(|q| format!("{root}_{}", skel[q].0)).unwrap_or_else(|| root.to_string());
        scene.insert(Entity {
            id: id.clone(),
            name: name.to_string(),
            parent: Some(parent),
            transform: Transform { translation: locals[i].to_array(), ..Default::default() },
            visible: true,
            locked: false,
            kind: EntityKind::Empty,
        })?;
        ids.push(id.clone());
        joints.push(id);
    }
    for part in PARTS {
        let id = format!("{root}_{}", part.name());
        scene.insert(Entity {
            id: id.clone(),
            name: part.name().to_string(),
            parent: Some(root.to_string()),
            transform: Transform::default(),
            visible: true,
            locked: false,
            kind: EntityKind::Mesh {
                source: MeshSource::Human {
                    body: p.body.clone(),
                    part: *part,
                    outfit: p.outfit.clone(),
                    joints: joints.clone(),
                },
                modifiers: vec![],
                color: part_color(p, *part),
            },
        })?;
        ids.push(id);
    }
    Ok(ids)
}

/// The part meshes of a human person (root id), with their body and outfit.
pub fn human_parts(scene: &Scene, root: &str) -> Vec<(String, HumanPart, HumanBody, HumanOutfit)> {
    scene
        .children(Some(root))
        .filter_map(|e| match &e.kind {
            EntityKind::Mesh { source: MeshSource::Human { body, part, outfit, .. }, .. } => {
                Some((e.id.clone(), *part, body.clone(), outfit.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Body and outfit of the human person `root` (None for a mannequin).
pub fn human_of(scene: &Scene, root: &str) -> Option<(HumanBody, HumanOutfit)> {
    human_parts(scene, root).into_iter().next().map(|(_, _, b, o)| (b, o))
}

pub fn human_root(scene: &Scene, id: &str) -> CmdResult<String> {
    let root = person_root(scene, id)?;
    if human_of(scene, &root).is_none() {
        return Err(CommandError::new(
            "failed",
            format!("`{root}` is a simple mannequin; this needs a realistic person (e.g. prefab humanMan)"),
        ));
    }
    Ok(root)
}

/// Add part meshes a person made by an older version lacks (the apron), so outfits can use them.
fn ensure_parts(scene: &mut Scene, root: &str) -> CmdResult<()> {
    let have = human_parts(scene, root);
    let Some((template, ..)) = have.first() else { return Ok(()) };
    let template = scene.get(template)?.clone();
    for part in PARTS {
        if have.iter().any(|(_, p, ..)| p == part) {
            continue;
        }
        let mut e = template.clone();
        let want = format!("{root}_{}", part.name());
        e.id = if scene.contains(&want) { scene.unique_id(&want) } else { want };
        e.name = part.name().to_string();
        if let EntityKind::Mesh { source: MeshSource::Human { part: p, .. }, color, .. } = &mut e.kind {
            *p = *part;
            *color = part_color(&preset(1.0, 30.0), *part);
        }
        scene.insert(e)?;
    }
    Ok(())
}

fn update_parts(scene: &mut Scene, root: &str, f: impl Fn(&mut HumanBody, &mut HumanOutfit)) -> CmdResult<()> {
    for (id, ..) in human_parts(scene, root) {
        if let EntityKind::Mesh { source: MeshSource::Human { body, outfit, .. }, .. } = &mut scene.get_mut(&id)?.kind {
            f(body, outfit);
            body.validate()?;
            outfit.validate()?;
        }
    }
    Ok(())
}

/// Move the joint entities to the rest positions of `body` (rotations are kept).
fn refit_joints(scene: &mut Scene, root: &str, body: &HumanBody) -> CmdResult<()> {
    let rest = human::rest(body);
    let locals = rest.joint_locals();
    for (i, (name, _)) in human::skeleton().iter().enumerate() {
        let id = format!("{root}_{name}");
        if let Ok(e) = scene.get_mut(&id) {
            e.transform.translation = locals[i].to_array();
        }
    }
    Ok(())
}

/// Rest height of a human's pelvis joint.
pub fn pelvis_rest(scene: &Scene, root: &str) -> Option<f32> {
    let (body, _) = human_of(scene, root)?;
    let rest = human::rest(&body);
    let i = human::skeleton().iter().position(|(n, _)| *n == "pelvis")?;
    Some(rest.joints[i].y)
}

// ---------------------------------------------------------------- hands

/// A hand shape: curl of each finger (degrees per joint) and of the thumb, and finger spread.
pub struct HandShape {
    pub name: &'static str,
    pub description: &'static str,
    /// index, middle, ring, pinky: degrees at the three joints.
    pub fingers: [[f32; 3]; 4],
    /// thumb: degrees at the three joints (towards the palm).
    pub thumb: [f32; 3],
    /// Fan the fingers apart, degrees.
    pub spread: f32,
}

const CURL: [f32; 3] = [75.0, 95.0, 60.0];
const RELAX: [f32; 3] = [14.0, 22.0, 12.0];
const STRAIGHT: [f32; 3] = [0.0; 3];

pub const HAND_SHAPES: &[HandShape] = &[
    HandShape {
        name: "relaxed",
        description: "Natural, slightly curled",
        fingers: [RELAX; 4],
        thumb: [8.0, 10.0, 8.0],
        spread: 2.0,
    },
    HandShape {
        name: "open",
        description: "Flat open hand, fingers together",
        fingers: [STRAIGHT; 4],
        thumb: [0.0; 3],
        spread: 0.0,
    },
    HandShape {
        name: "spread",
        description: "Fingers spread wide (showing, stop)",
        fingers: [STRAIGHT; 4],
        thumb: [-10.0, 0.0, 0.0],
        spread: 14.0,
    },
    HandShape { name: "fist", description: "Closed fist", fingers: [CURL; 4], thumb: [30.0, 40.0, 35.0], spread: 0.0 },
    HandShape {
        name: "grip",
        description: "Holding a cup, bottle, phone or handle",
        fingers: [[45.0, 50.0, 30.0], [50.0, 55.0, 30.0], [55.0, 55.0, 30.0], [60.0, 55.0, 30.0]],
        thumb: [25.0, 25.0, 15.0],
        spread: 0.0,
    },
    HandShape {
        name: "pinch",
        description: "Picking something small between thumb and index",
        fingers: [[40.0, 35.0, 25.0], [35.0, 45.0, 25.0], [40.0, 50.0, 25.0], [45.0, 50.0, 25.0]],
        thumb: [30.0, 25.0, 20.0],
        spread: 0.0,
    },
    HandShape {
        name: "point",
        description: "Index finger pointing",
        fingers: [STRAIGHT, CURL, CURL, CURL],
        thumb: [30.0, 40.0, 35.0],
        spread: 0.0,
    },
    HandShape { name: "thumbsUp", description: "Thumbs up", fingers: [CURL; 4], thumb: [-15.0, 0.0, 0.0], spread: 0.0 },
    HandShape {
        name: "peace",
        description: "V sign: index and middle up",
        fingers: [STRAIGHT, STRAIGHT, CURL, CURL],
        thumb: [30.0, 40.0, 35.0],
        spread: 10.0,
    },
    HandShape {
        name: "ok",
        description: "OK sign: thumb and index make a ring",
        fingers: [[45.0, 55.0, 35.0], [5.0, 5.0, 0.0], [8.0, 8.0, 0.0], [12.0, 10.0, 0.0]],
        thumb: [35.0, 30.0, 20.0],
        spread: 8.0,
    },
    HandShape {
        name: "claw",
        description: "Fingers bent like a claw (grabbing)",
        fingers: [[20.0, 60.0, 50.0]; 4],
        thumb: [15.0, 30.0, 25.0],
        spread: 8.0,
    },
];

const FINGERS: [&str; 4] = ["index", "middle", "ring", "pinky"];

/// Local rotations that curl the fingers of one hand into `shape` (in the hand's rest frame:
/// joint entities have no rest rotation, so the person's axes apply).
fn hand_rotations(scene: &Scene, root: &str, side: &str, shape: &HandShape) -> CmdResult<Vec<(String, Quat)>> {
    let (body, _) = human_of(scene, root).ok_or_else(|| CommandError::new("failed", "not a realistic person"))?;
    let rest = human::rest(&body);
    let skel = human::skeleton();
    let at = |n: &str| -> CmdResult<Vec3> {
        let name = format!("{n}_{side}");
        skel.iter()
            .position(|(s, _)| *s == name)
            .map(|i| rest.joints[i])
            .ok_or_else(|| CommandError::new("failed", format!("no joint {name}")))
    };
    let wrist = at("wrist")?;
    let (index, middle, pinky) = (at("index_01")?, at("middle_01")?, at("pinky_01")?);
    let along = (middle - wrist).normalize();
    let sign = if side == "l" { 1.0 } else { -1.0 };
    // the palm faces the body at rest: towards -X for the left hand
    let mut palm = (index - pinky).cross(along).normalize() * sign;
    if palm.x * sign > 0.0 {
        palm = -palm;
    }
    let mut out = vec![];
    for (f, finger) in FINGERS.iter().enumerate() {
        let a = at(&format!("{finger}_01"))?;
        let b = at(&format!("{finger}_02"))?;
        let dir = (b - a).normalize_or(along);
        let axis = dir.cross(palm).normalize_or(Vec3::X);
        // spread: fan around the palm normal, index and pinky most
        let fan = [1.0, 0.25, -0.4, -1.0][f] * shape.spread.to_radians();
        for (k, deg) in shape.fingers[f].iter().enumerate() {
            let mut q = Quat::from_axis_angle(axis, deg.to_radians());
            if k == 0 {
                q *= Quat::from_axis_angle(palm, fan * -sign);
            }
            out.push((format!("{root}_{finger}_0{}_{side}", k + 1), q));
        }
    }
    let (t1, t2) = (at("thumb_01")?, at("thumb_02")?);
    let tdir = (t2 - t1).normalize_or(along);
    // the thumb folds across the palm towards the little finger
    let across = (pinky - index).normalize_or(Vec3::Z);
    let taxis = tdir.cross((palm + across).normalize_or(palm)).normalize_or(Vec3::X);
    for (k, deg) in shape.thumb.iter().enumerate() {
        out.push((format!("{root}_thumb_0{}_{side}", k + 1), Quat::from_axis_angle(taxis, deg.to_radians())));
    }
    Ok(out)
}

/// Where a realistic person's palm is at `frame` (world): its centre and the way it faces.
pub fn palm(scene: &Scene, root: &str, side: &str, frame: f32) -> CmdResult<(Vec3, Vec3)> {
    let (body, _) = human_of(scene, root).ok_or_else(|| CommandError::new("failed", "not a realistic person"))?;
    let rest = human::rest(&body);
    let skel = human::skeleton();
    let at = |n: &str| -> CmdResult<Vec3> {
        let name = format!("{n}_{side}");
        skel.iter()
            .position(|(s, _)| *s == name)
            .map(|i| rest.joints[i])
            .ok_or_else(|| CommandError::new("failed", format!("no joint {name}")))
    };
    let (wrist, index, middle, pinky) = (at("wrist")?, at("index_01")?, at("middle_01")?, at("pinky_01")?);
    let along = (middle - wrist).normalize();
    let sign = if side == "l" { 1.0 } else { -1.0 };
    let mut n = (index - pinky).cross(along).normalize() * sign;
    if n.x * sign > 0.0 {
        n = -n;
    }
    // the hand's rest frame is the wrist joint's (identity at rest), so carry the rest palm along
    let w = shotstage_core::eval::world_at(scene, &format!("{root}_wrist_{side}"), frame)?;
    let (_, rot, _) = w.to_scale_rotation_translation();
    let centre_rest = wrist.lerp(middle, 0.55) + n * 0.012;
    let centre = w.transform_point3(centre_rest - wrist);
    Ok((centre, (rot * n).normalize_or(Vec3::Y)))
}

/// Shape one or both hands of a realistic person (mannequins have no fingers: skipped).
pub fn set_hands(
    scene: &mut Scene,
    root: &str,
    shape: &str,
    side: HandSide,
    frame: f32,
    keyframe_at: Option<f32>,
) -> CmdResult<Vec<String>> {
    let shape = HAND_SHAPES.iter().find(|h| h.name == shape).ok_or_else(|| {
        CommandError::invalid(format!(
            "no hand shape `{shape}`; use one of {}",
            HAND_SHAPES.iter().map(|h| h.name).collect::<Vec<_>>().join(", ")
        ))
    })?;
    if human_of(scene, root).is_none() {
        return Ok(vec![]);
    }
    let sides: &[&str] = match side {
        HandSide::Both => &["l", "r"],
        HandSide::Left => &["l"],
        HandSide::Right => &["r"],
    };
    let mut done = vec![];
    for s in sides {
        for (id, q) in hand_rotations(scene, root, s, shape)? {
            if scene.get(&id).is_err() {
                continue;
            }
            let mut t = local_at(scene, &id, frame)?;
            t.rotation = q.normalize().to_array();
            crate::placement::store_pose(scene, &id, frame, keyframe_at, t)?;
            done.push(id);
        }
    }
    Ok(done)
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Default, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum HandSide {
    #[default]
    Both,
    Left,
    Right,
}

// ---------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BodyParams {
    /// The person (root, joint or part).
    id: String,
    /// 0 = female … 1 = male.
    gender: Option<f32>,
    /// Years, 11 … 90.
    age: Option<f32>,
    /// -1 thin … 0 average … 1 heavy.
    weight: Option<f32>,
    /// -1 slight … 0 average … 1 muscular.
    muscle: Option<f32>,
    /// -1 short … 0 average … 1 tall.
    height: Option<f32>,
}

fn set_body(scene: &mut Scene, p: BodyParams) -> CmdResult<Value> {
    let root = human_root(scene, &p.id)?;
    update_parts(scene, &root, |b, _| {
        if let Some(v) = p.gender {
            b.gender = v;
        }
        if let Some(v) = p.age {
            b.age = v;
        }
        if let Some(v) = p.weight {
            b.weight = v;
        }
        if let Some(v) = p.muscle {
            b.muscle = v;
        }
        if let Some(v) = p.height {
            b.height = v;
        }
    })?;
    let (body, _) = human_of(scene, &root).expect("checked");
    refit_joints(scene, &root, &body)?;
    let frame = scene.timeline.start as f32;
    crate::placement::drop_down(scene, &root, frame, None)?;
    Ok(json!({ "id": root, "body": body, "heightM": human::rest(&body).height() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OutfitParams {
    /// The person (root, joint or part).
    id: String,
    /// Wear a top.
    top: Option<bool>,
    /// 0 sleeveless, 0.35 short sleeves, 1 elbow, 2 long sleeves.
    sleeves: Option<f32>,
    /// Wear trousers / shorts.
    bottom: Option<bool>,
    /// 0 briefs, 0.6 shorts, 1 knee, 2 full-length trousers.
    legs: Option<f32>,
    shoes: Option<bool>,
    /// Hair length: 0 none, 0.3 cropped, 0.5 short, 0.75 chin, 1 shoulder, 1.4 long.
    hair: Option<f32>,
    /// Wear a bib apron.
    apron: Option<bool>,
    /// Part colours, linear RGB 0..1.
    colors: Option<HumanColors>,
}

fn set_outfit(scene: &mut Scene, p: OutfitParams) -> CmdResult<Value> {
    let root = human_root(scene, &p.id)?;
    ensure_parts(scene, &root)?;
    update_parts(scene, &root, |_, o| {
        if let Some(v) = p.top {
            o.top = v;
        }
        if let Some(v) = p.sleeves {
            o.sleeves = v;
        }
        if let Some(v) = p.bottom {
            o.bottom = v;
        }
        if let Some(v) = p.legs {
            o.legs = v;
        }
        if let Some(v) = p.shoes {
            o.shoes = v;
        }
        if let Some(v) = p.hair {
            o.hair = v;
        }
        if let Some(v) = p.apron {
            o.apron = v;
        }
    })?;
    if let Some(c) = &p.colors {
        for (id, part, ..) in human_parts(scene, &root) {
            let want = match part {
                HumanPart::Skin => c.skin,
                HumanPart::Top => c.top,
                HumanPart::Bottom => c.bottom,
                HumanPart::Shoes => c.shoes,
                HumanPart::Hair => c.hair,
                HumanPart::Eyes => None,
                HumanPart::Irises => c.eyes,
                HumanPart::Teeth => None,
                HumanPart::Apron => c.apron,
            };
            if let Some(v) = want {
                let v = color(v)?;
                if let EntityKind::Mesh { color, .. } = &mut scene.get_mut(&id)?.kind {
                    *color = v;
                }
            }
        }
        // hair pieces (ponytail, bun) follow the hair colour
        if let Some(h) = c.hair {
            for id in hair_pieces(scene, &root) {
                if let EntityKind::Mesh { color, .. } = &mut scene.get_mut(&id)?.kind {
                    *color = h;
                }
            }
        }
    }
    let (_, outfit) = human_of(scene, &root).expect("checked");
    Ok(json!({ "id": root, "outfit": outfit }))
}

/// Extra hair pieces (ponytail, bun) hanging off a human's head joint.
fn hair_pieces(scene: &Scene, root: &str) -> Vec<String> {
    let head = format!("{root}_head");
    scene.children(Some(&head)).filter(|e| e.name.starts_with("hair")).map(|e| e.id.clone()).collect()
}

/// `person.setHair` for realistic people: the hair volume cut to a length, plus a ponytail or
/// bun piece.
pub fn set_human_hair(
    scene: &mut Scene,
    root: &str,
    style: crate::rig::HairStyle,
    hair_color: Option<[f32; 3]>,
) -> CmdResult<Value> {
    use crate::rig::HairStyle::*;
    let len = match style {
        Short => 0.45,
        Long => 1.4,
        Bob => 0.8,
        Ponytail | Bun => 0.42,
        None => 0.0,
    };
    let hair_id = format!("{root}_hair");
    let current = match &scene.get(&hair_id)?.kind {
        EntityKind::Mesh { color, .. } => *color,
        _ => HAIR,
    };
    let c = color(hair_color.unwrap_or(current))?;
    update_parts(scene, root, |_, o| o.hair = len)?;
    if let EntityKind::Mesh { color, .. } = &mut scene.get_mut(&hair_id)?.kind {
        *color = c;
    }
    for id in hair_pieces(scene, root) {
        scene.remove(&id)?;
    }
    let (body, _) = human_of(scene, root).expect("a human");
    let k = human::rest(&body).height() / 1.76;
    let mut ids = vec![hair_id];
    let pieces = if matches!(style, Ponytail | Bun) { crate::rig::hair(style, k) } else { vec![] };
    for part in pieces.into_iter().filter(|p| p.name == "hair_back") {
        let id = format!("{root}_{}", part.name);
        // the mannequin's head joint sits lower on the skull than the human's
        let mut pos = part.pos;
        pos[1] -= 0.02 * k;
        pos[2] -= 0.02 * k;
        let mut t = Transform { translation: pos, scale: part.scale, ..Default::default() };
        t.set_euler_degrees(part.rot);
        scene.insert(Entity {
            id: id.clone(),
            name: part.name.into(),
            parent: Some(format!("{root}_head")),
            transform: t,
            visible: true,
            locked: false,
            kind: EntityKind::Mesh {
                source: MeshSource::Primitive { primitive: part.prim.expect("hair is geometry") },
                modifiers: vec![],
                color: c,
            },
        })?;
        ids.push(id);
    }
    Ok(json!({ "id": root, "parts": ids }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExpressionParams {
    /// The person (root, joint or part).
    id: String,
    /// A named expression (see `person.expressions`).
    expression: Option<String>,
    /// How strong, 0 … 1.5 (default 1).
    amount: Option<f32>,
    /// Raw expression units with weights, added on top of `expression` (not with
    /// `keyframeAt`).
    units: Option<BTreeMap<String, f32>>,
    /// Keyframe the named expression at this frame: between keys the face cross-fades from
    /// one expression to the next. Once a person has expression keys, their face follows them.
    keyframe_at: Option<f32>,
}

fn set_expression(scene: &mut Scene, p: ExpressionParams) -> CmdResult<Value> {
    let root = human_root(scene, &p.id)?;
    let amount = p.amount.unwrap_or(1.0);
    if !(amount.is_finite() && (0.0..=1.5).contains(&amount)) {
        return Err(CommandError::invalid("amount must be 0 … 1.5"));
    }
    let mut face: BTreeMap<String, f32> = BTreeMap::new();
    if let Some(name) = &p.expression {
        let e = EXPRESSIONS.iter().find(|e| e.name == name).ok_or_else(|| {
            CommandError::invalid(format!(
                "no expression `{name}`; use one of {}",
                EXPRESSIONS.iter().map(|e| e.name).collect::<Vec<_>>().join(", ")
            ))
        })?;
        for (u, w) in e.units {
            face.insert(u.to_string(), w * amount);
        }
    }
    for (u, w) in p.units.iter().flatten() {
        *face.entry(u.clone()).or_default() += *w;
    }
    if p.expression.is_none() && p.units.is_none() {
        return Err(CommandError::invalid("give `expression` and/or `units`"));
    }
    face.retain(|_, w| w.abs() > 1e-4);
    if let Some(f) = p.keyframe_at {
        let f = frame_ok(f)?;
        let (Some(name), None) = (&p.expression, &p.units) else {
            return Err(CommandError::invalid("keyframes take a named `expression` (no raw `units`)"));
        };
        let index = shotstage_core::human::expression_index(name).unwrap_or(0);
        scene.timeline.track_mut_or_insert(&root, TrackProperty::Expression).set_key(Key {
            frame: f,
            value: KeyValue::Expression(ExpressionMix::single(index, amount)),
            interp: Interp::default(),
        });
    }
    update_parts(scene, &root, |b, _| b.face = face.clone())?;
    let keys = scene.timeline.track(&root, TrackProperty::Expression).map_or(0, |t| t.keys.len());
    Ok(json!({ "id": root, "units": face, "keys": keys }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct HandParams {
    /// The person (root, joint or part).
    id: String,
    /// Hand shape (see `pose.list`): relaxed, open, spread, fist, grip, pinch, point,
    /// thumbsUp, peace, ok, claw.
    shape: String,
    /// Which hand (default both).
    #[serde(default)]
    hand: HandSide,
    frame: Option<f32>,
    keyframe_at: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PersonOnly {
    /// The person (root, joint or part).
    id: String,
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("person.setBody", "Change a realistic person's body: gender (0 female … 1 male), age in years (11 … 90), weight, muscle and height (-1 … 1). Joints follow; the feet stay on the ground.", mutates: true, BodyParams, set_body),
        command!("person.setOutfit", "Change a realistic person's clothes and hair length (top + sleeve length, trousers / shorts + leg length, shoes, hair) and part colours.", mutates: true, OutfitParams, set_outfit),
        command!("person.setExpression", "Give a realistic person a facial expression (smile, laugh, surprised, angry, sad, talking… see person.expressions), optionally with raw expression units. With keyframeAt the expression is keyed on the timeline and the face cross-fades between keys.", mutates: true, ExpressionParams, set_expression),
        command!("person.expressions", "Named facial expressions and the raw expression units person.setExpression accepts.", mutates: false, crate::builtin::NoParams,
        |_, _| Ok(json!({
            "expressions": EXPRESSIONS.iter().map(|e| json!({ "name": e.name, "description": e.description })).collect::<Vec<_>>(),
            "units": EXPRESSION_UNITS,
        }))),
        command!("pose.hand", "Shape a realistic person's hand(s): relaxed, open, spread, fist, grip, pinch, point, thumbsUp, peace, ok, claw.", mutates: true, HandParams,
        |s, p| {
            let root = human_root(s, &p.id)?;
            let frame = frame_ok(p.frame.unwrap_or(s.timeline.start as f32))?;
            let joints = set_hands(s, &root, &p.shape, p.hand, frame, p.keyframe_at)?;
            Ok(json!({ "id": root, "joints": joints }))
        }),
        command!("person.get", "A person's kind (mannequin or realistic), and for realistic people the body, outfit, expression and height.", mutates: false, PersonOnly,
        |s, p| {
            let root = person_root(s, &p.id)?;
            Ok(match human_of(s, &root) {
                Some((body, outfit)) => json!({
                    "id": root, "kind": "human", "heightM": human::rest(&body).height(),
                    "body": body, "outfit": outfit,
                    "joints": human::skeleton().iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                }),
                None => json!({ "id": root, "kind": "mannequin", "joints": crate::rig::JOINTS }),
            })
        }),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    fn world(s: &Session, id: &str) -> glam::Vec3 {
        shotstage_core::eval::world_at(s.scene(), id, 0.0).unwrap().w_axis.truncate()
    }

    #[test]
    fn realistic_person_poses_like_the_mannequin() {
        let mut s = Session::default();
        let r = s.dispatch("prefab.create", json!({"kind": "humanMan", "id": "h"})).unwrap().result;
        assert!(r["parts"].as_array().unwrap().len() > 50);
        let b = s.dispatch("entity.bounds", json!({"id": "h"})).unwrap().result;
        let size = b["size"][1].as_f64().unwrap();
        assert!((1.6..1.9).contains(&size), "height {size}");
        assert!(b["min"][1].as_f64().unwrap().abs() < 0.02, "feet on the ground: {b}");
        let hand = world(&s, "h_wrist_r");
        s.dispatch("pose.set", json!({"id": "h", "pose": "wave"})).unwrap();
        assert!(world(&s, "h_wrist_r").y > hand.y + 0.6, "the right hand goes up");
        s.dispatch("pose.set", json!({"id": "h", "pose": "sit"})).unwrap();
        let b = s.dispatch("entity.bounds", json!({"id": "h"})).unwrap().result;
        assert!(b["size"][1].as_f64().unwrap() < 1.45, "sitting is lower: {b}");
        let got = s.dispatch("person.get", json!({"id": "h_head"})).unwrap().result;
        assert_eq!(got["kind"], "human");
    }

    #[test]
    fn expressions_keyframe_and_cross_fade() {
        let mut s = Session::default();
        s.dispatch("prefab.create", json!({"kind": "humanMan", "id": "h"})).unwrap();
        s.dispatch("person.setExpression", json!({"id": "h_head", "expression": "neutral", "keyframeAt": 0})).unwrap();
        let r = s
            .dispatch("person.setExpression", json!({"id": "h", "expression": "smile", "keyframeAt": 20}))
            .unwrap()
            .result;
        assert_eq!(r["keys"], 2);
        // raw units cannot be keyed
        assert!(
            s.dispatch("person.setExpression", json!({"id": "h", "units": {"mouth-open": 0.5}, "keyframeAt": 5}))
                .is_err()
        );
        let mouth = |s: &Session, f: f32| {
            let m = shotstage_core::eval::posed_mesh(s.scene(), "h_skin", f).unwrap();
            m.positions.iter().map(|p| p[0] * 3.1 + p[1] * 1.7 + p[2] * 0.3).sum::<f32>()
        };
        let (a, b, c) = (mouth(&s, 0.0), mouth(&s, 10.0), mouth(&s, 20.0));
        assert!((a - c).abs() > 1e-3, "the face changes between keys");
        assert!((b - a) * (c - b) > 0.0, "and passes through in between");
        // re-key whatever shows at a frame, from any part
        s.dispatch("keyframe.set", json!({"id": "h_skin", "property": "expression", "frame": 40})).unwrap();
        let keys = &s.scene().timeline.track("h", shotstage_core::TrackProperty::Expression).unwrap().keys;
        assert_eq!(keys.len(), 3);
        // duplicates carry their expression track along
        s.dispatch("entity.duplicate", json!({"id": "h"})).unwrap();
        assert_eq!(
            s.scene()
                .timeline
                .tracks
                .iter()
                .filter(|t| t.property == shotstage_core::TrackProperty::Expression)
                .count(),
            2
        );
    }

    #[test]
    fn holding_a_prop_grips_it_and_aprons_come_and_go() {
        let mut s = Session::default();
        s.dispatch("prefab.create", json!({"kind": "humanMan", "id": "h"})).unwrap();
        s.dispatch("pose.set", json!({"id": "h", "pose": "stand"})).unwrap();
        let before = s.scene().get("h_index_02_r").unwrap().transform.rotation;
        s.dispatch("prefab.create", json!({"kind": "cup", "id": "c", "position": [1, 0, 0]})).unwrap();
        s.dispatch("entity.attach", json!({"id": "c", "to": "h", "hand": "right"})).unwrap();
        let b = s.dispatch("entity.bounds", json!({"id": "c"})).unwrap().result;
        let c = glam::Vec3::new(
            b["center"][0].as_f64().unwrap() as f32,
            b["center"][1].as_f64().unwrap() as f32,
            b["center"][2].as_f64().unwrap() as f32,
        );
        assert!(c.distance(world(&s, "h_wrist_r")) < 0.15, "in the hand: {c}");
        assert_ne!(s.scene().get("h_index_02_r").unwrap().transform.rotation, before, "fingers close");
        // an apron, also on a person saved before aprons existed
        s.dispatch("entity.delete", json!({"id": "h_apron"})).unwrap();
        s.dispatch("person.setOutfit", json!({"id": "h", "apron": true, "colors": {"apron": [0.2, 0.4, 0.3]}}))
            .unwrap();
        let m = shotstage_core::eval::posed_mesh(s.scene(), "h_apron", 0.0).unwrap();
        assert!(m.positions.len() > 100);
        s.dispatch("person.setOutfit", json!({"id": "h", "apron": false})).unwrap();
        assert!(shotstage_core::eval::posed_mesh(s.scene(), "h_apron", 0.0).unwrap().positions.is_empty());
    }

    #[test]
    fn body_outfit_expression_and_hands() {
        let mut s = Session::default();
        s.dispatch("prefab.create", json!({"kind": "humanWoman", "id": "w"})).unwrap();
        let tall = s.dispatch("person.setBody", json!({"id": "w", "height": 1.0})).unwrap().result;
        let short = s.dispatch("person.setBody", json!({"id": "w", "height": -1.0, "age": 60})).unwrap().result;
        assert!(tall["heightM"].as_f64().unwrap() > short["heightM"].as_f64().unwrap() + 0.1);
        let b = s.dispatch("entity.bounds", json!({"id": "w"})).unwrap().result;
        assert!(b["min"][1].as_f64().unwrap().abs() < 0.02, "still on the ground after reshaping: {b}");
        s.dispatch("person.setOutfit", json!({"id": "w", "sleeves": 2, "colors": {"top": [0.1, 0.2, 0.3]}})).unwrap();
        s.dispatch("person.setExpression", json!({"id": "w", "expression": "smile"})).unwrap();
        assert!(s.dispatch("person.setExpression", json!({"id": "w", "expression": "nope"})).is_err());
        let tip = world(&s, "w_index_03_r");
        s.dispatch("pose.hand", json!({"id": "w", "shape": "fist", "hand": "right"})).unwrap();
        assert!((world(&s, "w_index_03_r") - tip).length() > 0.01, "the finger curls");
        s.dispatch("person.setHair", json!({"id": "w", "style": "ponytail"})).unwrap();
        assert!(s.scene().contains("w_hair_back"));
        // everything draws
        for e in &s.scene().entities {
            shotstage_core::eval::posed_tris(s.scene(), &e.id, 0.0).ok();
        }
        // copy / paste keeps the skin bound to the copy's joints
        let d = s.dispatch("entity.duplicate", json!({"id": "w"})).unwrap().result;
        let copy = d["id"].as_str().unwrap().to_string();
        match &s.scene().get(&format!("{copy}_skin")).unwrap().kind {
            shotstage_core::EntityKind::Mesh { source: shotstage_core::MeshSource::Human { joints, .. }, .. } => {
                assert!(joints.iter().all(|j| j.starts_with(&copy)), "{joints:?}");
            }
            _ => panic!("skin"),
        }
        assert!(s.dispatch("person.setBody", json!({"id": "w", "age": 5})).is_err());
    }

    #[test]
    fn mannequins_are_unchanged() {
        let mut s = Session::default();
        s.dispatch("prefab.create", json!({"kind": "man", "id": "m"})).unwrap();
        assert_eq!(s.dispatch("person.get", json!({"id": "m"})).unwrap().result["kind"], "mannequin");
        assert!(s.dispatch("person.setBody", json!({"id": "m", "age": 40})).is_err());
    }
}
