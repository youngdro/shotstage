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
    Pose {
        name: "relaxed",
        description: "Weight on one leg, hip shifted (contrapposto)",
        pelvis_drop: 0.01,
        joints: &[
            ("pelvis", [0.0, 0.0, 4.0]),
            ("spine", [0.0, 0.0, -5.0]),
            ("hip_l", [0.0, 0.0, -4.0]),
            ("hip_r", [-8.0, 0.0, 2.0]),
            ("knee_r", [16.0, 0.0, 0.0]),
            ("shoulder_l", [3.0, 0.0, 8.0]),
            ("shoulder_r", [-3.0, 0.0, -7.0]),
            ("elbow_l", [-10.0, 0.0, 0.0]),
            ("elbow_r", [-14.0, 0.0, 0.0]),
            ("head", [0.0, 0.0, 4.0]),
        ],
    },
    Pose {
        name: "handsOnHips",
        description: "Hands on the hips, elbows out (confident)",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [8.0, 0.0, 48.0]),
            ("shoulder_r", [8.0, 0.0, -48.0]),
            ("elbow_l", [0.0, 0.0, -100.0]),
            ("elbow_r", [0.0, 0.0, 100.0]),
        ],
    },
    Pose {
        name: "thinking",
        description: "Hand on the chin, the other arm across the waist",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_r", [-35.0, 0.0, -8.0]),
            ("elbow_r", [-140.0, 30.0, 0.0]),
            ("shoulder_l", [-15.0, 0.0, 8.0]),
            ("elbow_l", [-90.0, -75.0, 0.0]),
            ("head", [8.0, 0.0, -6.0]),
        ],
    },
    Pose {
        name: "taste",
        description: "Bringing food or a sample to the mouth",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_r", [-40.0, 0.0, -18.0]),
            ("elbow_r", [-135.0, 40.0, 0.0]),
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("elbow_l", [-10.0, 0.0, 0.0]),
            ("head", [6.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "drink",
        description: "Drinking from a cup or bottle",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_r", [-50.0, 0.0, -15.0]),
            ("elbow_r", [-125.0, 45.0, 0.0]),
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("neck", [-10.0, 0.0, 0.0]),
            ("head", [-12.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "checkPhone",
        description: "Looking down at a phone held in both hands",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-20.0, 0.0, 6.0]),
            ("shoulder_r", [-20.0, 0.0, -6.0]),
            ("elbow_l", [-85.0, -30.0, 0.0]),
            ("elbow_r", [-85.0, 30.0, 0.0]),
            ("neck", [22.0, 0.0, 0.0]),
            ("head", [15.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "selfie",
        description: "Taking a selfie with the right hand",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_r", [-115.0, 0.0, -30.0]),
            ("elbow_r", [-35.0, 0.0, 0.0]),
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("neck", [-6.0, -10.0, 0.0]),
            ("head", [-8.0, -12.0, 4.0]),
        ],
    },
    Pose {
        name: "browse",
        description: "Reaching for a product on a shelf at chest height",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [6.0, -10.0, 0.0]),
            ("shoulder_r", [-75.0, 0.0, -10.0]),
            ("elbow_r", [-25.0, 0.0, 0.0]),
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("head", [5.0, -10.0, 0.0]),
        ],
    },
    Pose {
        name: "lookUpShelf",
        description: "Standing back, looking up at a high shelf",
        pelvis_drop: 0.0,
        joints: &[
            ("neck", [-18.0, 10.0, 0.0]),
            ("head", [-15.0, 10.0, 0.0]),
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("shoulder_r", [-10.0, 0.0, -10.0]),
            ("elbow_r", [-80.0, 50.0, 0.0]),
        ],
    },
    Pose {
        name: "carryBox",
        description: "Carrying a box or crate in front with both arms",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [-6.0, 0.0, 0.0]),
            ("shoulder_l", [-40.0, 0.0, 14.0]),
            ("shoulder_r", [-40.0, 0.0, -14.0]),
            ("elbow_l", [-70.0, 0.0, 0.0]),
            ("elbow_r", [-70.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "carryBag",
        description: "Carrying a shopping bag in the right hand",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [0.0, 0.0, 4.0]),
            ("shoulder_r", [0.0, 0.0, -12.0]),
            ("elbow_r", [-4.0, 0.0, 0.0]),
            ("shoulder_l", [6.0, 0.0, 8.0]),
            ("elbow_l", [-15.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "cashier",
        description: "At a checkout, scanning an item with the right hand",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [8.0, -15.0, 0.0]),
            ("shoulder_r", [-45.0, 0.0, -25.0]),
            ("elbow_r", [-65.0, 20.0, 0.0]),
            ("shoulder_l", [-30.0, 0.0, 10.0]),
            ("elbow_l", [-60.0, 0.0, 0.0]),
            ("neck", [15.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "chop",
        description: "Working at a counter: both hands forward, looking down (chopping, packing)",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [10.0, 0.0, 0.0]),
            ("shoulder_l", [-25.0, 0.0, 10.0]),
            ("shoulder_r", [-25.0, 0.0, -10.0]),
            ("elbow_l", [-75.0, -20.0, 0.0]),
            ("elbow_r", [-75.0, 20.0, 0.0]),
            ("neck", [20.0, 0.0, 0.0]),
            ("head", [10.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "leanCounter",
        description: "Leaning forward on a counter on the forearms",
        pelvis_drop: 0.02,
        joints: &[
            ("spine", [30.0, 0.0, 0.0]),
            ("neck", [-20.0, 0.0, 0.0]),
            ("shoulder_l", [-55.0, 0.0, 12.0]),
            ("shoulder_r", [-55.0, 0.0, -12.0]),
            ("elbow_l", [-65.0, -15.0, 0.0]),
            ("elbow_r", [-65.0, 15.0, 0.0]),
            ("hip_l", [-6.0, 0.0, 0.0]),
            ("hip_r", [4.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "explain",
        description: "Talking with open hands, explaining",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-25.0, 0.0, 14.0]),
            ("shoulder_r", [-30.0, 0.0, -14.0]),
            ("elbow_l", [-70.0, 40.0, 0.0]),
            ("elbow_r", [-75.0, -40.0, 0.0]),
            ("head", [0.0, 8.0, 3.0]),
        ],
    },
    Pose {
        name: "shrug",
        description: "Shrugging, palms up",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-10.0, 0.0, 18.0]),
            ("shoulder_r", [-10.0, 0.0, -18.0]),
            ("elbow_l", [-85.0, 60.0, 0.0]),
            ("elbow_r", [-85.0, -60.0, 0.0]),
            ("head", [0.0, 0.0, 10.0]),
        ],
    },
    Pose {
        name: "clap",
        description: "Clapping hands in front of the chest",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-35.0, 0.0, 10.0]),
            ("shoulder_r", [-35.0, 0.0, -10.0]),
            ("elbow_l", [-85.0, -50.0, 0.0]),
            ("elbow_r", [-85.0, 50.0, 0.0]),
        ],
    },
    Pose {
        name: "cheer",
        description: "Both arms up, celebrating",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-10.0, 0.0, 150.0]),
            ("shoulder_r", [-10.0, 0.0, -150.0]),
            ("elbow_l", [0.0, 0.0, 20.0]),
            ("elbow_r", [0.0, 0.0, -20.0]),
            ("head", [-10.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "stretch",
        description: "Stretching both arms overhead",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [-5.0, 0.0, 0.0]),
            ("shoulder_l", [-175.0, 0.0, 8.0]),
            ("shoulder_r", [-175.0, 0.0, -8.0]),
            ("head", [-10.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "facepalm",
        description: "Hand over the face (oops, frustration)",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_r", [-45.0, 0.0, -10.0]),
            ("elbow_r", [-145.0, 30.0, 0.0]),
            ("shoulder_l", [0.0, 0.0, 6.0]),
            ("neck", [15.0, 0.0, 0.0]),
            ("head", [12.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "bow",
        description: "Bowing politely",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [35.0, 0.0, 0.0]),
            ("neck", [10.0, 0.0, 0.0]),
            ("shoulder_l", [-10.0, 0.0, 4.0]),
            ("shoulder_r", [-10.0, 0.0, -4.0]),
            ("hip_l", [-10.0, 0.0, 0.0]),
            ("hip_r", [-10.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "kneel",
        description: "Kneeling on the right knee (to a low shelf, to a child)",
        pelvis_drop: 0.42,
        joints: &[
            ("hip_l", [-85.0, 0.0, 4.0]),
            ("knee_l", [85.0, 0.0, 0.0]),
            ("hip_r", [5.0, 0.0, -4.0]),
            ("knee_r", [95.0, 0.0, 0.0]),
            ("ankle_r", [40.0, 0.0, 0.0]),
            ("shoulder_l", [-20.0, 0.0, 6.0]),
            ("elbow_l", [-40.0, 0.0, 0.0]),
            ("shoulder_r", [0.0, 0.0, -6.0]),
        ],
    },
    Pose {
        name: "sitFloor",
        description: "Sitting on the floor, legs out, leaning on the hands",
        pelvis_drop: 0.84,
        joints: &[
            ("spine", [-12.0, 0.0, 0.0]),
            ("neck", [12.0, 0.0, 0.0]),
            ("hip_l", [-88.0, 0.0, 5.0]),
            ("hip_r", [-88.0, 0.0, -5.0]),
            ("knee_l", [4.0, 0.0, 0.0]),
            ("knee_r", [12.0, 0.0, 0.0]),
            ("ankle_l", [-20.0, 0.0, 0.0]),
            ("ankle_r", [-20.0, 0.0, 0.0]),
            ("shoulder_l", [30.0, 0.0, 16.0]),
            ("shoulder_r", [30.0, 0.0, -16.0]),
        ],
    },
    Pose {
        name: "walkAway",
        description: "Mid-stride, right leg forward (the other half of the walk cycle)",
        pelvis_drop: 0.02,
        joints: &[
            ("hip_r", [-24.0, 0.0, 0.0]),
            ("knee_r", [8.0, 0.0, 0.0]),
            ("hip_l", [16.0, 0.0, 0.0]),
            ("knee_l", [28.0, 0.0, 0.0]),
            ("ankle_l", [-12.0, 0.0, 0.0]),
            ("shoulder_r", [18.0, 0.0, -6.0]),
            ("shoulder_l", [-20.0, 0.0, 6.0]),
            ("elbow_r", [-12.0, 0.0, 0.0]),
            ("elbow_l", [-25.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "handOver",
        description: "Handing something over with both hands at chest height",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [6.0, 0.0, 0.0]),
            ("shoulder_l", [-55.0, 0.0, 2.0]),
            ("shoulder_r", [-55.0, 0.0, -2.0]),
            ("elbow_l", [-35.0, -20.0, 0.0]),
            ("elbow_r", [-35.0, 20.0, 0.0]),
            ("head", [10.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "receive",
        description: "Reaching out to take something, palms up",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [4.0, 0.0, 0.0]),
            ("shoulder_l", [-50.0, 0.0, 6.0]),
            ("shoulder_r", [-50.0, 0.0, -6.0]),
            ("elbow_l", [-35.0, -10.0, 0.0]),
            ("elbow_r", [-35.0, 10.0, 0.0]),
            ("head", [10.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "welcome",
        description: "Greeting a customer: open hand out, a slight bow",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [8.0, 0.0, 0.0]),
            ("shoulder_r", [-40.0, 0.0, -20.0]),
            ("elbow_r", [-70.0, -10.0, 0.0]),
            ("shoulder_l", [5.0, 0.0, 4.0]),
            ("head", [6.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "recommend",
        description: "Showing a product to the side with an open hand",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-35.0, 0.0, 45.0]),
            ("elbow_l", [-25.0, 30.0, 0.0]),
            ("neck", [0.0, 10.0, 0.0]),
            ("head", [0.0, 18.0, 0.0]),
        ],
    },
    Pose {
        name: "scan",
        description: "Cashier scanning an item over the counter",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [10.0, 0.0, 0.0]),
            ("chest", [4.0, 0.0, 0.0]),
            ("shoulder_r", [-45.0, 0.0, -12.0]),
            ("elbow_r", [-50.0, 20.0, 0.0]),
            ("shoulder_l", [-35.0, 0.0, 12.0]),
            ("elbow_l", [-70.0, -30.0, 0.0]),
            ("head", [22.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "pay",
        description: "Paying: holding a phone or card out to the reader",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [3.0, 0.0, 0.0]),
            ("shoulder_r", [-55.0, 0.0, -5.0]),
            ("elbow_r", [-35.0, 20.0, 0.0]),
            ("head", [12.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "bagItems",
        description: "Packing items into a bag on the counter",
        pelvis_drop: 0.0,
        joints: &[
            ("spine", [22.0, 0.0, 0.0]),
            ("chest", [6.0, 0.0, 0.0]),
            ("shoulder_l", [-50.0, 0.0, 10.0]),
            ("shoulder_r", [-50.0, 0.0, -10.0]),
            ("elbow_l", [-30.0, -20.0, 0.0]),
            ("elbow_r", [-30.0, 20.0, 0.0]),
            ("head", [18.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "restock",
        description: "Placing a product on a shelf at shoulder height",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_r", [-85.0, 0.0, -8.0]),
            ("elbow_r", [-25.0, 10.0, 0.0]),
            ("shoulder_l", [-25.0, 0.0, 10.0]),
            ("elbow_l", [-60.0, -20.0, 0.0]),
            ("head", [-6.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "inspect",
        description: "Holding produce up in both hands to look at it",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-22.0, 0.0, 8.0]),
            ("shoulder_r", [-22.0, 0.0, -8.0]),
            ("elbow_l", [-80.0, -35.0, 0.0]),
            ("elbow_r", [-80.0, 35.0, 0.0]),
            ("head", [20.0, 0.0, 0.0]),
        ],
    },
    Pose {
        name: "waitInLine",
        description: "Waiting with hands clasped in front",
        pelvis_drop: 0.0,
        joints: &[
            ("shoulder_l", [-8.0, 0.0, 4.0]),
            ("shoulder_r", [-8.0, 0.0, -4.0]),
            ("elbow_l", [-45.0, -50.0, 0.0]),
            ("elbow_r", [-45.0, 50.0, 0.0]),
        ],
    },
];

/// Hand shape that goes with a named pose on realistic people (others: relaxed).
pub const POSE_HANDS: &[(&str, &str)] = &[
    ("tPose", "open"),
    ("run", "fist"),
    ("wave", "spread"),
    ("point", "point"),
    ("reach", "grip"),
    ("pickUp", "claw"),
    ("holdBasket", "grip"),
    ("pushCart", "grip"),
    ("phone", "grip"),
    ("present", "open"),
    ("relaxed", "relaxed"),
    ("handsOnHips", "open"),
    ("thinking", "relaxed"),
    ("taste", "pinch"),
    ("drink", "grip"),
    ("checkPhone", "grip"),
    ("selfie", "grip"),
    ("browse", "grip"),
    ("carryBox", "grip"),
    ("carryBag", "grip"),
    ("cashier", "grip"),
    ("chop", "grip"),
    ("explain", "open"),
    ("shrug", "open"),
    ("clap", "open"),
    ("cheer", "fist"),
    ("stretch", "spread"),
    ("facepalm", "open"),
    ("sitFloor", "open"),
    ("handOver", "grip"),
    ("receive", "open"),
    ("welcome", "open"),
    ("recommend", "open"),
    ("scan", "grip"),
    ("pay", "grip"),
    ("bagItems", "grip"),
    ("restock", "grip"),
    ("inspect", "grip"),
    ("waitInLine", "relaxed"),
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

/// The joints a skinned mesh follows (imported skeletons and realistic people).
pub fn mesh_joints(e: &Entity) -> Option<&Vec<String>> {
    match &e.kind {
        EntityKind::Mesh { source: MeshSource::Skinned { skin, .. }, .. } => Some(&skin.joints),
        EntityKind::Mesh { source: MeshSource::Human { joints, .. }, .. } => Some(joints),
        _ => None,
    }
}

/// Joints of imported skeletons and realistic people: every entity a skinned mesh follows.
pub fn skin_joints(scene: &Scene) -> std::collections::HashSet<String> {
    scene.entities.iter().filter_map(mesh_joints).flatten().cloned().collect()
}

/// A joint of a person prefab or of an imported skeleton.
pub fn is_joint(scene: &Scene, id: &str) -> bool {
    joint_name(scene, id).is_some() || skin_joints(scene).contains(id)
}

/// The joint nearest to a world point on a skinned mesh (measured to each bone, from a joint to
/// its first child joint).
pub fn joint_near(scene: &Scene, mesh: &str, point: Vec3, frame: f32) -> Option<String> {
    let joints = mesh_joints(scene.get(mesh).ok()?)?;
    let pos = |id: &str| world_at(scene, id, frame).ok().map(|m| m.w_axis.truncate());
    let mut best: Option<(f32, String)> = None;
    for j in joints {
        let Some(a) = pos(j) else { continue };
        let child = scene.children(Some(j)).find(|c| joints.contains(&c.id)).and_then(|c| pos(&c.id));
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
pub(crate) fn pelvis_rest(scene: &Scene, root: &str) -> CmdResult<f32> {
    if let Some(y) = crate::humans::pelvis_rest(scene, root) {
        return Ok(y);
    }
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
    /// Hand shape for realistic people (see `pose.list`), e.g. `fist`, `grip`, `point`.
    /// Default with a named pose: the pose's own hand shape.
    hands: Option<String>,
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
    let human = crate::humans::human_of(scene, &root).is_some();
    if human && p.pose.is_some() {
        // a named pose also straightens the joints only realistic people have
        for j in ["spine2", "chest", "clavicle_l", "clavicle_r", "toes_l", "toes_r"] {
            let id = format!("{root}_{j}");
            if scene.contains(&id) {
                let mut t = local_at(scene, &id, frame)?;
                t.rotation = Quat::IDENTITY.to_array();
                crate::placement::store_pose(scene, &id, frame, p.keyframe_at, t)?;
            }
        }
    }
    let hands = p.hands.clone().or_else(|| {
        p.pose
            .as_deref()
            .map(|n| POSE_HANDS.iter().find(|(q, _)| *q == n).map(|(_, h)| *h).unwrap_or("relaxed").to_string())
    });
    if let (true, Some(h)) = (human, hands) {
        crate::humans::set_hands(scene, &root, &h, crate::humans::HandSide::Both, frame, p.keyframe_at)?;
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
        .find(|e| e.name.starts_with("middle_01"))
        .or_else(|| kids.iter().find(|e| matches!(e.kind, EntityKind::Empty)))
        .or_else(|| kids.iter().find(|e| !e.name.starts_with("hair") && e.name != "nose"))
        .map(|e| e.transform.translation());
    // person limbs hang along -Y; imported (Blender) bones point along their +Y
    let human = person_root(scene, joint).is_ok_and(|r| crate::humans::human_of(scene, &r).is_some());
    let rest = if joint_name(scene, joint).is_some() || human { Vec3::NEG_Y } else { Vec3::Y };
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
    if crate::humans::human_of(scene, &root).is_some() {
        return crate::humans::set_human_hair(scene, &root, p.style, p.color);
    }
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
                material: None,
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
