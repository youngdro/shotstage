//! Scene document: entities in a parent/child hierarchy, plus the timeline.

use glam::{EulerRot, Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::camera::CameraData;
use crate::image::{Asset, ImageData};
use crate::light::LightData;
use crate::mesh::{MeshSource, Modifier};
use crate::timeline::Timeline;
use crate::{CoreError, Result};

/// Current scene file format version.
pub const SCENE_VERSION: u32 = 1;

/// Position, rotation and scale. Rotation is stored as a quaternion `[x, y, z, w]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Transform {
    /// Metres. +Y is up, the default camera looks down -Z.
    pub translation: [f32; 3],
    /// Unit quaternion `[x, y, z, w]`.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self { translation: [0.0; 3], rotation: [0.0, 0.0, 0.0, 1.0], scale: [1.0; 3] }
    }
}

impl Transform {
    pub fn translation(&self) -> Vec3 {
        Vec3::from_array(self.translation)
    }
    pub fn rotation(&self) -> Quat {
        let q = Quat::from_array(self.rotation);
        if q.length_squared() < 1e-12 { Quat::IDENTITY } else { q.normalize() }
    }
    pub fn scale(&self) -> Vec3 {
        Vec3::from_array(self.scale)
    }
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale(), self.rotation(), self.translation())
    }
    pub fn from_parts(t: Vec3, r: Quat, s: Vec3) -> Self {
        Self { translation: t.to_array(), rotation: r.normalize().to_array(), scale: s.to_array() }
    }
    /// Euler angles in degrees, applied in Y (yaw), X (pitch), Z (roll) order.
    pub fn euler_degrees(&self) -> [f32; 3] {
        let (y, x, z) = self.rotation().to_euler(EulerRot::YXZ);
        [x.to_degrees(), y.to_degrees(), z.to_degrees()]
    }
    /// Set rotation from `[pitch_x, yaw_y, roll_z]` degrees (YXZ order).
    pub fn set_euler_degrees(&mut self, e: [f32; 3]) {
        let q = Quat::from_euler(EulerRot::YXZ, e[1].to_radians(), e[0].to_radians(), e[2].to_radians());
        self.rotation = q.to_array();
    }
}

/// What an entity is.
// (entities live in a Vec of whole entities; boxing a variant would only add a pointer hop)
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EntityKind {
    /// A grouping node with no geometry.
    Empty,
    /// Editable geometry with a non-destructive modifier stack.
    Mesh {
        source: MeshSource,
        #[serde(default)]
        modifiers: Vec<Modifier>,
        /// Display colour, linear RGB 0..1 (multiplies the texture when there is one).
        #[serde(default = "default_color")]
        color: [f32; 3],
        /// Texture and how it is laid on (see `mesh.setMaterial`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        material: Option<crate::uv::Material>,
    },
    /// A shot camera.
    Camera(CameraData),
    /// A light (optional: scenes without lights use the studio clay light).
    Light(LightData),
    /// A picture on a card (reference, billboard, backdrop, decal).
    Image(ImageData),
}

fn default_color() -> [f32; 3] {
    [0.72, 0.70, 0.66]
}

impl EntityKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            EntityKind::Empty => "empty",
            EntityKind::Mesh { .. } => "mesh",
            EntityKind::Camera(_) => "camera",
            EntityKind::Light(_) => "light",
            EntityKind::Image(_) => "image",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Entity {
    /// Stable id used by every command, e.g. `chair_1`.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Locked entities (a floor, a backdrop) can be selected but not dragged in the viewport,
    /// so dragging on them orbits the view. Commands still edit them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    pub kind: EntityKind,
}

fn yes() -> bool {
    true
}

/// The whole document: what gets saved, loaded, undone and sent to agents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    pub version: u32,
    /// Entities in creation order (the hierarchy is given by `parent`).
    pub entities: Vec<Entity>,
    pub timeline: Timeline,
    /// The camera the viewport and exports look through, if any.
    #[serde(default)]
    pub active_camera: Option<String>,
    /// Images used by image entities, by key.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub assets: std::collections::BTreeMap<String, Asset>,
    /// Reference pictures laid over a camera's view to match a shot, by camera id.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub references: std::collections::BTreeMap<String, CameraReference>,
    /// Sky light, background and shadow settings.
    #[serde(default, skip_serializing_if = "crate::environment::Environment::is_default")]
    pub environment: crate::environment::Environment,
}

/// A picture shown over a camera's view (a storyboard frame, a reference photo).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CameraReference {
    /// Asset key.
    pub asset: String,
    /// 0..1.
    pub opacity: f32,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            version: SCENE_VERSION,
            entities: Vec::new(),
            timeline: Timeline::default(),
            active_camera: None,
            assets: Default::default(),
            references: Default::default(),
            environment: Default::default(),
        }
    }
}

/// Id must be 1..=64 chars of letters, digits, `_` or `-`.
pub fn validate_id(id: &str) -> Result<()> {
    let ok = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok { Ok(()) } else { Err(CoreError::Invalid(format!("bad id `{id}`: use 1-64 of [A-Za-z0-9_-]"))) }
}

/// Finding an entity by id is a scan of the entity list; drawing a frame looks up every
/// entity, its parents and a person's joints, which made big scenes quadratic. So a scene that
/// is looked up again and again without changing gets a hash index, kept per thread for the
/// last few entity lists. Every hit is checked against the list itself, and anything the index
/// does not know falls back to the scan, so a stale index can never give a wrong answer.
mod id_index {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::Entity;

    /// Below this many entities the scan is as fast as hashing.
    const MIN_LEN: usize = 48;
    /// Scans of the same list before it is indexed (a list that keeps growing, like a template
    /// being built, is not worth indexing on every step).
    const SCANS_BEFORE_INDEX: u32 = 16;
    const SLOTS: usize = 4;

    struct Index {
        key: (usize, usize),
        map: HashMap<u64, u32>,
    }

    #[derive(Default)]
    struct State {
        indexes: Vec<Index>,
        /// The list being scanned without an index, and how many times.
        scanning: ((usize, usize), u32),
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    fn hash(id: &str) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for b in id.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }

    pub(super) fn position(entities: &[Entity], id: &str) -> Option<usize> {
        let scan = || entities.iter().position(|e| e.id == id);
        if entities.len() < MIN_LEN {
            return scan();
        }
        let key = (entities.as_ptr() as usize, entities.len());
        let h = hash(id);
        let hit = STATE.with(|s| s.borrow().indexes.iter().find(|x| x.key == key).and_then(|x| x.map.get(&h).copied()));
        if let Some(i) = hit
            && entities.get(i as usize).is_some_and(|e| e.id == id)
        {
            return Some(i as usize);
        }
        let found = scan()?;
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.scanning.0 == key {
                s.scanning.1 += 1;
            } else {
                s.scanning = (key, 1);
            }
            if s.scanning.1 >= SCANS_BEFORE_INDEX {
                s.scanning = ((0, 0), 0);
                s.indexes.retain(|x| x.key != key);
                if s.indexes.len() >= SLOTS {
                    s.indexes.remove(0);
                }
                let map = entities.iter().enumerate().map(|(i, e)| (hash(&e.id), i as u32)).collect();
                s.indexes.push(Index { key, map });
            }
        });
        Some(found)
    }
}

impl Scene {
    pub fn get(&self, id: &str) -> Result<&Entity> {
        id_index::position(&self.entities, id).map(|i| &self.entities[i]).ok_or_else(|| CoreError::NoEntity(id.into()))
    }
    pub fn get_mut(&mut self, id: &str) -> Result<&mut Entity> {
        match id_index::position(&self.entities, id) {
            Some(i) => Ok(&mut self.entities[i]),
            None => Err(CoreError::NoEntity(id.into())),
        }
    }
    pub fn contains(&self, id: &str) -> bool {
        id_index::position(&self.entities, id).is_some()
    }

    /// A fresh id like `cube_3` that is not taken yet.
    pub fn unique_id(&self, stem: &str) -> String {
        let stem: String = stem
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .take(48)
            .collect();
        let stem = if stem.is_empty() { "entity".to_string() } else { stem };
        (1..).map(|n| format!("{stem}_{n}")).find(|id| !self.contains(id)).expect("unbounded")
    }

    pub fn insert(&mut self, entity: Entity) -> Result<()> {
        validate_id(&entity.id)?;
        if self.contains(&entity.id) {
            return Err(CoreError::DuplicateId(entity.id));
        }
        if let Some(p) = &entity.parent {
            self.get(p)?;
        }
        self.entities.push(entity);
        Ok(())
    }

    /// Remove an entity and all its descendants; returns the removed ids.
    pub fn remove(&mut self, id: &str) -> Result<Vec<String>> {
        self.get(id)?;
        let mut doomed = vec![id.to_string()];
        let mut i = 0;
        while i < doomed.len() {
            let cur = doomed[i].clone();
            doomed.extend(self.entities.iter().filter(|e| e.parent.as_deref() == Some(&cur)).map(|e| e.id.clone()));
            i += 1;
        }
        self.entities.retain(|e| !doomed.contains(&e.id));
        self.timeline.tracks.retain(|t| !doomed.contains(&t.entity));
        self.timeline.shots.retain(|s| !doomed.contains(&s.camera));
        self.references.retain(|cam, _| !doomed.contains(cam));
        if self.active_camera.as_ref().is_some_and(|c| doomed.contains(c)) {
            self.active_camera = None;
        }
        Ok(doomed)
    }

    /// Re-parent, refusing cycles. `None` moves the entity to the root.
    pub fn set_parent(&mut self, id: &str, parent: Option<&str>) -> Result<()> {
        self.get(id)?;
        if let Some(p) = parent {
            let mut cur = Some(p.to_string());
            while let Some(c) = cur {
                if c == id {
                    return Err(CoreError::Invalid(format!("parenting `{id}` to `{p}` would make a cycle")));
                }
                cur = self.get(&c)?.parent.clone();
            }
        }
        self.get_mut(id)?.parent = parent.map(str::to_string);
        Ok(())
    }

    /// World matrix of an entity using its stored (rest) transforms.
    pub fn world_matrix(&self, id: &str) -> Result<Mat4> {
        let mut m = Mat4::IDENTITY;
        let mut cur = Some(id.to_string());
        let mut guard = 0;
        while let Some(c) = cur {
            let e = self.get(&c)?;
            m = e.transform.matrix() * m;
            cur = e.parent.clone();
            guard += 1;
            if guard > 10_000 {
                return Err(CoreError::Invalid("hierarchy too deep or cyclic".into()));
            }
        }
        Ok(m)
    }

    pub fn children(&self, id: Option<&str>) -> impl Iterator<Item = &Entity> {
        self.entities.iter().filter(move |e| e.parent.as_deref() == id)
    }

    pub fn camera(&self, id: &str) -> Result<&CameraData> {
        match &self.get(id)?.kind {
            EntityKind::Camera(c) => Ok(c),
            _ => Err(CoreError::WrongKind(id.into(), "camera")),
        }
    }
}
