//! Realistic people from MakeHuman / MPFB (CC0 data, baked by `scripts/mpfb/bake_human.py`).
//!
//! A human is a person root with joint entities (the MPFB "game engine" skeleton: spine, neck,
//! head, arms with clavicles and all finger joints, legs and toes) and a few mesh entities whose
//! source is [`MeshSource::Human`](crate::MeshSource::Human): skin, top, bottom, shoes, hair,
//! eyes and teeth. Each part is a slice of the same skinned body, generated from a handful of
//! parameters ([`HumanBody`], [`HumanOutfit`]), so a scene file and every undo step only store
//! those numbers; the vertices come from the built-in asset.
//!
//! The base mesh is modelled in an A-pose. Its joints are re-rested with the arms hanging
//! straight down, so a joint entity with no rotation means the same thing as on the simple
//! mannequin (`pose.set` presets, motions and hand-held props work on both).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::OnceLock;

use glam::{Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mesh::{Mesh, TriMesh};
use crate::{CoreError, Result};

static DATA: &[u8] = include_bytes!("../assets/human.bin");

/// Body shape and facial expression.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct HumanBody {
    /// 0 = female … 1 = male (in between blends).
    pub gender: f32,
    /// Years, 11 … 90 (25 = young adult).
    pub age: f32,
    /// -1 thin … 0 average … 1 heavy.
    pub weight: f32,
    /// -1 slight … 0 average … 1 muscular.
    pub muscle: f32,
    /// -1 short … 0 average … 1 tall.
    pub height: f32,
    /// Facial expression units (`human::EXPRESSION_UNITS`) with weights 0 … 1.
    pub face: BTreeMap<String, f32>,
}

impl Default for HumanBody {
    fn default() -> Self {
        Self { gender: 1.0, age: 25.0, weight: 0.0, muscle: 0.0, height: 0.0, face: BTreeMap::new() }
    }
}

impl HumanBody {
    pub fn validate(&self) -> Result<()> {
        let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if !ok(self.gender, 0.0, 1.0) {
            return Err(CoreError::Invalid("gender must be 0 (female) … 1 (male)".into()));
        }
        if !ok(self.age, 11.0, 90.0) {
            return Err(CoreError::Invalid("age must be 11 … 90 years".into()));
        }
        for (v, n) in [(self.weight, "weight"), (self.muscle, "muscle"), (self.height, "height")] {
            if !ok(v, -1.0, 1.0) {
                return Err(CoreError::Invalid(format!("{n} must be -1 … 1")));
            }
        }
        let a = asset();
        for (k, v) in &self.face {
            if !a.morphs.contains_key(&format!("expr:{k}")) {
                return Err(CoreError::Invalid(format!(
                    "no expression unit `{k}`; units are {}",
                    EXPRESSION_UNITS.join(", ")
                )));
            }
            if !ok(*v, -1.0, 1.5) {
                return Err(CoreError::Invalid(format!("expression unit `{k}` must be -1 … 1.5")));
            }
        }
        Ok(())
    }

    fn key(&self, h: &mut Hasher) {
        for v in [self.gender, self.age, self.weight, self.muscle, self.height] {
            h.word(v.to_bits());
        }
        for (k, v) in &self.face {
            h.bytes(k.as_bytes());
            h.word(v.to_bits());
        }
    }
}

/// Which slice of the body a mesh entity shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HumanPart {
    /// Bare skin: the body where clothes do not cover it.
    Skin,
    /// A fitted top (shirt, sweater) with `sleeves`.
    Top,
    /// Fitted trousers / shorts of length `legs`.
    Bottom,
    /// The feet, as shoes.
    Shoes,
    /// A hair volume around the skull, cut to `hair` length.
    Hair,
    /// The whites of the eyes.
    Eyes,
    /// Irises and pupils.
    Irises,
    Teeth,
}

pub const PARTS: &[HumanPart] = &[
    HumanPart::Skin,
    HumanPart::Top,
    HumanPart::Bottom,
    HumanPart::Shoes,
    HumanPart::Hair,
    HumanPart::Eyes,
    HumanPart::Irises,
    HumanPart::Teeth,
];

impl HumanPart {
    pub fn name(self) -> &'static str {
        match self {
            HumanPart::Skin => "skin",
            HumanPart::Top => "top",
            HumanPart::Bottom => "bottom",
            HumanPart::Shoes => "shoes",
            HumanPart::Hair => "hair",
            HumanPart::Eyes => "eyes",
            HumanPart::Irises => "irises",
            HumanPart::Teeth => "teeth",
        }
    }
}

/// Simple fitted clothes and hair length.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct HumanOutfit {
    /// Wear a top.
    pub top: bool,
    /// Sleeve length: 0 sleeveless, 0.35 short, 1 to the elbow, 2 long.
    pub sleeves: f32,
    /// Wear trousers / shorts.
    pub bottom: bool,
    /// Leg length: 0 briefs, 0.6 shorts, 1 to the knee, 2 full length.
    pub legs: f32,
    /// Wear shoes (otherwise bare feet).
    pub shoes: bool,
    /// Hair length: 0 none, 0.3 cropped, 0.5 short, 0.75 chin, 1 shoulder, 1.4 long.
    pub hair: f32,
}

impl Default for HumanOutfit {
    fn default() -> Self {
        Self { top: true, sleeves: 0.35, bottom: true, legs: 2.0, shoes: true, hair: 0.5 }
    }
}

impl HumanOutfit {
    pub fn validate(&self) -> Result<()> {
        let ok = |v: f32, hi: f32| v.is_finite() && (0.0..=hi).contains(&v);
        if !ok(self.sleeves, 2.0) || !ok(self.legs, 2.0) || !ok(self.hair, 2.0) {
            return Err(CoreError::Invalid("sleeves and legs must be 0 … 2, hair 0 … 2".into()));
        }
        Ok(())
    }

    fn key(&self, h: &mut Hasher) {
        h.word(self.top as u32 | (self.bottom as u32) << 1 | (self.shoes as u32) << 2);
        for v in [self.sleeves, self.legs, self.hair] {
            h.word(v.to_bits());
        }
    }
}

/// Every joint of a human, parents first (the MPFB game-engine rig). The mannequin names are
/// used where the two rigs share a joint, so poses and motions carry over.
pub fn joint_names() -> Vec<&'static str> {
    asset().bones.iter().map(|b| b.name).collect()
}

/// The MPFB bone a joint entity stands for, and back.
const RENAMES: &[(&str, &str)] = &[
    ("spine_01", "spine"),
    ("spine_02", "spine2"),
    ("spine_03", "chest"),
    ("neck_01", "neck"),
    ("upperarm_l", "shoulder_l"),
    ("lowerarm_l", "elbow_l"),
    ("hand_l", "wrist_l"),
    ("upperarm_r", "shoulder_r"),
    ("lowerarm_r", "elbow_r"),
    ("hand_r", "wrist_r"),
    ("thigh_l", "hip_l"),
    ("calf_l", "knee_l"),
    ("foot_l", "ankle_l"),
    ("thigh_r", "hip_r"),
    ("calf_r", "knee_r"),
    ("foot_r", "ankle_r"),
    ("ball_l", "toes_l"),
    ("ball_r", "toes_r"),
];

fn rename(mpfb: &str) -> &'static str {
    RENAMES.iter().find(|(m, _)| *m == mpfb).map(|(_, n)| *n).unwrap_or_else(|| leak(mpfb))
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// Facial expression units (morph targets), usable in [`HumanBody::face`].
pub const EXPRESSION_UNITS: &[&str] = &[
    "eye-left-closure",
    "eye-left-opened-up",
    "eye-left-slit",
    "eye-right-closure",
    "eye-right-opened-up",
    "eye-right-slit",
    "eyebrows-left-down",
    "eyebrows-left-extern-up",
    "eyebrows-left-inner-up",
    "eyebrows-left-up",
    "eyebrows-right-down",
    "eyebrows-right-extern-up",
    "eyebrows-right-inner-up",
    "eyebrows-right-up",
    "mouth-compression",
    "mouth-corner-puller",
    "mouth-depression-retraction",
    "mouth-depression",
    "mouth-elevation",
    "mouth-eversion",
    "mouth-open",
    "mouth-parling",
    "mouth-part-later",
    "mouth-protusion",
    "mouth-pursing",
    "mouth-retraction",
    "mouth-upward-retraction",
    "neck-platysma",
    "nose-compression",
    "nose-depression",
    "nose-left-dilatation",
    "nose-left-elevation",
    "nose-right-dilatation",
    "nose-right-elevation",
];

/// A named facial expression: units and weights.
pub struct Expression {
    pub name: &'static str,
    pub description: &'static str,
    pub units: &'static [(&'static str, f32)],
}

/// Index of a named expression in [`EXPRESSIONS`].
pub fn expression_index(name: &str) -> Option<u8> {
    EXPRESSIONS.iter().position(|e| e.name == name).map(|i| i as u8)
}

/// Expression-unit weights of a keyed mix of named expressions.
pub fn mix_face(mix: &crate::ExpressionMix) -> BTreeMap<String, f32> {
    let mut face: BTreeMap<String, f32> = BTreeMap::new();
    for (i, w) in [(mix.a, mix.a_amount), (mix.b, mix.b_amount)] {
        let Some(e) = EXPRESSIONS.get(i as usize) else { continue };
        for (u, x) in e.units {
            *face.entry(u.to_string()).or_default() += x * w;
        }
    }
    // a few steps per unit keep cross-fades cheap to cache without visible stepping
    face.retain(|_, w| {
        *w = (*w * 200.0).round() / 200.0;
        w.abs() > 1e-4
    });
    face
}

pub const EXPRESSIONS: &[Expression] = &[
    Expression { name: "neutral", description: "Relaxed, no expression", units: &[] },
    Expression {
        name: "smile",
        description: "Friendly closed-mouth smile",
        units: &[
            ("mouth-corner-puller", 0.8),
            ("mouth-upward-retraction", 0.25),
            ("eye-left-slit", 0.25),
            ("eye-right-slit", 0.25),
        ],
    },
    Expression {
        name: "grin",
        description: "Big smile showing the teeth",
        units: &[
            ("mouth-corner-puller", 1.0),
            ("mouth-upward-retraction", 0.6),
            ("mouth-part-later", 0.5),
            ("mouth-open", 0.12),
            ("eye-left-slit", 0.45),
            ("eye-right-slit", 0.45),
            ("eyebrows-left-up", 0.2),
            ("eyebrows-right-up", 0.2),
        ],
    },
    Expression {
        name: "laugh",
        description: "Laughing, mouth open, eyes narrowed",
        units: &[
            ("mouth-corner-puller", 1.0),
            ("mouth-upward-retraction", 0.5),
            ("mouth-open", 0.55),
            ("eye-left-slit", 0.7),
            ("eye-right-slit", 0.7),
            ("eyebrows-left-up", 0.3),
            ("eyebrows-right-up", 0.3),
        ],
    },
    Expression {
        name: "surprised",
        description: "Brows up, eyes wide, mouth open",
        units: &[
            ("eyebrows-left-up", 1.0),
            ("eyebrows-right-up", 1.0),
            ("eye-left-opened-up", 1.0),
            ("eye-right-opened-up", 1.0),
            ("mouth-open", 0.5),
        ],
    },
    Expression {
        name: "delighted",
        description: "Pleasantly surprised (tasting, unboxing)",
        units: &[
            ("eyebrows-left-up", 0.7),
            ("eyebrows-right-up", 0.7),
            ("mouth-corner-puller", 0.8),
            ("mouth-open", 0.25),
            ("eye-left-opened-up", 0.4),
            ("eye-right-opened-up", 0.4),
        ],
    },
    Expression {
        name: "sad",
        description: "Inner brows raised, mouth corners down",
        units: &[
            ("eyebrows-left-inner-up", 1.0),
            ("eyebrows-right-inner-up", 1.0),
            ("mouth-depression", 0.8),
            ("eye-left-closure", 0.2),
            ("eye-right-closure", 0.2),
        ],
    },
    Expression {
        name: "angry",
        description: "Brows down, lips pressed",
        units: &[
            ("eyebrows-left-down", 1.0),
            ("eyebrows-right-down", 1.0),
            ("mouth-compression", 0.7),
            ("nose-left-elevation", 0.4),
            ("nose-right-elevation", 0.4),
            ("eye-left-slit", 0.3),
            ("eye-right-slit", 0.3),
        ],
    },
    Expression {
        name: "disgusted",
        description: "Nose wrinkled, upper lip raised",
        units: &[
            ("nose-left-elevation", 1.0),
            ("nose-right-elevation", 1.0),
            ("mouth-elevation", 0.6),
            ("mouth-depression", 0.4),
            ("eyebrows-left-down", 0.6),
            ("eyebrows-right-down", 0.6),
        ],
    },
    Expression {
        name: "fear",
        description: "Brows up and together, eyes wide, mouth stretched",
        units: &[
            ("eyebrows-left-inner-up", 1.0),
            ("eyebrows-right-inner-up", 1.0),
            ("eyebrows-left-up", 0.5),
            ("eyebrows-right-up", 0.5),
            ("eye-left-opened-up", 1.0),
            ("eye-right-opened-up", 1.0),
            ("mouth-retraction", 0.8),
            ("mouth-open", 0.2),
            ("neck-platysma", 0.6),
        ],
    },
    Expression {
        name: "thinking",
        description: "Pondering: one brow up, lips pursed to the side",
        units: &[
            ("eyebrows-left-up", 0.7),
            ("eyebrows-right-down", 0.4),
            ("mouth-pursing", 0.4),
            ("mouth-compression", 0.3),
            ("eye-right-slit", 0.3),
        ],
    },
    Expression {
        name: "talking",
        description: "Mid-sentence, mouth half open",
        units: &[("mouth-open", 0.3), ("mouth-part-later", 0.3), ("eyebrows-left-up", 0.2), ("eyebrows-right-up", 0.2)],
    },
    Expression {
        name: "shout",
        description: "Shouting or cheering, mouth wide open",
        units: &[
            ("mouth-open", 1.0),
            ("mouth-retraction", 0.5),
            ("eyebrows-left-down", 0.5),
            ("eyebrows-right-down", 0.5),
            ("eye-left-slit", 0.4),
            ("eye-right-slit", 0.4),
        ],
    },
    Expression {
        name: "kiss",
        description: "Puckered lips (also blowing, whistling)",
        units: &[("mouth-protusion", 1.0), ("mouth-pursing", 1.0)],
    },
    Expression {
        name: "eyesClosed",
        description: "Eyes gently closed (sleeping, savouring)",
        units: &[("eye-left-closure", 1.0), ("eye-right-closure", 1.0)],
    },
    Expression {
        name: "wink",
        description: "Winking with the right eye, half smile",
        units: &[("eye-right-closure", 1.0), ("mouth-corner-puller", 0.6), ("eyebrows-right-down", 0.3)],
    },
    Expression {
        name: "skeptical",
        description: "One eyebrow raised, unconvinced",
        units: &[
            ("eyebrows-left-up", 1.0),
            ("eyebrows-right-down", 0.5),
            ("mouth-compression", 0.4),
            ("mouth-depression", 0.2),
        ],
    },
    Expression {
        name: "focused",
        description: "Concentrating: brows slightly down, eyes narrowed",
        units: &[
            ("eyebrows-left-down", 0.4),
            ("eyebrows-right-down", 0.4),
            ("eye-left-slit", 0.4),
            ("eye-right-slit", 0.4),
            ("mouth-compression", 0.3),
        ],
    },
];

// ---------------------------------------------------------------- the asset

struct Bone {
    name: &'static str,
    mpfb: &'static str,
    parent: Option<usize>,
    head: Vec<(u32, f32)>,
}

enum MorphData {
    Dense(Vec<[i16; 3]>),
    Sparse(Vec<(u16, [i16; 3])>),
}

struct Morph {
    unit: f32,
    data: MorphData,
}

struct Asset {
    verts: usize,
    quads: Vec<[u32; 4]>,
    part: Vec<u8>,
    region: Vec<u8>,
    /// Per vertex: where clothing edges fall.
    vfield: Vec<VField>,
    base: [Vec<Vec3>; 2],
    bones: Vec<Bone>,
    skin_joints: Vec<[u8; 4]>,
    skin_weights: Vec<[f32; 4]>,
    morphs: HashMap<String, Morph>,
}

struct Reader<'a> {
    b: &'a [u8],
    o: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let s = &self.b[self.o..self.o + n];
        self.o += n;
        s
    }
    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take(2).try_into().unwrap())
    }
    fn i16(&mut self) -> i16 {
        i16::from_le_bytes(self.take(2).try_into().unwrap())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn str(&mut self) -> &'static str {
        let n = self.u8() as usize;
        leak(std::str::from_utf8(self.take(n)).expect("utf-8 name"))
    }
}

fn asset() -> &'static Asset {
    static A: OnceLock<Asset> = OnceLock::new();
    A.get_or_init(|| {
        let mut r = Reader { b: DATA, o: 0 };
        assert_eq!(r.take(4), b"SSHM", "human asset magic");
        assert_eq!(r.u32(), 3, "human asset version");
        let (verts, quads, bones, morphs) = (r.u32() as usize, r.u32() as usize, r.u32() as usize, r.u32() as usize);
        let quad_list = (0..quads).map(|_| [0; 4].map(|_: u32| r.u16() as u32)).collect();
        let part = r.take(quads).to_vec();
        let region = r.take(quads).to_vec();
        r.take(quads); // per-face position along the limb (unused at runtime)
        let vfield = (0..verts)
            .map(|_| VField {
                region: r.u8(),
                limb: r.i16() as f32 / 10000.0,
                neck: r.i16() as f32 / 1000.0,
                waist: r.i16() as f32 / 1000.0,
                armhole: r.i16() as f32 / 10000.0,
            })
            .collect();
        let mut base = [Vec::with_capacity(verts), Vec::with_capacity(verts)];
        for b in &mut base {
            for _ in 0..verts {
                let p = [0; 3].map(|_: i32| r.i16() as f32 / 16000.0);
                b.push(Vec3::from_array(p));
            }
        }
        let mut bone_list = Vec::with_capacity(bones);
        for _ in 0..bones {
            let mpfb = r.str();
            let parent = r.i16();
            let n = r.u16() as usize;
            let head = (0..n).map(|_| (r.u16() as u32, r.f32())).collect();
            bone_list.push(Bone { name: rename(mpfb), mpfb, parent: (parent >= 0).then_some(parent as usize), head });
        }
        let mut skin_joints = Vec::with_capacity(verts);
        let mut skin_weights = Vec::with_capacity(verts);
        for _ in 0..verts {
            skin_joints.push([0; 4].map(|_: u8| r.u8()));
            skin_weights.push([0; 4].map(|_: u8| r.u8() as f32 / 255.0));
        }
        let mut morph_map = HashMap::new();
        for _ in 0..morphs {
            let name = r.str();
            let unit = r.f32();
            let dense = r.u8() != 0;
            let n = r.u32() as usize;
            let data = if dense {
                MorphData::Dense((0..n).map(|_| [0; 3].map(|_: i32| r.i16())).collect())
            } else {
                MorphData::Sparse((0..n).map(|_| (r.u16(), [0; 3].map(|_: i32| r.i16()))).collect())
            };
            morph_map.insert(name.to_string(), Morph { unit, data });
        }
        assert_eq!(r.o, DATA.len(), "human asset length");
        Asset {
            verts,
            quads: quad_list,
            part,
            region,
            vfield,
            base,
            bones: bone_list,
            skin_joints,
            skin_weights,
            morphs: morph_map,
        }
    })
}

impl Morph {
    fn add_to(&self, out: &mut [Vec3], w: f32) {
        if w.abs() < 1e-6 {
            return;
        }
        let k = self.unit * w;
        let d = |q: &[i16; 3]| Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32) * k;
        match &self.data {
            MorphData::Dense(v) => out.iter_mut().zip(v).for_each(|(p, q)| *p += d(q)),
            MorphData::Sparse(v) => v.iter().for_each(|(i, q)| out[*i as usize] += d(q)),
        }
    }
}

// ---------------------------------------------------------------- shape and skeleton

/// The body for a set of parameters, before posing.
pub struct HumanRest {
    /// Vertices in the person's space (feet on y = 0, facing +Z), in the modelled A-pose.
    positions: Vec<Vec3>,
    /// Joint positions with the arms hanging down: what the joint entities are placed at.
    pub joints: Vec<Vec3>,
    /// Maps an A-pose vertex into the joint's space with the arms re-rested.
    inverse_bind: Vec<Mat4>,
}

/// Bone names (as joint entity suffixes) and parent indices.
pub fn skeleton() -> Vec<(&'static str, Option<usize>)> {
    asset().bones.iter().map(|b| (b.name, b.parent)).collect()
}

/// The MPFB name of a joint (`shoulder_l` → `upperarm_l`).
pub fn mpfb_name(joint: &str) -> Option<&'static str> {
    asset().bones.iter().find(|b| b.name == joint).map(|b| b.mpfb)
}

/// Hasher for cache keys (FNV-1a).
struct Hasher(u64);

impl Hasher {
    fn new() -> Self {
        Hasher(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for x in b {
            self.0 ^= *x as u64;
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
    fn word(&mut self, w: u32) {
        self.0 ^= w as u64;
        self.0 = self.0.wrapping_mul(0x100_0000_01b3);
    }
}

thread_local! {
    static RESTS: RefCell<HashMap<u64, Rc<HumanRest>>> = RefCell::new(HashMap::new());
    static SLICES: RefCell<HashMap<u64, Rc<Slice>>> = RefCell::new(HashMap::new());
    static POSED: RefCell<HashMap<u64, Rc<TriMesh>>> = RefCell::new(HashMap::new());
    static GEOMETRY: RefCell<HashMap<u64, Rc<Geometry>>> = RefCell::new(HashMap::new());
}

fn cached<T>(
    cache: &'static std::thread::LocalKey<RefCell<HashMap<u64, Rc<T>>>>,
    key: u64,
    limit: usize,
    make: impl FnOnce() -> T,
) -> Rc<T> {
    if let Some(v) = cache.with(|c| c.borrow().get(&key).cloned()) {
        return v;
    }
    let v = Rc::new(make());
    cache.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= limit {
            c.clear();
        }
        c.insert(key, v.clone());
    });
    v
}

/// The body for `body`: shape, joint positions and bind matrices (cached).
pub fn rest(body: &HumanBody) -> Rc<HumanRest> {
    let mut h = Hasher::new();
    body.key(&mut h);
    cached(&RESTS, h.0, 64, || build_rest(body))
}

fn build_rest(body: &HumanBody) -> HumanRest {
    let a = asset();
    let g = body.gender.clamp(0.0, 1.0);
    let mut pos: Vec<Vec3> = a.base[0].iter().zip(&a.base[1]).map(|(f, m)| f.lerp(*m, g)).collect();
    let signed = |v: f32| if v >= 0.0 { (v, 0.0) } else { (0.0, -v) };
    let (wp, wm) = signed(body.weight);
    let (mp, mm) = signed(body.muscle);
    let (hp, hm) = signed(body.height);
    let child = ((25.0 - body.age) / 14.0).clamp(0.0, 1.0);
    let old = ((body.age - 25.0) / 65.0).clamp(0.0, 1.0);
    for (sex, sw) in [("female", 1.0 - g), ("male", g)] {
        for (m, w) in [
            ("weight+", wp),
            ("weight-", wm),
            ("muscle+", mp),
            ("muscle-", mm),
            ("height+", hp),
            ("height-", hm),
            ("child", child),
            ("old", old),
        ] {
            if let Some(mo) = a.morphs.get(&format!("{sex}:{m}")) {
                mo.add_to(&mut pos, w * sw);
            }
        }
    }
    for (k, w) in &body.face {
        if let Some(mo) = a.morphs.get(&format!("expr:{k}")) {
            mo.add_to(&mut pos, *w);
        }
    }
    // feet on the ground
    let floor = pos
        .iter()
        .zip(0..)
        .filter(|(_, i)| a.part_of_vertex(*i) == Some(0))
        .map(|(p, _)| p.y)
        .fold(f32::INFINITY, f32::min);
    let floor = if floor.is_finite() { floor } else { 0.0 };
    for p in &mut pos {
        p.y -= floor;
    }
    let heads: Vec<Vec3> =
        a.bones.iter().map(|b| b.head.iter().map(|(v, w)| pos[*v as usize] * *w).sum::<Vec3>()).collect();
    // re-rest the arms hanging straight down
    let idx = |n: &str| a.bones.iter().position(|b| b.mpfb == n).expect("bone");
    let mut own = vec![Quat::IDENTITY; a.bones.len()];
    for s in ["l", "r"] {
        let (up, lo, hand) = (idx(&format!("upperarm_{s}")), idx(&format!("lowerarm_{s}")), idx(&format!("hand_{s}")));
        let c_up = Quat::from_rotation_arc((heads[lo] - heads[up]).normalize(), Vec3::NEG_Y);
        let fore = c_up * (heads[hand] - heads[lo]).normalize();
        own[up] = c_up;
        own[lo] = Quat::from_rotation_arc(fore, Vec3::NEG_Y) * c_up;
    }
    let mut corr = vec![Quat::IDENTITY; a.bones.len()];
    let mut joints = vec![Vec3::ZERO; a.bones.len()];
    for (i, b) in a.bones.iter().enumerate() {
        match b.parent {
            Some(p) => {
                joints[i] = joints[p] + corr[p] * (heads[i] - heads[p]);
                corr[i] = if own[i] != Quat::IDENTITY { own[i] } else { corr[p] };
            }
            None => {
                joints[i] = heads[i];
                corr[i] = own[i];
            }
        }
    }
    let inverse_bind =
        (0..a.bones.len()).map(|i| Mat4::from_quat(corr[i]) * Mat4::from_translation(-heads[i])).collect();
    HumanRest { positions: pos, joints, inverse_bind }
}

impl Asset {
    /// Part of the first face using a vertex (vertices are not shared between parts).
    fn part_of_vertex(&self, v: usize) -> Option<u8> {
        static MAP: OnceLock<Vec<u8>> = OnceLock::new();
        let map = MAP.get_or_init(|| {
            let a = asset();
            let mut m = vec![u8::MAX; a.verts];
            for (q, p) in a.quads.iter().zip(&a.part) {
                for i in q {
                    m[*i as usize] = *p;
                }
            }
            m
        });
        map.get(v).copied().filter(|p| *p != u8::MAX)
    }
}

impl HumanRest {
    /// Local translations for the joint entities (relative to their parent joint; the root
    /// joint relative to the person).
    pub fn joint_locals(&self) -> Vec<Vec3> {
        asset()
            .bones
            .iter()
            .enumerate()
            .map(|(i, b)| match b.parent {
                Some(p) => self.joints[i] - self.joints[p],
                None => self.joints[i],
            })
            .collect()
    }

    /// Standing height (top of the head), metres.
    pub fn height(&self) -> f32 {
        let a = asset();
        self.positions
            .iter()
            .enumerate()
            .filter(|(i, _)| a.part_of_vertex(*i) == Some(0))
            .map(|(_, p)| p.y)
            .fold(0.0, f32::max)
    }
}

// ---------------------------------------------------------------- parts

/// A vertex of a part: an asset vertex, or a point on the edge between two (where a clothing
/// edge cuts through a face).
#[derive(Clone, Copy)]
enum SliceVert {
    Asset(u32),
    Cut(u32, u32, f32),
}

/// The faces and vertices of one part (independent of the body shape).
struct Slice {
    verts: Vec<SliceVert>,
    /// Polygons (3 … 6 corners) in slice-local indices.
    faces: Vec<Vec<u32>>,
}

const REGION_HEAD: u8 = 0;
const REGION_TORSO: u8 = 1;
const REGION_HIPS: u8 = 2;
const REGION_ARM: u8 = 3;
const REGION_LEG: u8 = 5;
const REGION_FOOT: u8 = 6;

/// Per-vertex measures that clothing edges are cut along (smooth, so cuts come out clean).
#[derive(Clone, Copy)]
struct VField {
    region: u8,
    /// Position along the limb: 0 shoulder / hip, 1 elbow / knee, 2 wrist / ankle (negative
    /// just before the shoulder).
    limb: f32,
    /// Metres below the neckline.
    neck: f32,
    /// Metres above the waist.
    waist: f32,
    /// Inside a sleeveless armhole (> 0).
    armhole: f32,
}

/// How far inside the top a vertex is (> 0 = covered), in units of about 25 cm.
fn top_field(o: &HumanOutfit, v: usize) -> f32 {
    if !o.top {
        return -1.0;
    }
    let f = asset().vfield[v];
    match f.region {
        REGION_HEAD | REGION_TORSO | REGION_HIPS => {
            let trunk = f.neck.min(f.waist) / 0.25;
            if o.sleeves > 0.0 { trunk } else { trunk.min(f.armhole) }
        }
        REGION_ARM if o.sleeves > 0.0 => o.sleeves - f.limb,
        REGION_ARM => f.armhole,
        _ => -1.0,
    }
}

/// How far inside the trousers a vertex is (> 0 = covered and not under the top).
fn bottom_field(o: &HumanOutfit, v: usize) -> f32 {
    if !o.bottom {
        return -1.0;
    }
    let f = asset().vfield[v];
    let b = match f.region {
        REGION_HEAD | REGION_TORSO | REGION_HIPS => -f.waist / 0.25,
        REGION_LEG => o.legs.max(0.15) - f.limb,
        _ => -1.0,
    };
    if o.top { b.min(-top_field(o, v)) } else { b }
}

/// How far inside a shoe a vertex is.
fn shoe_field(o: &HumanOutfit, v: usize) -> f32 {
    if !o.shoes {
        return -1.0;
    }
    let f = asset().vfield[v];
    match f.region {
        REGION_FOOT => 1.0,
        REGION_LEG => (f.limb - 1.93) * 4.0,
        _ => -1.0,
    }
}

/// Centre of the eyes on the (male) base mesh, which the scalp fields are measured from.
fn base_eye() -> Vec3 {
    static EYE: OnceLock<Vec3> = OnceLock::new();
    *EYE.get_or_init(|| eye_center(&asset().base[1]))
}

/// How far inside the hairline a vertex is (> 0 = under the hair), in metres.
fn hair_field(o: &HumanOutfit, v: usize) -> f32 {
    let a = asset();
    if o.hair <= 0.0 || a.vfield[v].region != REGION_HEAD {
        return -1.0;
    }
    let eye = base_eye();
    let p = a.base[1][v];
    // high on the forehead, down to the nape at the back
    let nape = eye.y - 0.035 - 0.05 * o.hair.min(1.0);
    let t = ((eye.z - 0.03 - p.z) / 0.13).clamp(0.0, 1.0);
    let hairline = (eye.y + 0.062) * (1.0 - t) + nape * t;
    (p.y - hairline).min(p.y - (eye.y - 0.12))
}

fn slice(part: HumanPart, outfit: &HumanOutfit) -> Rc<Slice> {
    let mut h = Hasher::new();
    h.word(part as u32);
    outfit.key(&mut h);
    cached(&SLICES, h.0, 256, || {
        let a = asset();
        // an eye face is iris when it is near the front of its eyeball
        let iris = |q: usize| -> bool {
            let c = a.quads[q].iter().map(|i| a.base[1][*i as usize]).sum::<Vec3>() / 4.0;
            let side: Vec<Vec3> = (0..a.quads.len())
                .filter(|o| a.part[*o] == 3)
                .flat_map(|o| a.quads[o].iter().map(|i| a.base[1][*i as usize]))
                .filter(|p| p.x.signum() == c.x.signum())
                .collect();
            let centre = side.iter().sum::<Vec3>() / side.len().max(1) as f32;
            let radius = side.iter().map(|p| (p.z - centre.z).abs()).fold(0.0, f32::max);
            c.z > centre.z + radius * 0.86
        };
        // skin stays under the edges of clothes (they sit a few mm off it): hidden only well inside
        let covered = |v: u32| {
            let v = v as usize;
            top_field(outfit, v).max(bottom_field(outfit, v)).max(shoe_field(outfit, v)) > 0.12
        };
        // (source part, field to cut by)
        let (source, field): (u8, Option<Field>) = match part {
            HumanPart::Skin => (0, None),
            HumanPart::Top => (1, Some(top_field)),
            HumanPart::Bottom => (1, Some(bottom_field)),
            HumanPart::Shoes => (0, Some(shoe_field)),
            HumanPart::Hair => (0, Some(hair_field)),
            HumanPart::Eyes | HumanPart::Irises => (3, None),
            HumanPart::Teeth => (4, None),
        };
        let mut out = Slice { verts: vec![], faces: vec![] };
        let mut asset_map: HashMap<u32, u32> = HashMap::new();
        let mut cut_map: HashMap<(u32, u32), u32> = HashMap::new();
        for q in (0..a.quads.len()).filter(|q| a.part[*q] == source) {
            let quad = a.quads[q];
            let keep_whole = match part {
                HumanPart::Skin => !quad.iter().all(|v| covered(*v)),
                HumanPart::Eyes => !iris(q),
                HumanPart::Irises => iris(q),
                _ => true,
            };
            if !keep_whole {
                continue;
            }
            let mut add_asset = |out: &mut Slice, v: u32| {
                *asset_map.entry(v).or_insert_with(|| {
                    out.verts.push(SliceVert::Asset(v));
                    out.verts.len() as u32 - 1
                })
            };
            let Some(f) = field else {
                let face = quad.iter().map(|v| add_asset(&mut out, *v)).collect();
                out.faces.push(face);
                continue;
            };
            let vals = quad.map(|v| f(outfit, v as usize));
            if vals.iter().all(|x| *x < 0.0) {
                continue;
            }
            let mut poly = vec![];
            for k in 0..4 {
                let (va, vb, fa, fb) = (quad[k], quad[(k + 1) % 4], vals[k], vals[(k + 1) % 4]);
                if fa >= 0.0 {
                    poly.push(add_asset(&mut out, va));
                }
                if (fa >= 0.0) != (fb >= 0.0) {
                    // the same edge point for both faces sharing the edge
                    let (lo, hi, flo, fhi) = if va < vb { (va, vb, fa, fb) } else { (vb, va, fb, fa) };
                    let t = (flo / (flo - fhi)).clamp(0.0, 1.0);
                    let i = *cut_map.entry((lo, hi)).or_insert_with(|| {
                        out.verts.push(SliceVert::Cut(lo, hi, t));
                        out.verts.len() as u32 - 1
                    });
                    poly.push(i);
                }
            }
            if poly.len() >= 3 {
                out.faces.push(poly);
            }
        }
        out
    })
}

/// Skinning matrices for `rest` with joints at world matrices `joint_world` (None = in rest),
/// expressed in the mesh entity's space (`to_local`).
pub fn skin_matrices(rest: &HumanRest, to_local: Mat4, joint_world: &[Option<Mat4>]) -> Vec<Mat4> {
    rest.inverse_bind
        .iter()
        .enumerate()
        .map(|(i, ib)| {
            let w = joint_world.get(i).copied().flatten().unwrap_or_else(|| Mat4::from_translation(rest.joints[i]));
            to_local * w * *ib
        })
        .collect()
}

/// Up to four joints and their weights.
/// How far a vertex is covered by a garment (> 0 covered), from the outfit.
type Field = fn(&HumanOutfit, usize) -> f32;

type Influence = ([u8; 4], [f32; 4]);

fn influence(v: u32) -> Influence {
    let a = asset();
    (a.skin_joints[v as usize], a.skin_weights[v as usize])
}

/// Weights of a point between two vertices.
fn blend(x: Influence, y: Influence, t: f32) -> Influence {
    let mut acc: Vec<(u8, f32)> = vec![];
    for (inf, w) in [(x, 1.0 - t), (y, t)] {
        for k in 0..4 {
            if inf.1[k] > 0.0 {
                match acc.iter_mut().find(|(j, _)| *j == inf.0[k]) {
                    Some(e) => e.1 += inf.1[k] * w,
                    None => acc.push((inf.0[k], inf.1[k] * w)),
                }
            }
        }
    }
    acc.sort_by(|p, q| q.1.total_cmp(&p.1));
    acc.truncate(4);
    let total: f32 = acc.iter().map(|e| e.1).sum::<f32>().max(1e-6);
    let mut out = ([0u8; 4], [0f32; 4]);
    for (k, (j, w)) in acc.into_iter().enumerate() {
        out.0[k] = j;
        out.1[k] = w / total;
    }
    out
}

/// A part ready to skin: rest (A-pose) positions, skin weights and polygons.
struct Geometry {
    rest: Vec<Vec3>,
    skin: Vec<Influence>,
    faces: Vec<Vec<u32>>,
}

impl Geometry {
    fn push(&mut self, p: Vec3, v: u32) -> u32 {
        self.rest.push(p);
        self.skin.push(influence(v));
        self.rest.len() as u32 - 1
    }
}

/// Vertex normals of the rest body (area-weighted over the skin faces).
fn rest_normals(rest: &HumanRest) -> Vec<Vec3> {
    let a = asset();
    let p = &rest.positions;
    let mut n = vec![Vec3::ZERO; p.len()];
    for (q, part) in a.quads.iter().zip(&a.part) {
        if *part != 0 && *part != 1 {
            continue;
        }
        let [i, j, k, l] = q.map(|v| v as usize);
        let f = (p[k] - p[i]).cross(p[l] - p[j]);
        for v in [i, j, k, l] {
            n[v] += f;
        }
    }
    n.into_iter().map(|v| v.normalize_or(Vec3::Y)).collect()
}

fn geometry(body: &HumanBody, part: HumanPart, outfit: &HumanOutfit) -> Rc<Geometry> {
    let mut h = Hasher::new();
    body.key(&mut h);
    outfit.key(&mut h);
    h.word(part as u32 + 1000);
    cached(&GEOMETRY, h.0, 128, || {
        let r = rest(body);
        if part == HumanPart::Hair {
            return hair(&r, outfit);
        }
        let s = slice(part, outfit);
        // shoes stand a little proud of the feet
        let inflate = if part == HumanPart::Shoes { 0.006 } else { 0.0 };
        let normals = if inflate > 0.0 { rest_normals(&r) } else { vec![] };
        let at = |v: u32| {
            let p = r.positions[v as usize];
            if inflate > 0.0 { p + normals[v as usize] * inflate } else { p }
        };
        let mut g = Geometry { rest: vec![], skin: vec![], faces: s.faces.clone() };
        for v in &s.verts {
            match *v {
                SliceVert::Asset(i) => {
                    g.push(at(i), i);
                }
                SliceVert::Cut(i, j, t) => {
                    g.rest.push(at(i).lerp(at(j), t));
                    g.skin.push(blend(influence(i), influence(j), t));
                }
            }
        }
        g
    })
}

/// Hair grown from the scalp: a cap a little off the skin, closed to the scalp at its edge, and
/// for longer styles a curtain hanging from the sides and back (both sides drawn).
fn hair(r: &HumanRest, outfit: &HumanOutfit) -> Geometry {
    let a = asset();
    let length = outfit.hair;
    let mut g = Geometry { rest: vec![], skin: vec![], faces: vec![] };
    if length <= 0.0 {
        return g;
    }
    let base = &a.base[1];
    let eye = base_eye();
    let normals = rest_normals(r);
    let thick = 0.004 + 0.008 * (length / 0.5).min(1.0);
    // the cap: the scalp cut cleanly along the hairline, a little off the skin, thinning to
    // nothing at the hairline itself
    let s = slice(HumanPart::Hair, outfit);
    let cap_at = |v: u32| {
        let p = r.positions[v as usize];
        // more volume on the crown than at the hairline
        let up = ((base[v as usize].y - eye.y) / 0.12).clamp(0.0, 1.0);
        p + normals[v as usize] * thick * (0.6 + 0.6 * up)
    };
    let skin_at = |v: u32| r.positions[v as usize] - normals[v as usize] * 0.001;
    for v in &s.verts {
        match *v {
            SliceVert::Asset(i) => {
                g.push(cap_at(i), i);
            }
            SliceVert::Cut(i, j, t) => {
                // where a curtain hangs from the edge (behind the temples) the cap keeps its
                // thickness so the two meet without a groove
                let curtain = length > 0.6 && base[i as usize].lerp(base[j as usize], t).z <= eye.z - 0.075;
                let p = if curtain { cap_at(i).lerp(cap_at(j), t) } else { skin_at(i).lerp(skin_at(j), t) };
                g.rest.push(p);
                g.skin.push(blend(influence(i), influence(j), t));
            }
        }
    }
    g.faces = s.faces.clone();
    // the cap's edge (cut to cut), in the winding of its face
    let mut boundary: Vec<(u32, u32)> = vec![];
    for f in &s.faces {
        for k in 0..f.len() {
            let (x, y) = (f[k], f[(k + 1) % f.len()]);
            if matches!(s.verts[x as usize], SliceVert::Cut(..)) && matches!(s.verts[y as usize], SliceVert::Cut(..)) {
                boundary.push((x, y));
            }
        }
    }
    // where the curtain hangs from: the hair surface just inside the hairline
    let top_of = |x: u32| -> (Vec3, Vec3, Influence) {
        match s.verts[x as usize] {
            SliceVert::Asset(i) => (cap_at(i), base[i as usize], influence(i)),
            SliceVert::Cut(i, j, t) => (
                cap_at(i).lerp(cap_at(j), t),
                base[i as usize].lerp(base[j as usize], t),
                blend(influence(i), influence(j), t),
            ),
        }
    };
    // a curtain from the sides and back for chin-length and longer hair
    if length > 0.6 {
        let drop = (length - 0.6) / 0.4 * 0.17 + 0.04; // 1 = to the shoulders
        let rest_eye = eye_center(&r.positions);
        let rest_centre = Vec3::new(0.0, rest_eye.y, rest_eye.z - 0.09);
        let steps = 8;
        // keep the hair outside the torso (and the clothes on it): body points by height
        let bin = 0.015f32;
        let mut torso: HashMap<i32, Vec<Vec3>> = HashMap::new();
        for (q, quad) in a.quads.iter().enumerate() {
            if matches!(a.part[q], 0 | 1) && matches!(a.region[q], REGION_HEAD | REGION_TORSO | REGION_HIPS) {
                for v in quad {
                    let p = r.positions[*v as usize];
                    if p.y < rest_eye.y - 0.05 {
                        torso.entry((p.y / bin).floor() as i32).or_default().push(p);
                    }
                }
            }
        }
        // how far out from the head's axis the body reaches in direction `out` at height y
        let reach = |y: f32, out: Vec3| -> f32 {
            let axis = Vec3::new(rest_centre.x, y, rest_centre.z);
            let b = (y / bin).floor() as i32;
            let mut ext = 0.0f32;
            for k in b - 1..=b + 1 {
                for q in torso.get(&k).into_iter().flatten() {
                    let d = Vec3::new(q.x - axis.x, 0.0, q.z - axis.z);
                    let along = d.dot(out);
                    if along > 0.0 && (d - out * along).length_squared() < 0.03 * 0.03 {
                        ext = ext.max(along);
                    }
                }
            }
            ext
        };
        let mut columns: HashMap<u32, Vec<[u32; 2]>> = HashMap::new();
        let mut column = |g: &mut Geometry, v: u32| -> Vec<[u32; 2]> {
            columns
                .entry(v)
                .or_insert_with(|| {
                    let (top, _, inf) = top_of(v);
                    let radial = {
                        let d = top - rest_centre;
                        Vec3::new(d.x, 0.0, d.z).normalize_or(Vec3::NEG_Z)
                    };
                    let top_len = {
                        let d = top - rest_centre;
                        Vec3::new(d.x, 0.0, d.z).length()
                    };
                    let mut least = 0.0f32;
                    let mut out = radial;
                    (0..=steps)
                        .map(|k| {
                            let t = k as f32 / steps as f32;
                            let y = top.y - drop * t;
                            // below the jaw the sides fall behind the shoulders
                            let below = ((rest_eye.y - 0.1 - y) / 0.12).clamp(0.0, 1.0);
                            out = radial.lerp(Vec3::NEG_Z, below * 0.65).normalize_or(Vec3::NEG_Z);
                            let axis = Vec3::new(rest_centre.x, y, rest_centre.z);
                            // outside the body, and never folding back in
                            let len = if k == 0 {
                                top_len
                            } else {
                                (top_len + 0.015 * t.sqrt()).max(reach(y, out) + 0.015).max(least)
                            };
                            least = len;
                            let p = if k == 0 { top } else { axis + out * len };
                            let outer = g.rest.len() as u32;
                            g.rest.push(p);
                            g.skin.push(inf);
                            g.rest.push(p - out * 0.006);
                            g.skin.push(inf);
                            [outer, outer + 1]
                        })
                        .collect()
                })
                .clone()
        };
        for (x, y) in &boundary {
            let (px, py) = (top_of(*x).1, top_of(*y).1);
            // only behind the temples: never over the face
            if px.z > eye.z - 0.075 || py.z > eye.z - 0.075 {
                continue;
            }
            let (cx, cy) = (column(&mut g, *x), column(&mut g, *y));
            for k in 0..steps {
                // outer side faces away from the head, inner side towards it
                g.faces.push(vec![cy[k][0], cx[k][0], cx[k + 1][0], cy[k + 1][0]]);
                g.faces.push(vec![cx[k][1], cy[k][1], cy[k + 1][1], cx[k + 1][1]]);
            }
            // the bottom edge
            g.faces.push(vec![cx[steps][0], cy[steps][0], cy[steps][1], cx[steps][1]]);
        }
    }
    g
}

/// Centre of the eyes in a set of asset positions.
fn eye_center(p: &[Vec3]) -> Vec3 {
    let a = asset();
    let mut sum = Vec3::ZERO;
    let mut n = 0.0f32;
    for (q, part) in a.quads.iter().zip(&a.part) {
        if *part == 3 {
            for i in q {
                sum += p[*i as usize];
                n += 1.0;
            }
        }
    }
    sum / n.max(1.0)
}

fn skinned_positions(g: &Geometry, mats: &[Mat4]) -> Vec<Vec3> {
    g.rest
        .iter()
        .zip(&g.skin)
        .map(|(p, (js, ws))| {
            let mut out = Vec3::ZERO;
            for k in 0..4 {
                if ws[k] > 0.0 {
                    out += mats[js[k] as usize].transform_point3(*p) * ws[k];
                }
            }
            out
        })
        .collect()
}

/// A posed part as an editable mesh (export, mesh edits).
pub fn part_mesh(body: &HumanBody, part: HumanPart, outfit: &HumanOutfit, mats: &[Mat4]) -> Mesh {
    let g = geometry(body, part, outfit);
    Mesh { positions: skinned_positions(&g, mats).into_iter().map(|p| p.to_array()).collect(), faces: g.faces.clone() }
}

/// A posed part triangulated with smooth normals (cached by shape, outfit and joint matrices).
pub fn part_tris(body: &HumanBody, part: HumanPart, outfit: &HumanOutfit, mats: &[Mat4]) -> Rc<TriMesh> {
    let mut h = Hasher::new();
    body.key(&mut h);
    outfit.key(&mut h);
    h.word(part as u32);
    for m in mats {
        for v in m.to_cols_array() {
            h.word(v.to_bits());
        }
    }
    cached(&POSED, h.0, 128, || {
        let g = geometry(body, part, outfit);
        let pos = skinned_positions(&g, mats);
        let mut normals = vec![Vec3::ZERO; pos.len()];
        let mut indices = Vec::with_capacity(g.faces.len() * 6);
        for f in &g.faces {
            // area-weighted normal of the polygon, fanned into triangles
            let p0 = pos[f[0] as usize];
            let mut n = Vec3::ZERO;
            for k in 1..f.len() - 1 {
                n += (pos[f[k] as usize] - p0).cross(pos[f[k + 1] as usize] - p0);
                indices.extend_from_slice(&[f[0], f[k], f[k + 1]]);
            }
            for i in f {
                normals[*i as usize] += n;
            }
        }
        TriMesh {
            positions: pos.iter().map(|p| p.to_array()).collect(),
            normals: normals.iter().map(|n| n.normalize_or(Vec3::Y).to_array()).collect(),
            indices,
        }
    })
}

/// Number of joints a human skeleton has.
pub fn joint_count() -> usize {
    asset().bones.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_loads_and_stands_on_the_ground() {
        let a = asset();
        assert_eq!(a.bones.len(), 52);
        assert!(a.verts > 10_000);
        let man = rest(&HumanBody::default());
        let h = man.height();
        assert!((1.65..1.85).contains(&h), "man height {h}");
        let woman = rest(&HumanBody { gender: 0.0, ..Default::default() });
        assert!(woman.height() < h);
        let child = rest(&HumanBody { age: 11.0, ..Default::default() });
        assert!(child.height() < 1.6, "child {}", child.height());
    }

    #[test]
    fn arms_hang_down_at_rest() {
        let r = rest(&HumanBody::default());
        let names = joint_names();
        let at = |n: &str| r.joints[names.iter().position(|x| *x == n).unwrap()];
        let (sh, el, wr) = (at("shoulder_l"), at("elbow_l"), at("wrist_l"));
        assert!(sh.x > 0.1, "left is +X");
        assert!((el.x - sh.x).abs() < 1e-3 && el.y < sh.y);
        assert!((wr.x - sh.x).abs() < 1e-3 && wr.y < el.y);
        // and the skinned rest mesh puts the left hand's vertices near the wrist joint
        let mats = skin_matrices(&r, Mat4::IDENTITY, &[]);
        let skin = part_tris(&HumanBody::default(), HumanPart::Skin, &HumanOutfit::default(), &mats);
        let lowest_hand = skin
            .positions
            .iter()
            .map(|p| Vec3::from_array(*p))
            .filter(|p| p.x > 0.1 && p.y > 0.5)
            .fold(f32::INFINITY, |m, p| m.min(p.y));
        assert!(lowest_hand < wr.y && lowest_hand > wr.y - 0.25, "hand tip {lowest_hand}, wrist {}", wr.y);
    }

    #[test]
    fn outfit_parts_do_not_overlap() {
        let o = HumanOutfit::default();
        let n = |p| slice(p, &o).faces.len();
        let all: usize = PARTS.iter().map(|p| n(*p)).sum();
        assert!(n(HumanPart::Top) > 100 && n(HumanPart::Bottom) > 100 && n(HumanPart::Shoes) > 50);
        assert!(all < asset().quads.len());
        let bare = HumanOutfit { top: false, bottom: false, shoes: false, ..o };
        assert!(slice(HumanPart::Skin, &bare).faces.len() > n(HumanPart::Skin));
    }

    #[test]
    fn expressions_use_known_units() {
        for e in EXPRESSIONS {
            for (u, _) in e.units {
                assert!(EXPRESSION_UNITS.contains(u), "{} uses unknown unit {u}", e.name);
                assert!(asset().morphs.contains_key(&format!("expr:{u}")));
            }
        }
        let smile = HumanBody {
            face: EXPRESSIONS[1].units.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            ..Default::default()
        };
        smile.validate().unwrap();
        assert_ne!(rest(&smile).positions, rest(&HumanBody::default()).positions);
    }
}

#[cfg(test)]
mod perf {
    use super::*;

    #[test]
    #[ignore]
    fn timing() {
        let t = std::time::Instant::now();
        let b = HumanBody { weight: 0.3, ..Default::default() };
        let r = rest(&b);
        println!("rest {:?}", t.elapsed());
        let o = HumanOutfit { hair: 1.4, ..Default::default() };
        let t = std::time::Instant::now();
        for p in PARTS {
            geometry(&b, *p, &o);
        }
        println!("geometry {:?}", t.elapsed());
        for i in 0..3 {
            let t = std::time::Instant::now();
            let mats = skin_matrices(&r, Mat4::from_rotation_y(i as f32 * 0.1), &[]);
            for p in PARTS {
                part_tris(&b, *p, &o, &mats);
            }
            println!("pose {:?}", t.elapsed());
        }
    }
}
