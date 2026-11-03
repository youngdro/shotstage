//! Posable people. A person prefab is a joint hierarchy of empties; body parts are meshes
//! hanging off their joints with the pivot at the joint. Conventions (figure facing +Z, +X is
//! its left): limbs hang along -Y; arms forward / thighs up = -X rotation; knees bend with +X;
//! elbows bend with -X; leaning the spine or nodding the head forward = +X; raising the left arm
//! sideways = +Z, the right arm = -Z.

use std::collections::BTreeMap;

use glam::{Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::eval::{local_at, world_at};
use shotstage_core::{Entity, EntityKind, MeshSource, Primitive, Scene, Transform};

use crate::builtin::{finite3, frame_ok};
use crate::prefabs::Part;
use crate::{CmdResult, Command, CommandError, command};

/// Every joint of a person, parents first.
pub const JOINTS: &[&str] = &[
    "pelvis",
    "spine",
    "neck",
    "head",
    "shoulder_l",
    "elbow_l",
    "wrist_l",
    "shoulder_r",
    "elbow_r",
    "wrist_r",
    "hip_l",
    "knee_l",
    "ankle_l",
    "hip_r",
    "knee_r",
    "ankle_r",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Body {
    Man,
    Woman,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum HairStyle {
    Short,
    Long,
    Bob,
    Ponytail,
    Bun,
    None,
}

pub const HAIR_STYLES: &[&str] = &["short", "long", "bob", "ponytail", "bun", "none"];

const SKIN: [f32; 3] = [0.86, 0.68, 0.55];
const HAIR: [f32; 3] = [0.2, 0.14, 0.1];
const NOSE_FRONT: f32 = 0.1;

fn capsule(r: f32, h: f32) -> Primitive {
    Primitive::Capsule { radius: r, height: h.max(2.0 * r + 0.001), segments: 12 }
}

fn ball() -> Primitive {
    Primitive::Sphere { radius: 0.5, segments: 14, rings: 8 }
}

fn joint(name: &'static str, parent: Option<&'static str>, pos: [f32; 3]) -> Part {
    Part {
        name,
        parent,
        prim: None,
        pos,
        rot: [0.0; 3],
        scale: [1.0; 3],
        color: [1.0; 3],
        modifiers: vec![],
        light: None,
    }
}

fn mesh(
    name: &'static str,
    parent: &'static str,
    prim: Primitive,
    pos: [f32; 3],
    scale: [f32; 3],
    color: [f32; 3],
) -> Part {
    Part {
        name,
        parent: Some(parent),
        prim: Some(prim),
        pos,
        rot: [0.0; 3],
        scale,
        color,
        modifiers: vec![],
        light: None,
    }
}

/// Hair parts for a style, children of the `head` joint.
pub fn hair(style: HairStyle, k: f32) -> Vec<Part> {
    let s = |v: [f32; 3]| v.map(|x| x * k);
    let cap = mesh("hair", "head", ball(), s([0.0, 0.14, -0.012]), s([0.182, 0.13, 0.215]), HAIR);
    match style {
        HairStyle::None => vec![],
        HairStyle::Short => vec![cap],
        HairStyle::Long => {
            vec![cap, mesh("hair_back", "head", ball(), s([0.0, 0.0, -0.075]), s([0.19, 0.36, 0.1]), HAIR)]
        }
        HairStyle::Bob => {
            vec![cap, mesh("hair_back", "head", ball(), s([0.0, 0.07, -0.025]), s([0.205, 0.21, 0.2]), HAIR)]
        }
        HairStyle::Ponytail => vec![
            cap,
            Part {
                rot: [25.0, 0.0, 0.0],
                ..mesh("hair_back", "head", capsule(0.035 * k, 0.22 * k), s([0.0, 0.03, -0.14]), [1.0; 3], HAIR)
            },
        ],
        HairStyle::Bun => {
            vec![cap, mesh("hair_back", "head", ball(), s([0.0, 0.18, -0.09]), s([0.11, 0.1, 0.1]), HAIR)]
        }
    }
}

/// A person with joints, body parts and hair, feet on the ground at the origin.
pub fn person(body: Body) -> Vec<Part> {
    let man = body == Body::Man;
    let k = if man { 1.0 } else { 0.94 };
    let s = |v: [f32; 3]| v.map(|x| x * k);
    let (shirt, pants) =
        if man { ([0.33, 0.42, 0.56], [0.22, 0.24, 0.3]) } else { ([0.72, 0.38, 0.4], [0.24, 0.24, 0.3]) };
    let shoulder_x = if man { 0.2 } else { 0.18 };
    let hip_x = if man { 0.095 } else { 0.1 };
    let mut v = vec![
        joint("pelvis", None, s([0.0, 0.95, 0.0])),
        mesh(
            "hips",
            "pelvis",
            ball(),
            s([0.0, 0.0, 0.0]),
            s(if man { [0.34, 0.24, 0.22] } else { [0.37, 0.25, 0.23] }),
            pants,
        ),
        joint("spine", Some("pelvis"), s([0.0, 0.08, 0.0])),
        mesh(
            "belly",
            "spine",
            ball(),
            s([0.0, 0.12, 0.0]),
            s(if man { [0.31, 0.3, 0.2] } else { [0.28, 0.3, 0.19] }),
            shirt,
        ),
        mesh(
            "chest",
            "spine",
            ball(),
            s([0.0, 0.3, 0.0]),
            s(if man { [0.4, 0.36, 0.23] } else { [0.34, 0.34, 0.24] }),
            shirt,
        ),
        joint("neck", Some("spine"), s([0.0, 0.46, 0.0])),
        mesh("neck_part", "neck", capsule(0.055 * k, 0.14 * k), s([0.0, 0.05, 0.0]), [1.0; 3], SKIN),
        joint("head", Some("neck"), s([0.0, 0.09, 0.0])),
        mesh("skull", "head", ball(), s([0.0, 0.075, 0.0]), s([0.165, 0.23, 0.2]), SKIN),
        mesh("nose", "head", Primitive::Cube { size: [0.03, 0.045, 0.04] }, s([0.0, 0.07, NOSE_FRONT]), [k; 3], SKIN),
    ];
    for (side, x) in [("l", 1.0f32), ("r", -1.0)] {
        let n = |a: &'static str, b: &'static str| if side == "l" { a } else { b };
        v.push(joint(n("shoulder_l", "shoulder_r"), Some("spine"), s([x * shoulder_x, 0.42, 0.0])));
        v.push(mesh(
            n("upper_arm_l", "upper_arm_r"),
            n("shoulder_l", "shoulder_r"),
            capsule(0.05 * k, 0.32 * k),
            s([0.0, -0.14, 0.0]),
            [1.0; 3],
            shirt,
        ));
        v.push(joint(n("elbow_l", "elbow_r"), Some(n("shoulder_l", "shoulder_r")), s([0.0, -0.29, 0.0])));
        v.push(mesh(
            n("forearm_l", "forearm_r"),
            n("elbow_l", "elbow_r"),
            capsule(0.042 * k, 0.29 * k),
            s([0.0, -0.13, 0.0]),
            [1.0; 3],
            SKIN,
        ));
        v.push(joint(n("wrist_l", "wrist_r"), Some(n("elbow_l", "elbow_r")), s([0.0, -0.26, 0.0])));
        v.push(mesh(
            n("hand_l", "hand_r"),
            n("wrist_l", "wrist_r"),
            ball(),
            s([0.0, -0.08, 0.0]),
            s([0.035, 0.17, 0.085]),
            SKIN,
        ));
        v.push(joint(n("hip_l", "hip_r"), Some("pelvis"), s([x * hip_x, -0.04, 0.0])));
        v.push(mesh(
            n("thigh_l", "thigh_r"),
            n("hip_l", "hip_r"),
            capsule(0.075 * k, 0.5 * k),
            s([0.0, -0.21, 0.0]),
            [1.0; 3],
            pants,
        ));
        v.push(joint(n("knee_l", "knee_r"), Some(n("hip_l", "hip_r")), s([0.0, -0.43, 0.0])));
        v.push(mesh(
            n("shin_l", "shin_r"),
            n("knee_l", "knee_r"),
            capsule(0.056 * k, 0.46 * k),
            s([0.0, -0.21, 0.0]),
            [1.0; 3],
            pants,
        ));
        v.push(joint(n("ankle_l", "ankle_r"), Some(n("knee_l", "knee_r")), s([0.0, -0.42, 0.0])));
        v.push(mesh(
            n("foot_l", "foot_r"),
            n("ankle_l", "ankle_r"),
            ball(),
            s([0.0, -0.025, 0.06]),
            s([0.1, 0.08, 0.26]),
            [0.15, 0.15, 0.17],
        ));
    }
    v.extend(hair(if man { HairStyle::Short } else { HairStyle::Long }, k));
    // joints before the parts that hang off them, and every parent before its children
    let order = |p: &Part| JOINTS.iter().position(|j| *j == p.name).unwrap_or(usize::MAX);
    let (mut joints, parts): (Vec<Part>, Vec<Part>) = v.into_iter().partition(|p| p.prim.is_none());
    joints.sort_by_key(order);
    joints.extend(parts);
    joints
}

// ---------------------------------------------------------------- poses

/// Joint rotations of a named pose, in degrees `[x, y, z]`; unlisted joints are straight.
/// `pelvis_drop` lowers the hips (metres for a 1.76 m figure) for sitting and crouching.
pub struct Pose {
    pub name: &'static str,
    pub description: &'static str,
    pub joints: &'static [(&'static str, [f32; 3])],
    pub pelvis_drop: f32,
}

pub const POSES: &[Pose] = &[
    Pose {
        name: "stand",
        description: "Relaxed standing, arms down",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("shoulder_r", [0.0, 0.0, -6.0]),
            ("elbow_l", [-8.0, 0.0, 0.0]),
            ("elbow_r", [-8.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "tPose",
        description: "Arms straight out (for checking the rig)",
        pelvis_drop: 0.0,
        joints: &[("shoulder_l", [0.0, 0.0, 90.0]), ("shoulder_r", [0.0, 0.0, -90.0])],
    },
    Pose {
        name: "walk",
        description: "Mid-stride, left leg forward",
        pelvis_drop: 0.02,
        joints: &[
            ("hip_l", [-24.0, 0.0, 0.0]),
            ("knee_l", [8.0, 0.0, 0.0]),
            ("hip_r", [16.0, 0.0, 0.0]),
            ("knee_r", [28.0, 0.0, 0.0]),
            ("ankle_r", [-12.0, 0.0, 0.0]),
            ("shoulder_l", [18.0, 0.0, 6.0]),
            ("shoulder_r", [-20.0, 0.0, -6.0]),
            ("elbow_l", [-12.0, 0.0, 0.0]),
            ("elbow_r", [-25.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "run",
        description: "Running stride",
        pelvis_drop: 0.08,
        joints: &[
            ("spine", [12.0, 0.0, 0.0]),
            ("hip_l", [-55.0, 0.0, 0.0]),
            ("knee_l", [65.0, 0.0, 0.0]),
            ("hip_r", [25.0, 0.0, 0.0]),
            ("knee_r", [85.0, 0.0, 0.0]),
            ("shoulder_l", [45.0, 0.0, 8.0]),
            ("elbow_l", [-80.0, 0.0, 0.0]),
            ("shoulder_r", [-50.0, 0.0, -8.0]),
            ("elbow_r", [-95.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "sit",
        description: "Sitting on a chair (seat ≈ 0.45 m), hands on the thighs",
        pelvis_drop: 0.41,
        joints: &[
            ("hip_l", [-88.0, 0.0, 4.0]),
            ("hip_r", [-88.0, 0.0, -4.0]),
            ("knee_l", [88.0, 0.0, 0.0]),
            ("knee_r", [88.0, 0.0, 0.0]),
            ("shoulder_l", [-25.0, 0.0, 6.0]),
            ("shoulder_r", [-25.0, 0.0, -6.0]),
            ("elbow_l", [-45.0, 0.0, 0.0]),
            ("elbow_r", [-45.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "crouch",
        description: "Crouching down (to a low shelf or crate)",
        pelvis_drop: 0.42,
        joints: &[
            ("spine", [30.0, 0.0, 0.0]),
            ("neck", [-15.0, 0.0, 0.0]),
            ("hip_l", [-110.0, 0.0, 6.0]),
            ("hip_r", [-110.0, 0.0, -6.0]),
            ("knee_l", [135.0, 0.0, 0.0]),
            ("knee_r", [135.0, 0.0, 0.0]),
            ("ankle_l", [-25.0, 0.0, 0.0]),
            ("ankle_r", [-25.0, 0.0, 0.0]),
            ("shoulder_l", [-45.0, 0.0, 5.0]),
            ("shoulder_r", [-45.0, 0.0, -5.0]),
            ("elbow_l", [-30.0, 0.0, 0.0]),
            ("elbow_r", [-30.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "wave",
        description: "Waving with the right hand",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("shoulder_r", [0.0, 0.0, -130.0]),
            ("elbow_r", [0.0, 0.0, -50.0]),
            ("head", [0.0, -10.0, 0.0]),
        ],
    },
    Pose {
        name: "point",
        description: "Pointing ahead with the right arm",
        pelvis_drop: 0.0,
        joints: &[("shoulder_l", [0.0, 0.0, 6.0]), ("shoulder_r", [-85.0, 0.0, -8.0])],
    },
    Pose {
        name: "reach",
        description: "Reaching up to a high shelf with the right hand",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [-4.0, 0.0, 0.0]),
            ("neck", [-15.0, 0.0, 0.0]),
            ("shoulder_r", [-160.0, 0.0, -6.0]),
            ("elbow_r", [-10.0, 0.0, 0.0]),
            ("shoulder_l", [0.0, 0.0, 6.0]),
        ],
    },
    Pose {
        name: "pickUp",
        description: "Bending forward to pick from a crate or freezer",
        pelvis_drop: 0.03,
        joints: &[
            ("spine", [55.0, 0.0, 0.0]),
            ("neck", [-20.0, 0.0, 0.0]),
            ("hip_l", [-20.0, 0.0, 0.0]),
            ("hip_r", [-20.0, 0.0, 0.0]),
            ("knee_l", [20.0, 0.0, 0.0]),
            ("knee_r", [20.0, 0.0, 0.0]),
            ("shoulder_l", [-60.0, 0.0, 8.0]),
            ("shoulder_r", [-60.0, 0.0, -8.0]),
        ],
    },
    Pose {
        name: "holdBasket",
        description: "Carrying a basket on the bent left arm",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-10.0, 0.0, 12.0]),
            ("elbow_l", [-80.0, 0.0, 0.0]),
            ("shoulder_r", [0.0, 0.0, -6.0]),
            ("elbow_r", [-8.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "pushCart",
        description: "Both arms forward on a trolley handle",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [10.0, 0.0, 0.0]),
            ("shoulder_l", [-55.0, 0.0, 4.0]),
            ("shoulder_r", [-55.0, 0.0, -4.0]),
            ("elbow_l", [-35.0, 0.0, 0.0]),
            ("elbow_r", [-35.0, 0.0, 0.0]),
            ("hip_l", [-10.0, 0.0, 0.0]),
            ("hip_r", [8.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "phone",
        description: "Phone at the right ear",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_r", [-30.0, 0.0, -35.0]),
            ("elbow_r", [-145.0, 0.0, 0.0]),
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("head", [0.0, 0.0, -8.0]),
        ],
    },
    Pose {
        name: "armsCrossed",
        description: "Arms folded across the chest",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-15.0, 0.0, 8.0]),
            ("shoulder_r", [-15.0, 0.0, -8.0]),
            ("elbow_l", [-95.0, -70.0, 0.0]),
            ("elbow_r", [-95.0, 70.0, 0.0]),
        ],
    },
    Pose {
        name: "present",
        description: "Presenting a product with both hands, palms up",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-35.0, 0.0, 10.0]),
            ("shoulder_r", [-35.0, 0.0, -10.0]),
            ("elbow_l", [-60.0, 0.0, 0.0]),
            ("elbow_r", [-60.0, 0.0, 0.0]),
            ("wrist_l", [0.0, -60.0, 0.0]),
            ("wrist_r", [0.0, 60.0, 0.0]),
            ("neck", [12.0, 0.0, 0.0]),
        ],
    },
];

/// The root of the group `id` belongs to.
pub fn root_of(scene: &Scene, id: &str) -> String {
    let mut cur = id.to_string();
    for _ in 0..10_000 {
        match scene.get(&cur).ok().and_then(|e| e.parent.clone()) {
            Some(p) => cur = p,
            None => break,
        }
    }
    cur
}

/// The person root `id` belongs to (a person, one of its joints or body parts).
pub fn person_root(scene: &Scene, id: &str) -> CmdResult<String> {
    scene.get(id)?;
    let root = root_of(scene, id);
    if scene.contains(&format!("{root}_pelvis")) && scene.contains(&format!("{root}_head")) {
        Ok(root)
    } else {
        Err(CommandError::new("failed", format!("`{id}` is not part of a person (prefab man / woman)")))
    }
}

/// If `id` is a joint of a person, its joint name (`elbow_l`).
pub fn joint_name(scene: &Scene, id: &str) -> Option<&'static str> {
    let root = person_root(scene, id).ok()?;
    let rest = id.strip_prefix(&format!("{root}_"))?;
    let j = JOINTS.iter().find(|j| **j == rest)?;
    matches!(scene.get(id).ok()?.kind, EntityKind::Empty).then_some(*j)
}

/// For a body part mesh hanging off a person's joint, that joint's entity id.
pub fn joint_of(scene: &Scene, id: &str) -> Option<String> {
    if joint_name(scene, id).is_some() {
        return Some(id.to_string());
    }
    let parent = scene.get(id).ok()?.parent.clone()?;
    joint_name(scene, &parent).map(|_| parent)
}

/// Joints of imported skeletons: every entity a skinned mesh follows.
pub fn skin_joints(scene: &Scene) -> std::collections::HashSet<String> {
    scene
        .entities
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Mesh { source: MeshSource::Skinned { skin, .. }, .. } => Some(skin.joints.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// A joint of a person prefab or of an imported skeleton.
pub fn is_joint(scene: &Scene, id: &str) -> bool {
    joint_name(scene, id).is_some() || skin_joints(scene).contains(id)
}

/// The joint nearest to a world point on a skinned mesh (measured to each bone, from a joint to
/// its first child joint).
pub fn joint_near(scene: &Scene, mesh: &str, point: Vec3, frame: f32) -> Option<String> {
    let EntityKind::Mesh { source: MeshSource::Skinned { skin, .. }, .. } = &scene.get(mesh).ok()?.kind else {
        return None;
    };
    let pos = |id: &str| world_at(scene, id, frame).ok().map(|m| m.w_axis.truncate());
    let mut best: Option<(f32, String)> = None;
    for j in &skin.joints {
        let Some(a) = pos(j) else { continue };
        let child = scene.children(Some(j)).find(|c| skin.joints.contains(&c.id)).and_then(|c| pos(&c.id));
        let d = match child {
            Some(b) => {
                let ab = b - a;
                let t = ((point - a).dot(ab) / ab.length_squared().max(1e-9)).clamp(0.0, 1.0);
                (a + ab * t - point).length()
            }
            None => (a - point).length(),
        };
        if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, j.clone()));
        }
    }
    best.map(|(_, j)| j)
}

/// Rest height of the pelvis, from the leg chain (works for any body size).
fn pelvis_rest(scene: &Scene, root: &str) -> CmdResult<f32> {
    let y = |j: &str| -> CmdResult<f32> { Ok(scene.get(&format!("{root}_{j}"))?.transform.translation[1]) };
    let knee = y("knee_l")?;
    let scale = (knee.abs() / 0.43).max(1e-3);
    Ok(-(y("hip_l")? + knee + y("ankle_l")?) + 0.06 * scale)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PoseParams {
    /// The person (its root, a joint or a body part).
    id: String,
    /// A named pose (see `pose.list`); every joint not in it is straightened.
    pose: Option<String>,
    /// Joint rotations in degrees `[x, y, z]` (local, see `pose.list` for the conventions),
    /// applied after `pose`, e.g. `{"elbow_r": [-90, 0, 0]}`.
    joints: Option<BTreeMap<String, [f32; 3]>>,
    /// Frame to evaluate at. Default: the timeline start.
    frame: Option<f32>,
    /// Write keyframes at this frame (animated joints are always keyed).
    keyframe_at: Option<f32>,
    /// Re-rest the person on the surface below afterwards. Default true.
    drop: Option<bool>,
}

fn pose_set(scene: &mut Scene, p: PoseParams) -> CmdResult<Value> {
    let root = person_root(scene, &p.id)?;
    let frame = frame_ok(p.frame.unwrap_or(scene.timeline.start as f32))?;
    let mut set: BTreeMap<&str, [f32; 3]> = BTreeMap::new();
    if let Some(name) = &p.pose {
        let pose = POSES.iter().find(|q| q.name == name).ok_or_else(|| {
            CommandError::invalid(format!(
                "no pose `{name}`; use one of {}",
                POSES.iter().map(|q| q.name).collect::<Vec<_>>().join(", ")
            ))
        })?;
        for j in JOINTS {
            set.insert(j, [0.0; 3]);
        }
        for (j, r) in pose.joints {
            set.insert(j, *r);
        }
        let rest = pelvis_rest(scene, &root)?;
        let k = rest / 0.95;
        let pelvis = format!("{root}_pelvis");
        let mut t = local_at(scene, &pelvis, frame)?;
        t.translation = [0.0, rest - pose.pelvis_drop * k, 0.0];
        crate::placement::store_pose(scene, &pelvis, frame, p.keyframe_at, t)?;
    }
    for (j, r) in p.joints.iter().flatten() {
        let name = JOINTS
            .iter()
            .find(|n| **n == j.as_str())
            .ok_or_else(|| CommandError::invalid(format!("no joint `{j}`; joints are {}", JOINTS.join(", "))))?;
        finite3(*r, j)?;
        set.insert(name, *r);
    }
    if set.is_empty() {
        return Err(CommandError::invalid("give `pose` and/or `joints`"));
    }
    for (j, r) in &set {
        let id = format!("{root}_{j}");
        let mut t = local_at(scene, &id, frame)?;
        t.set_euler_degrees(*r);
        crate::placement::store_pose(scene, &id, frame, p.keyframe_at, t)?;
    }
    if p.drop.unwrap_or(true) {
        crate::placement::drop_down(scene, &root, frame, p.keyframe_at)?;
    }
    Ok(json!({ "id": root, "joints": set.keys().collect::<Vec<_>>() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AimParams {
    /// A joint of a person (e.g. `man_1_elbow_r`), or a body part hanging off one.
    id: String,
    /// World point the limb should point at.
    target: [f32; 3],
    frame: Option<f32>,
    keyframe_at: Option<f32>,
}

/// Direction (in the joint's local space) its limb points: towards its child joint, else
/// towards the body part hanging off it.
fn bone_direction(scene: &Scene, joint: &str) -> Vec3 {
    let kids: Vec<&Entity> = scene.children(Some(joint)).collect();
    let pick = kids
        .iter()
        .find(|e| matches!(e.kind, EntityKind::Empty))
        .or_else(|| kids.iter().find(|e| !e.name.starts_with("hair") && e.name != "nose"))
        .map(|e| e.transform.translation());
    // person limbs hang along -Y; imported (Blender) bones point along their +Y
    let rest = if joint_name(scene, joint).is_some() { Vec3::NEG_Y } else { Vec3::Y };
    pick.map(|v| v.normalize_or_zero()).filter(|v| *v != Vec3::ZERO).unwrap_or(rest)
}

fn pose_aim(scene: &mut Scene, p: AimParams) -> CmdResult<Value> {
    let joint =
        joint_of(scene, &p.id).or_else(|| skin_joints(scene).contains(&p.id).then(|| p.id.clone())).ok_or_else(
            || CommandError::new("failed", format!("`{}` is not a joint of a person or of an imported skeleton", p.id)),
        )?;
    let frame = frame_ok(p.frame.unwrap_or(scene.timeline.start as f32))?;
    let target = finite3(p.target, "target")?;
    let world = world_at(scene, &joint, frame)?;
    let (_, rot, pos) = world.to_scale_rotation_translation();
    let want = (target - pos).normalize_or_zero();
    if want == Vec3::ZERO {
        return Err(CommandError::invalid("the target is at the joint"));
    }
    let now = (rot * bone_direction(scene, &joint)).normalize_or_zero();
    let new_world = (Quat::from_rotation_arc(now, want) * rot).normalize();
    let parent_rot = match scene.get(&joint)?.parent.clone() {
        Some(par) => world_at(scene, &par, frame)?.to_scale_rotation_translation().1,
        None => Quat::IDENTITY,
    };
    let mut t = local_at(scene, &joint, frame)?;
    t.rotation = (parent_rot.inverse() * new_world).normalize().to_array();
    crate::placement::store_pose(scene, &joint, frame, p.keyframe_at, t)?;
    Ok(json!({ "joint": joint, "rotationEuler": t.euler_degrees() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct HairParams {
    /// The person (root, joint or part).
    id: String,
    style: HairStyle,
    /// Linear RGB 0..1. Default: the current hair colour (dark brown).
    color: Option<[f32; 3]>,
}

fn set_hair(scene: &mut Scene, p: HairParams) -> CmdResult<Value> {
    let root = person_root(scene, &p.id)?;
    let head = format!("{root}_head");
    let old: Vec<String> =
        scene.children(Some(&head)).filter(|e| e.name.starts_with("hair")).map(|e| e.id.clone()).collect();
    let keep_color = old.first().and_then(|id| match &scene.get(id).ok()?.kind {
        EntityKind::Mesh { color, .. } => Some(*color),
        _ => None,
    });
    let color = crate::builtin::color(p.color.or(keep_color).unwrap_or(HAIR))?;
    for id in &old {
        scene.remove(id)?;
    }
    // match the hair to the head (women are built at 94 %)
    let k = match &scene.get(&format!("{root}_skull"))?.kind {
        EntityKind::Mesh { .. } => scene.get(&format!("{root}_skull"))?.transform.scale[1] / 0.23,
        _ => 1.0,
    };
    let mut ids = vec![];
    for part in hair(p.style, k) {
        let id = format!("{root}_{}", part.name);
        let mut t = Transform { translation: part.pos, scale: part.scale, ..Default::default() };
        t.set_euler_degrees(part.rot);
        scene.insert(Entity {
            id: id.clone(),
            name: part.name.into(),
            parent: Some(head.clone()),
            transform: t,
            visible: true,
            locked: false,
            kind: EntityKind::Mesh {
                source: MeshSource::Primitive { primitive: part.prim.expect("hair is geometry") },
                modifiers: vec![],
                color,
            },
        })?;
        ids.push(id);
    }
    Ok(json!({ "id": root, "parts": ids }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PersonOnly {
    /// The person (root, joint or part).
    id: String,
    frame: Option<f32>,
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("pose.set", "Pose a person (prefab man / woman): a named pose and/or joint rotations in degrees; keeps the feet on the surface below.", mutates: true, PoseParams, pose_set),
        command!("pose.aim", "Turn one joint of a person so its limb points at a world point (e.g. the right elbow at a shelf).", mutates: true, AimParams, pose_aim),
        command!("pose.get", "A person's joints with their local rotations in degrees.", mutates: false, PersonOnly,
        |s, p| {
            let root = person_root(s, &p.id)?;
            let f = frame_ok(p.frame.unwrap_or(s.timeline.start as f32))?;
            let mut out = serde_json::Map::new();
            for j in JOINTS {
                out.insert(j.to_string(), json!(local_at(s, &format!("{root}_{j}"), f)?.euler_degrees()));
            }
            Ok(json!({ "id": root, "joints": out }))
        }),
        command!("pose.list", "Named poses for pose.set, the joint names, and the rotation conventions.", mutates: false, crate::builtin::NoParams,
        |_, _| Ok(json!({
            "poses": POSES.iter().map(|q| json!({ "name": q.name, "description": q.description })).collect::<Vec<_>>(),
            "joints": JOINTS,
            "hairStyles": HAIR_STYLES,
            "conventions": "Degrees [x, y, z] in the joint's parent space; the person faces +Z and +X is its left. Arms forward / thighs up: -x. Knees bend: +x. Elbows bend: -x. Spine lean / head nod forward: +x. Raise left arm sideways: +z; right arm: -z. Turn head left: +y.",
        }))),
        command!("person.setHair", "Change a person's hairstyle: short, long, bob, ponytail, bun or none (optionally its colour).", mutates: true, HairParams, set_hair),
    ]
}
