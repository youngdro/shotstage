//! The egui editor: outliner, properties, timeline, command console and the 3D viewport.
//! Every edit goes through [`crate::hub::dispatch`]; the UI keeps only view state.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Vec2};
use glam::{Mat4, Vec3, Vec4Swizzles};
use serde_json::{Value, json};
use shotstage_commands::{prefabs, rig};
use shotstage_core::eval::{camera_data_at, camera_id_at, local_at, world_at};
use shotstage_core::spatial::{self, RayOptions, raycast};
use shotstage_core::{EntityKind, Modifier, Scene, TrackProperty};
use shotstage_render::{Overlay, Pass, Renderer, View, pick};

use crate::hub::{self, SharedHub};

/// Something in the block palette.
#[derive(Clone)]
enum Block {
    Shape(&'static str, Value, [f32; 3]),
    /// prefab kind, label, description
    Prefab(&'static str, &'static str, &'static str),
    /// light kind, label, description
    Light(&'static str, &'static str, &'static str),
    /// lighting preset, label, description (click only)
    LightPreset(&'static str, &'static str, &'static str),
}

impl Block {
    fn label(&self) -> &'static str {
        match self {
            Block::Shape(l, ..) | Block::Prefab(_, l, _) | Block::Light(_, l, _) | Block::LightPreset(_, l, _) => l,
        }
    }
    fn description(&self) -> &'static str {
        match self {
            Block::Shape(..) => "Basic shape (edit its size in Mesh / with mesh.setPrimitive)",
            Block::Prefab(_, _, d) | Block::Light(_, _, d) | Block::LightPreset(_, _, d) => d,
        }
    }
    /// Key of this block's palette thumbnail.
    fn thumb_key(&self) -> Option<String> {
        match self {
            Block::Prefab(k, ..) => Some(format!("prefab:{k}")),
            Block::Shape(l, ..) => Some(format!("shape:{l}")),
            _ => None,
        }
    }

    fn matches(&self, filter: &str) -> bool {
        filter.is_empty() || {
            let key = match self {
                Block::Shape(..) => "",
                Block::Prefab(k, ..) | Block::Light(k, ..) | Block::LightPreset(k, ..) => k,
            };
            self.label().to_lowercase().contains(filter)
                || key.to_lowercase().contains(filter)
                || self.description().to_lowercase().contains(filter)
        }
    }
}

/// The one accent colour of the UI.
const ACCENT: Color32 = Color32::from_rgb(70, 150, 255);

const HELP: &str = "Blocks: drag from the left panel into the view (or click to drop in the middle).\n\
Object: click to select (a second click picks a part); drag to slide it over surfaces.\n\
Shift+drag: lift · Alt+drag: copy · R / Shift+R or wheel while dragging: turn · Ctrl: no snap · Esc: cancel.\n\
People: click a body part to select its joint, then drag the part to bend it, or turn the coloured rings.\n\
View: drag empty space to orbit · right-drag or Shift+drag to pan · scroll or pinch to zoom · double-click or F to focus.\n\
Keys: Cmd/Ctrl+Z undo · Cmd/Ctrl+Shift+Z redo · Cmd/Ctrl+D duplicate · Delete · Space play.";

/// A collapsible properties section.
fn section(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(egui::RichText::new(title).strong()).id_salt(title).default_open(open).show(ui, add);
}

/// The joint a click on `hit` (at `point`) means: a person's body part's joint, or the nearest
/// bone of an imported skeleton.
fn joint_at(scene: &Scene, hit: &str, point: Vec3, frame: f32) -> Option<String> {
    rig::joint_of(scene, hit).or_else(|| rig::joint_near(scene, hit, point, frame))
}

/// Mesh under a ray, with the point hit.
fn pick_point(scene: &Scene, frame: f32, o: Vec3, d: Vec3) -> Option<(String, Vec3)> {
    // the ray starts at the viewer: billboards are hit where they are drawn
    let h = raycast(scene, frame, o, d, &RayOptions { eye: Some(o), ..Default::default() })?;
    Some((h.id?, Vec3::from_array(h.point)))
}

/// The joint a drag on `joint`'s body part should turn: hands swing the forearm, feet the shin.
fn drive_joint(scene: &Scene, joint: &str) -> String {
    let parent = |j: &str| scene.get(j).ok().and_then(|e| e.parent.clone());
    match rig::joint_name(scene, joint) {
        Some("wrist_l" | "wrist_r" | "ankle_l" | "ankle_r") => parent(joint).unwrap_or_else(|| joint.to_string()),
        _ => joint.to_string(),
    }
}

/// A command id as words for the History menu.
fn friendly(command: &str) -> String {
    match command {
        "entity.place" | "entity.moveTo" => "Move".into(),
        "entity.rotate" => "Turn".into(),
        "entity.setTransform" => "Edit transform".into(),
        "entity.create" => "Add".into(),
        "entity.delete" => "Delete".into(),
        "entity.duplicate" => "Duplicate".into(),
        "prefab.create" => "Add block".into(),
        "pose.set" | "pose.aim" => "Pose".into(),
        "motion.apply" => "Animate".into(),
        "keyframe.set" => "Key".into(),
        "keyframe.move" => "Retime key".into(),
        "keyframe.delete" => "Delete key".into(),
        "camera.frame" => "Frame shot".into(),
        "model.import" => "Import model".into(),
        "image.import" => "Add picture".into(),
        "light.preset" => "Lighting preset".into(),
        "scene.template" => "Template".into(),
        "batch" => "Several changes".into(),
        other => other.replace('.', " "),
    }
}

/// What the thumbnail renderer creates for each palette block (with id `thumb`).
fn thumb_items() -> Vec<(String, Value)> {
    let mut out = vec![];
    for (_, _, blocks) in palette() {
        for b in blocks {
            let Some(key) = b.thumb_key() else { continue };
            let create = match &b {
                Block::Prefab(kind, ..) => json!({"command": "prefab.create", "params": {"kind": kind, "id": "thumb"}}),
                Block::Shape(_, prim, color) => {
                    json!({"command": "entity.create", "params": {"type": "mesh", "id": "thumb", "primitive": prim, "color": color}})
                }
                _ => continue,
            };
            out.push((key, create));
        }
    }
    out
}

/// Palette sections: title, open at start, blocks.
fn palette() -> Vec<(&'static str, bool, Vec<Block>)> {
    use prefabs::Category;
    let grey = [0.72, 0.70, 0.66];
    let shapes = vec![
        Block::Shape("Cube", json!({"shape": "cube", "size": [0.5, 0.5, 0.5]}), grey),
        Block::Shape("Slab", json!({"shape": "cube", "size": [1.0, 0.1, 1.0]}), grey),
        Block::Shape("Pillar", json!({"shape": "cube", "size": [0.3, 2.0, 0.3]}), grey),
        Block::Shape("Sphere", json!({"shape": "sphere", "radius": 0.25, "segments": 24, "rings": 12}), grey),
        Block::Shape("Cylinder", json!({"shape": "cylinder", "radius": 0.25, "height": 0.5, "segments": 24}), grey),
        Block::Shape("Cone", json!({"shape": "cone", "radius": 0.25, "height": 0.5, "segments": 24}), grey),
        Block::Shape(
            "Torus",
            json!({"shape": "torus", "majorRadius": 0.3, "minorRadius": 0.08, "majorSegments": 32, "minorSegments": 12}),
            grey,
        ),
        Block::Shape("Floor", json!({"shape": "plane", "size": [12, 12]}), [0.5, 0.5, 0.52]),
    ];
    let of = |c: Category| -> Vec<Block> {
        prefabs::PREFABS
            .iter()
            .filter(|p| p.category == c && p.key != "person")
            .map(|p| Block::Prefab(p.key, p.label, p.description))
            .collect()
    };
    vec![
        ("People", true, of(Category::People)),
        ("Fresh retail", true, of(Category::Retail)),
        ("Product shoot", true, of(Category::Product)),
        ("Furniture & set", false, of(Category::Furniture)),
        (
            "Lights",
            false,
            vec![
                Block::Light("sun", "Sun", "Parallel sunlight; aim it to set the time of day"),
                Block::Light("point", "Point", "A bare bulb: light in every direction"),
                Block::Light("spot", "Spot", "A cone of light (stage light, rim light)"),
                Block::Light("area", "Area", "A soft panel: softbox, window"),
                Block::LightPreset(
                    "threePoint",
                    "3-point",
                    "Key, fill and rim around the selection (or the whole scene)",
                ),
                Block::LightPreset("rembrandt", "Rembrandt", "One high side key light, moody"),
                Block::LightPreset(
                    "productSoftbox",
                    "Product",
                    "Two side softboxes and one overhead: even product light",
                ),
                Block::LightPreset("backlight", "Backlight", "Rim light from behind"),
                Block::LightPreset("goldenHour", "Golden hr", "Warm low sun from the side"),
                Block::LightPreset("daylight", "Daylight", "High neutral sun"),
                Block::LightPreset("clear", "No lights", "Remove all lights: back to the flat studio light"),
            ],
        ),
        ("Shapes", false, shapes),
    ]
}

/// A block being dragged in from the palette.
struct PaletteDrag {
    block: Block,
    spawned: Option<String>,
    key: String,
}

/// What the current pointer drag in the viewport does.
#[derive(Clone)]
enum Gesture {
    Idle,
    Navigate,
    /// Slide an object over surfaces (Shift: lift it straight up).
    Move {
        id: String,
        grab: Vec3,
        offset: Vec3,
        key: String,
        lift: Option<(Vec3, f32)>,
    },
    /// Gizmo: move along a world axis.
    Axis {
        id: String,
        axis: Vec3,
        start: Vec3,
        t0: f32,
        key: String,
    },
    /// Gizmo: turn about a world axis (0 = X, 1 = Y, 2 = Z).
    Ring {
        id: String,
        axis: usize,
        last: f32,
        key: String,
    },
    /// Bend a person's joint so the dragged limb follows the pointer.
    Aim {
        joint: String,
        /// Grab point: the pointer moves on the plane through it facing the view.
        grab: Vec3,
        key: String,
    },
    /// Gizmo: uniform scale.
    Scale {
        id: String,
        key: String,
    },
    /// Drag a key point of a motion path (moves that key's position).
    PathKey {
        id: String,
        frame: f32,
        origin: Vec3,
        key: String,
    },
    /// Esc pressed: ignore the rest of this drag.
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Handle {
    Axis(usize),
    Ring(usize),
    Scale,
}

fn ray_at(rect: Rect, view: &View, pos: Pos2) -> (Vec3, Vec3) {
    let ndc = glam::Vec2::new(
        (pos.x - rect.left()) / rect.width() * 2.0 - 1.0,
        1.0 - (pos.y - rect.top()) / rect.height() * 2.0,
    );
    let inv = (view.proj * view.view).inverse();
    let near = inv.project_point3(Vec3::new(ndc.x, ndc.y, 0.0));
    let far = inv.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
    (near, (far - near).normalize_or_zero())
}

fn project(rect: Rect, view: &View, p: Vec3) -> Option<Pos2> {
    let c = view.proj * view.view * p.extend(1.0);
    if c.w <= 1e-5 {
        return None;
    }
    let n = c.xyz() / c.w;
    Some(egui::pos2(rect.left() + (n.x + 1.0) / 2.0 * rect.width(), rect.top() + (1.0 - n.y) / 2.0 * rect.height()))
}

/// Parameter along the line `p + a·s` closest to the ray `o + d·t`.
fn closest_on_line(p: Vec3, a: Vec3, o: Vec3, d: Vec3) -> Option<f32> {
    let w0 = p - o;
    let (aa, ab, bb, ad, bd) = (a.dot(a), a.dot(d), d.dot(d), a.dot(w0), d.dot(w0));
    let denom = aa * bb - ab * ab;
    (denom.abs() > 1e-6).then(|| (ab * bd - bb * ad) / denom)
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let (ab, ap) = (b - a, p - a);
    let t = (ap.dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

/// The top-level group an entity belongs to (a prefab's root).
fn root_of(scene: &Scene, id: &str) -> String {
    let mut cur = id.to_string();
    for _ in 0..10_000 {
        match scene.get(&cur).ok().and_then(|e| e.parent.clone()) {
            Some(p) => cur = p,
            None => break,
        }
    }
    cur
}

/// The mesh under a viewport ray that drags may grab (locked groups let the drag through).
fn pick_grabbable(scene: &Scene, frame: f32, o: Vec3, d: Vec3) -> Option<String> {
    let hit = pick_point(scene, frame, o, d)?.0;
    let locked = scene.get(&root_of(scene, &hit)).map(|e| e.locked).unwrap_or(false)
        || scene.get(&hit).map(|e| e.locked).unwrap_or(false);
    (!locked).then_some(hit)
}

/// A row of the track strip: one object (a person with all its joints) and its key frames.
struct TrackRow {
    id: String,
    label: String,
    tracks: Vec<(String, TrackProperty)>,
    frames: Vec<f32>,
}

/// A keyframe diamond being dragged along the track strip.
#[derive(Clone)]
struct KeyDrag {
    row: String,
    frame: f32,
    key: String,
}

fn timeline_rows(scene: &Scene) -> Vec<TrackRow> {
    let mut rows: Vec<TrackRow> = vec![];
    for tr in &scene.timeline.tracks {
        if tr.keys.is_empty() || !scene.contains(&tr.entity) {
            continue;
        }
        let root = root_of(scene, &tr.entity);
        let i = match rows.iter().position(|r| r.id == root) {
            Some(i) => i,
            None => {
                let name = scene.get(&root).map(|e| e.name.clone()).unwrap_or_else(|_| root.clone());
                rows.push(TrackRow { id: root.clone(), label: name, tracks: vec![], frames: vec![] });
                rows.len() - 1
            }
        };
        rows[i].tracks.push((tr.entity.clone(), tr.property));
        rows[i].frames.extend(tr.keys.iter().map(|k| k.frame));
    }
    for r in &mut rows {
        r.frames.sort_by(|a, b| a.total_cmp(b));
        r.frames.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    }
    rows
}

fn is_animated(scene: &Scene, id: &str) -> bool {
    scene.timeline.track(id, TrackProperty::Transform).is_some()
}

struct Orbit {
    target: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
}

impl Orbit {
    fn eye(&self) -> Vec3 {
        let dir = Vec3::new(self.pitch.cos() * self.yaw.sin(), self.pitch.sin(), self.pitch.cos() * self.yaw.cos());
        self.target + dir * self.distance
    }
    fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }
}

pub struct EditorApp {
    hub: SharedHub,
    renderer: Option<Renderer>,
    texture: Option<(egui::TextureId, u64)>,
    orbit: Orbit,
    look_through: bool,
    pass: Pass,
    playing: bool,
    console: String,
    console_out: String,
    /// Bumped at the start of each drag so a drag is one undo step.
    gesture: u64,
    snap: bool,
    snap_step: f32,
    drag: Gesture,
    palette: Option<PaletteDrag>,
    hovered: Option<String>,
    hot_handle: Option<Handle>,
    /// Viewport rect and camera of the last frame (for clicks on palette tiles).
    last_view: Option<(Rect, View)>,
    frame_angle: &'static str,
    frame_side: &'static str,
    block_filter: String,
    show_console: bool,
    /// The camera the next picked picture becomes the reference of.
    reference_for: Option<String>,
    /// egui textures of reference pictures: asset key -> (identity of the pixels, texture).
    ref_textures: std::collections::HashMap<String, (usize, egui::TextureHandle)>,
    /// Palette thumbnails (rendered in the background).
    thumbs: Option<crate::thumbs::Thumbs>,
    /// Draw action / title safe frames when looking through a camera.
    safe_areas: bool,
    /// Result of a background "save frame".
    saved: crate::files::Outbox,
    /// Key every edit at the current frame.
    auto_key: bool,
    key_drag: Option<KeyDrag>,
    /// The key a context menu on the track strip is about: (row, frame).
    key_menu: Option<(String, f32)>,
    motion_filter: String,
    motion_secs: f32,
    motion_strength: f32,
    /// Mode for the next picked image (card, billboard, backdrop, floor).
    image_mode: Option<&'static str>,
    /// Ignore scene lights (view option).
    studio_light: bool,
    /// The last object (group) selected: what lights and presets aim at.
    last_subject: Option<String>,
    /// A short status message and when it appeared.
    toast: Option<(String, f64)>,
    /// Files picked or dropped, waiting to be imported: (name, bytes, drop position).
    incoming: crate::files::Inbox,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>, hub: SharedHub) -> Self {
        hub.borrow_mut().repaint = Some(cc.egui_ctx.clone());
        hub.borrow_mut().gpu = cc.wgpu_render_state.clone();
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        cc.egui_ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = egui::vec2(6.0, 5.0);
            s.spacing.button_padding = egui::vec2(8.0, 3.0);
            s.spacing.interact_size.y = 22.0;
            s.visuals.selection.bg_fill = Color32::from_rgb(40, 98, 170);
            s.visuals.selection.stroke.color = Color32::WHITE;
            s.visuals.hyperlink_color = ACCENT;
        });
        let renderer = cc.wgpu_render_state.as_ref().map(|rs| Renderer::new(&rs.device));
        Self {
            hub,
            renderer,
            texture: None,
            orbit: Orbit { target: Vec3::new(0.0, 0.5, 0.0), yaw: 0.6, pitch: 0.35, distance: 9.0 },
            look_through: false,
            pass: Pass::Clay,
            playing: false,
            console: r#"{"command": "entity.create", "params": {"type": "mesh", "primitive": {"shape": "sphere", "radius": 0.5, "segments": 24, "rings": 12}, "position": [0, 0.5, 0]}}"#.into(),
            console_out: String::new(),
            gesture: 0,
            snap: true,
            snap_step: 0.25,
            drag: Gesture::Idle,
            palette: None,
            hovered: None,
            hot_handle: None,
            last_view: None,
            frame_angle: "eyeLevel",
            frame_side: "front",
            block_filter: String::new(),
            show_console: false,
            image_mode: None,
            saved: Default::default(),
            safe_areas: false,
            thumbs: None,
            reference_for: None,
            ref_textures: Default::default(),
            auto_key: false,
            key_drag: None,
            key_menu: None,
            motion_filter: String::new(),
            motion_secs: 0.0,
            motion_strength: 1.0,
            studio_light: false,
            last_subject: None,
            toast: None,
            incoming: Default::default(),
        }
    }

    fn run(&self, command: &str, params: Value) -> Option<Value> {
        hub::run(&self.hub, command, params)
    }

    /// Run a command whose repeated calls during one gesture merge into one undo step.
    fn run_merged(&self, key: &str, command: &str, params: Value) {
        let req = json!({ "command": command, "params": params, "merge": format!("{key}:{}", self.gesture) });
        if let Err(e) = hub::dispatch(&self.hub, req) {
            log::warn!("{command}: {e}");
        }
    }

    /// Run with an explicit merge key (one undo step per key); returns the result.
    fn run_key(&self, key: &str, command: &str, params: Value) -> Option<Value> {
        match hub::dispatch(&self.hub, json!({ "command": command, "params": params, "merge": key })) {
            Ok(o) => Some(o.result),
            Err(e) => {
                log::warn!("{command}: {e}");
                None
            }
        }
    }

    fn new_key(&mut self, tag: &str) -> String {
        self.gesture += 1;
        format!("{tag}:{}", self.gesture)
    }

    fn snap_value(&self, ctrl: bool) -> Value {
        if self.snap && !ctrl { json!(self.snap_step) } else { Value::Null }
    }

    /// Create a palette block (unplaced) under a merge key.
    fn spawn(&self, block: &Block, key: &str) -> Option<String> {
        let r = match block {
            Block::Shape(name, prim, color) => self.run_key(
                key,
                "entity.create",
                json!({"type": "mesh", "primitive": prim, "color": color, "name": name.to_lowercase(), "locked": *name == "Floor"}),
            )?,
            Block::Prefab(kind, ..) => self.run_key(key, "prefab.create", json!({"kind": kind, "drop": false}))?,
            Block::Light(kind, label, _) => self.run_key(
                key,
                "entity.create",
                json!({"type": "light", "name": label.to_lowercase(), "light": {"kind": kind}, "position": [0, 2.2, 0]}),
            )?,
            Block::LightPreset(..) => return None,
        };
        r["id"].as_str().map(str::to_string)
    }

    /// Drop `id` where a viewport ray lands.
    fn place_on_ray(&self, key: &str, id: &str, ray: (Vec3, Vec3), offset: Vec3, ctrl: bool) {
        let scene = self.scene();
        let f = self.frame();
        if matches!(scene.get(id).map(|e| &e.kind), Ok(EntityKind::Light(_))) {
            // a light hangs above where it is dropped, a little towards the viewer, aimed at it
            let opts = RayOptions { ignore: &[id.to_string()], ground: true, ..Default::default() };
            if let Some(h) = raycast(&scene, f, ray.0, ray.1, &opts) {
                let at = Vec3::from_array(h.point);
                let back = Vec3::new(-ray.1.x, 0.0, -ray.1.z).normalize_or_zero();
                let pos = at + Vec3::Y * 2.0 + back * 1.2;
                self.run_key(key, "entity.moveTo", json!({"id": id, "position": pos.to_array(), "frame": f}));
                self.run_key(key, "camera.lookAt", json!({"id": id, "target": (at + Vec3::Y * 0.5).to_array()}));
            }
            return;
        }
        let mut p = json!({"id": id, "ray": {"origin": ray.0.to_array(), "direction": ray.1.to_array()},
            "offset": offset.to_array(), "snap": self.snap_value(ctrl), "frame": f});
        if self.keyed(&scene, id) {
            p["keyframeAt"] = json!(f.round());
        }
        if p["snap"].is_null() {
            p.as_object_mut().map(|o| o.remove("snap"));
        }
        self.run_key(key, "entity.place", p);
    }

    fn apply_template(&mut self, name: &str, label: &str) {
        match hub::dispatch(&self.hub, json!({"command": "scene.template", "params": {"name": name}})) {
            Ok(o) => {
                let scene = self.scene();
                self.select(None);
                self.hub.borrow_mut().frame = scene.timeline.start as f32;
                if let Some(Some(b)) = scene
                    .entities
                    .iter()
                    .filter(|e| e.parent.is_none())
                    .map(|e| spatial::bounds(&scene, &e.id, 0.0).ok().flatten())
                    .reduce(spatial::Aabb::union)
                {
                    self.orbit.target = b.center();
                    self.orbit.distance = (b.size().max_element() * 1.4).clamp(2.0, 60.0);
                    // look from where the shot camera looks, a little further back
                    if let Some(cam) = camera_id_at(&scene, scene.timeline.start as f32)
                        && let Ok(w) = world_at(&scene, &cam, scene.timeline.start as f32)
                    {
                        // centre on what the camera looks at (the product, not the backdrop)
                        let eye = w.w_axis.truncate();
                        let fwd = w.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
                        let opts = RayOptions { ground: true, ..Default::default() };
                        let at = raycast(&scene, 0.0, eye, fwd, &opts)
                            .map(|h| Vec3::from_array(h.point))
                            .unwrap_or(b.center());
                        self.orbit.target = at;
                        let dir = eye - at;
                        if dir.length() > 0.1 {
                            self.orbit.yaw = dir.x.atan2(dir.z);
                            self.orbit.pitch = (dir.y / dir.length()).asin().clamp(0.1, 1.2);
                            self.orbit.distance = (dir.length() * 1.6).clamp(1.0, 60.0);
                        }
                    }
                }
                self.toast(format!(
                    "{label}: {} objects, {} shots · press Preview shot to play it",
                    o.result["entities"], o.result["shots"]
                ));
            }
            Err(e) => self.toast(format!("{label}: {}", e.message)),
        }
    }

    /// Show a short status message at the bottom of the view.
    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), f64::NAN));
    }

    /// Import the files that were picked or dropped.
    fn import_incoming(&mut self) {
        use shotstage_commands::model::{ModelFormat, encode};
        let items: Vec<_> = self.incoming.borrow_mut().drain(..).collect();
        for (name, bytes, at) in items {
            let (stem, ext) = name
                .rsplit_once('.')
                .map(|(s, e)| (s.to_string(), e.to_ascii_lowercase()))
                .unwrap_or((name.clone(), String::new()));
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp")
                && let Some(cam) = self.reference_for.take()
            {
                let req = json!({"command": "camera.setReference", "params": {"camera": cam, "data": encode(&bytes), "opacity": 0.5}});
                match hub::dispatch(&self.hub, req) {
                    Ok(_) => {
                        self.look_through = true;
                        self.hub.borrow_mut().look_through = true;
                        self.toast(format!("{name} is laid over the camera view: line the shot up with it"));
                    }
                    Err(e) => self.toast(format!("Could not use {name}: {}", e.message)),
                }
                continue;
            }
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp") {
                let key = self.new_key("import");
                let mut params = json!({"data": encode(&bytes), "name": stem});
                if let Some(mode) = self.image_mode.take() {
                    params["mode"] = json!(mode);
                }
                let backdrop = params["mode"] == "backdrop";
                match hub::dispatch(&self.hub, json!({"command": "image.import", "params": params, "merge": key})) {
                    Ok(o) => {
                        let id = o.result["id"].as_str().unwrap_or_default().to_string();
                        if !backdrop && let Some((rect, view)) = self.last_view {
                            let p = at.filter(|p| rect.contains(*p)).unwrap_or(rect.center());
                            self.place_on_ray(&key, &id, ray_at(rect, &view, p), Vec3::ZERO, false);
                        }
                        self.select(Some(id));
                        self.toast(format!("Added {name} ({} × {} px)", o.result["pixels"][0], o.result["pixels"][1]));
                    }
                    Err(e) => self.toast(format!("Could not add {name}: {}", e.message)),
                }
                continue;
            }
            let Some(format) = ModelFormat::from_extension(&ext) else {
                self.toast(format!("{name}: not a model shotstage can import (glb, gltf, obj, stl, fbx, blend)"));
                continue;
            };
            let key = self.new_key("import");
            let params = json!({"format": format.extension(), "data": encode(&bytes), "name": stem});
            match hub::dispatch(&self.hub, json!({"command": "model.import", "params": params, "merge": key})) {
                Ok(o) => {
                    let id = o.result["id"].as_str().unwrap_or_default().to_string();
                    if let Some((rect, view)) = self.last_view {
                        let p = at.filter(|p| rect.contains(*p)).unwrap_or(rect.center());
                        self.place_on_ray(&key, &id, ray_at(rect, &view, p), Vec3::ZERO, false);
                    }
                    self.select(Some(id));
                    let skins = o.result["skins"].as_u64().unwrap_or(0);
                    let mut msg = format!("Imported {name}: {} parts", o.result["entities"]);
                    if skins > 0 {
                        msg.push_str(" with a skeleton (click it, then a part, to pose)");
                    }
                    if let Some(w) = o.result["warnings"].as_array().and_then(|w| w.first()).and_then(Value::as_str) {
                        msg.push_str(&format!(" · {w}"));
                    }
                    self.toast(msg);
                }
                Err(e) => self.toast(format!("Could not import {name}: {}", e.message)),
            }
        }
    }

    fn export_model(&mut self, format: &str) {
        use base64::Engine as _;
        let req = json!({"command": "model.export", "params": {"format": format, "frame": self.frame()}});
        let r = match hub::dispatch(&self.hub, req) {
            Ok(o) => o.result,
            Err(e) => return self.toast(format!("Export failed: {}", e.message)),
        };
        let name = r["fileName"].as_str().unwrap_or("scene.glb").to_string();
        let bytes = match (r["data"].as_str(), r["text"].as_str()) {
            (Some(d), _) => base64::engine::general_purpose::STANDARD.decode(d).unwrap_or_default(),
            (None, Some(t)) => t.as_bytes().to_vec(),
            _ => return self.toast("Export failed: no data"),
        };
        let mime = r["mimeType"].as_str().unwrap_or("application/octet-stream").to_string();
        match crate::files::save(&name, &bytes, &mime) {
            Ok(Some(where_)) => {
                if let Some(mtl) = r["mtl"].as_str() {
                    let mtl_name = name.replace(".obj", ".mtl");
                    let _ = crate::files::save(&mtl_name, mtl.as_bytes(), "text/plain");
                }
                self.toast(format!("Exported {} meshes, {} triangles: {where_}", r["meshes"], r["triangles"]));
            }
            Ok(None) => {}
            Err(e) => self.toast(format!("Export failed: {e}")),
        }
    }

    /// Motion presets for the selection: camera moves for a camera, actions for a person,
    /// motion for anything else. One click writes editable keyframes from the playhead.
    fn animate_section(&mut self, ui: &mut egui::Ui, scene: &Scene, id: &str, frame: f32) {
        use shotstage_commands::motion::{Group, PRESETS};
        let is_cam = matches!(scene.get(id).map(|e| &e.kind), Ok(EntityKind::Camera(_)));
        let person = rig::person_root(scene, id).ok();
        let groups: &[Group] = if is_cam {
            &[Group::Camera]
        } else if person.is_some() {
            &[Group::Character, Group::Object]
        } else {
            &[Group::Object]
        };
        let target = person.clone().unwrap_or_else(|| if is_cam { id.to_string() } else { root_of(scene, id) });
        section(ui, "Animate", true, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.motion_filter)
                    .hint_text("Search moves…")
                    .desired_width(f32::INFINITY),
            );
            egui::Grid::new("motion_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Length");
                ui.add(
                    egui::DragValue::new(&mut self.motion_secs)
                        .range(0.0..=60.0)
                        .speed(0.05)
                        .custom_formatter(|v, _| if v <= 0.0 { "preset".into() } else { format!("{v:.1} s") }),
                )
                .on_hover_text("How long the move takes (preset = its own length)");
                ui.end_row();
                ui.label("Strength");
                ui.add(egui::Slider::new(&mut self.motion_strength, 0.25..=3.0).max_decimals(2));
                ui.end_row();
            });
            let subject = self.last_subject.clone().filter(|s| scene.contains(s) && *s != target);
            if is_cam {
                match &subject {
                    Some(s) => ui.weak(format!(
                        "Moves around / looks at: {}",
                        scene.get(s).map(|e| e.name.as_str()).unwrap_or(s)
                    )),
                    None => ui.weak("Select an object first to move around it (otherwise: what the camera looks at)."),
                };
            }
            let filter = self.motion_filter.trim().to_lowercase();
            ui.horizontal_wrapped(|ui| {
                // a person's own actions first, then general motion
                let ordered = groups.iter().flat_map(|g| PRESETS.iter().filter(move |p| p.group == *g));
                for p in ordered.filter(|p| p.name != "pathFollow") {
                    if !filter.is_empty() && !p.label.to_lowercase().contains(&filter) && !p.description.to_lowercase().contains(&filter) {
                        continue;
                    }
                    if ui.button(p.label).on_hover_text(format!("{} ({} s)", p.description, p.seconds)).clicked() {
                        let mut req = json!({"preset": p.name, "target": target, "start": frame.round(), "intensity": self.motion_strength});
                        if self.motion_secs > 0.0 {
                            req["duration"] = json!(self.motion_secs);
                        }
                        if is_cam && let Some(s) = &subject {
                            req["subject"] = json!(s);
                        }
                        match hub::dispatch(&self.hub, json!({"command": "motion.apply", "params": req})) {
                            Ok(o) => {
                                // play it back right away
                                self.hub.borrow_mut().frame = o.result["start"].as_f64().unwrap_or(0.0) as f32;
                                self.playing = true;
                                let (st, en) = (o.result["start"].as_f64().unwrap_or(0.0), o.result["end"].as_f64().unwrap_or(0.0));
                                self.toast(format!("{}: frames {st:.0}–{en:.0}, {} keys · drag them on the timeline to retime", p.label, o.result["keys"]));
                            }
                            Err(e) => self.toast(format!("{}: {}", p.label, e.message)),
                        }
                    }
                }
            });
        });
    }

    /// Point the orbit view at the selection and fit it (F).
    fn focus_selection(&mut self) {
        let Some(id) = self.selected() else { return };
        let scene = self.scene();
        let f = self.frame();
        let solid = matches!(
            scene.get(&id).map(|e| &e.kind),
            Ok(EntityKind::Mesh { .. } | EntityKind::Empty | EntityKind::Image(_))
        );
        let target = if solid { root_of(&scene, &id) } else { id.clone() };
        let (center, size) = match spatial::bounds(&scene, &target, f) {
            Ok(Some(b)) => (b.center(), b.size().max_element()),
            _ => match world_at(&scene, &id, f) {
                Ok(w) => (w.w_axis.truncate(), 1.0),
                Err(_) => return,
            },
        };
        self.orbit.target = center;
        self.orbit.distance = (size * 2.2).clamp(0.4, 200.0);
        self.look_through = false;
        self.hub.borrow_mut().look_through = false;
    }

    /// Should edits to `id` be keyed? (animated already, or Auto-key is on)
    fn keyed(&self, scene: &Scene, id: &str) -> bool {
        self.auto_key || is_animated(scene, id)
    }

    fn scene(&self) -> Scene {
        self.hub.borrow().session.scene().clone()
    }
    fn frame(&self) -> f32 {
        self.hub.borrow().frame
    }
    fn selected(&self) -> Option<String> {
        self.hub.borrow().selected.clone()
    }
    fn select(&self, id: Option<String>) {
        let mut h = self.hub.borrow_mut();
        h.selected = id;
        // the panels drawn earlier this frame show the old selection: draw again
        if let Some(ctx) = &h.repaint {
            ctx.request_repaint();
        }
    }
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Drop a selection that an agent deleted.
        // WebGL needs polling for buffer maps; keep frames coming while snapshots are pending.
        // (Poll without holding the hub: the map callback borrows it.)
        let polling = {
            let h = self.hub.borrow();
            (h.pending_snapshots > 0).then(|| h.gpu.clone()).flatten()
        };
        if let Some(rs) = polling {
            let _ = rs.device.poll(wgpu::PollType::Poll);
            ui.ctx().request_repaint();
        }
        {
            let mut h = self.hub.borrow_mut();
            if h.selected.as_ref().is_some_and(|s| !h.session.scene().contains(s)) {
                h.selected = None;
            }
        }
        // palette thumbnails, a few per frame until they are all done
        if let Some(rs) = frame.wgpu_render_state() {
            let thumbs = self.thumbs.get_or_insert_with(|| crate::thumbs::Thumbs::new(&rs.device, thumb_items()));
            if thumbs.step(rs, 4) {
                ui.ctx().request_repaint();
            }
        }
        crate::files::take_dropped(ui.ctx(), &self.incoming);
        let saved = self.saved.borrow_mut().take();
        match saved {
            Some(Ok(w)) => self.toast(format!("Saved the frame: {w}")),
            Some(Err(e)) => self.toast(format!("Could not save the frame: {e}")),
            None => {}
        }
        if !self.incoming.borrow().is_empty() {
            self.import_incoming();
        }
        if let Some(sel) = self.selected() {
            let scene = self.scene();
            if matches!(scene.get(&sel).map(|e| &e.kind), Ok(EntityKind::Mesh { .. } | EntityKind::Empty)) {
                self.last_subject = Some(root_of(&scene, &sel));
            }
        }
        if self.playing {
            let dt = ui.input(|i| i.stable_dt).min(0.1);
            let scene = self.scene();
            let t = &scene.timeline;
            let mut h = self.hub.borrow_mut();
            h.frame += dt * t.fps;
            if h.frame > t.end as f32 {
                h.frame = t.start as f32;
            }
            ui.ctx().request_repaint();
        }
        self.shortcuts(ui);

        {
            let h = self.hub.borrow();
            (self.pass, self.look_through) = (h.pass, h.look_through);
        }
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("timeline").resizable(false).show(ui, |ui| self.bottom(ui));
        egui::Panel::left("outliner")
            .resizable(true)
            .default_size(252.0)
            .min_size(220.0)
            .max_size(420.0)
            .show(ui, |ui| self.outliner(ui));
        egui::Panel::right("properties").resizable(true).default_size(300.0).min_size(260.0).max_size(460.0).show(
            ui,
            |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.properties(ui));
            },
        );
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.viewport(ui, frame));
        {
            let mut h = self.hub.borrow_mut();
            (h.pass, h.look_through) = (self.pass, self.look_through);
        }
    }
}

impl EditorApp {
    fn shortcuts(&mut self, ui: &egui::Ui) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let (undo, redo, del, space, turn, dup) = ui.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && !i.modifiers.shift && i.key_pressed(egui::Key::Z),
                cmd && (i.key_pressed(egui::Key::Y) || (i.modifiers.shift && i.key_pressed(egui::Key::Z))),
                i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::X) || i.key_pressed(egui::Key::Backspace),
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::R).then_some(if i.modifiers.shift { -15.0 } else { 15.0 }),
                cmd && i.key_pressed(egui::Key::D),
            )
        });
        if let (Some(deg), Some(id)) = (turn, self.selected()) {
            // while dragging, the turn joins the drag's undo step
            let f = self.frame();
            match &self.drag {
                Gesture::Move { key, .. } => {
                    let key = key.clone();
                    self.run_key(&key, "entity.rotate", json!({"id": id, "degrees": deg, "frame": f}));
                }
                _ => {
                    self.run("entity.rotate", json!({"id": id, "degrees": deg, "frame": f}));
                }
            }
        }
        if dup
            && let Some(id) = self.selected()
            && let Some(r) = self.run("entity.duplicate", json!({"id": id}))
        {
            self.select(r["id"].as_str().map(str::to_string));
        }
        if undo {
            self.run("history.undo", json!({}));
        }
        if redo {
            self.run("history.redo", json!({}));
        }
        if del && let Some(id) = self.selected() {
            self.run("entity.delete", json!({ "id": id }));
        }
        if space {
            self.playing = !self.playing;
        }
        if ui.input(|i| i.key_pressed(egui::Key::F)) {
            self.focus_selection();
        }
        let (key, home, end, step) = ui.input(|i| {
            let step = if i.key_pressed(egui::Key::ArrowLeft) {
                -1.0
            } else if i.key_pressed(egui::Key::ArrowRight) {
                1.0
            } else {
                0.0
            };
            (
                i.key_pressed(egui::Key::K),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
                step * if i.modifiers.shift { 10.0 } else { 1.0 },
            )
        });
        let t = self.scene().timeline;
        if key {
            self.key_selection();
        }
        if home {
            self.hub.borrow_mut().frame = t.start as f32;
        }
        if end {
            self.hub.borrow_mut().frame = t.end as f32;
        }
        if step != 0.0 {
            self.playing = false;
            let mut h = self.hub.borrow_mut();
            h.frame = (h.frame.round() + step).clamp(t.start as f32, t.end as f32);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("shotstage").strong().color(ACCENT));
            ui.separator();
            if ui.button("New").on_hover_text("Clear the scene (undoable)").clicked() {
                self.run("scene.new", json!({}));
            }
            if ui.button("Undo").on_hover_text("Cmd/Ctrl+Z").clicked() {
                self.run("history.undo", json!({}));
            }
            if ui.button("Redo").on_hover_text("Cmd/Ctrl+Shift+Z").clicked() {
                self.run("history.redo", json!({}));
            }
            ui.menu_button("History", |ui| {
                let info = self.run("history.info", json!({})).unwrap_or(Value::Null);
                let undo: Vec<String> = info["undo"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                if undo.is_empty() {
                    ui.weak("Nothing to undo yet");
                } else {
                    ui.weak("Click a step to go back to before it");
                }
                for (i, label) in undo.iter().take(20).enumerate() {
                    if ui.button(format!("{}. {}", i + 1, friendly(label))).clicked() {
                        for _ in 0..=i {
                            self.run("history.undo", json!({}));
                        }
                        ui.close();
                    }
                }
            });
            ui.separator();
            if ui
                .button("Import…")
                .on_hover_text("glTF / GLB, OBJ, STL, FBX and .blend (FBX / .blend on the desktop, through Blender). You can also drop files on the view.")
                .clicked()
            {
                crate::files::pick(&self.incoming, ui.ctx());
            }
            ui.menu_button("Templates", |ui| {
                ui.weak("Start from a ready scene (replaces this one; Undo brings it back)");
                for (name, label, desc) in shotstage_commands::templates::TEMPLATES {
                    if ui.button(*label).on_hover_text(*desc).clicked() {
                        self.apply_template(name, label);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Export", |ui| {
                ui.weak("The scene posed at the current frame");
                for (label, format, tip) in [
                    ("GLB (glTF binary)", "glb", "Meshes, colours, cameras and lights: Blender, Unreal, Unity, three.js"),
                    ("glTF (text)", "gltf", "The same as GLB, as one JSON file"),
                    ("OBJ + MTL", "obj", "Meshes and colours, for almost any 3D tool"),
                    ("STL", "stl", "Geometry only, for 3D printing and CAD"),
                    ("FBX (via Blender)", "fbx", "Needs Blender on this computer (desktop editor)"),
                ] {
                    if ui.button(label).on_hover_text(tip).clicked() {
                        self.export_model(format);
                        ui.close();
                    }
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak("3D previs for AI images and video");
            });
        });
    }

    fn outliner(&mut self, ui: &mut egui::Ui) {
        // The scene list has its own resizable strip, so a long palette never pushes it away.
        egui::Panel::bottom("scene_tree")
            .resizable(true)
            .default_size(230.0)
            .min_size(110.0)
            .show(ui, |ui| self.scene_tree(ui));
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.blocks(ui));
    }

    fn blocks(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.strong("Blocks");
            ui.add(
                egui::TextEdit::singleline(&mut self.block_filter)
                    .hint_text("Search blocks…")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.weak("Drag into the scene, or click to drop it in the middle.");
        ui.add_space(2.0);
        let filter = self.block_filter.trim().to_lowercase();
        egui::ScrollArea::vertical().id_salt("blocks").auto_shrink([false, false]).show(ui, |ui| {
            let gap = ui.spacing().item_spacing.x;
            // tiles fill the width inside a section (sections are indented)
            let tile_for = |w: f32| {
                let cols = ((w + gap) / (70.0 + gap)).floor().max(2.0);
                ((w + gap) / cols - gap).floor() - 0.5
            };
            let tile = tile_for(ui.available_width() - ui.spacing().indent);
            for (title, open, blocks) in palette() {
                // searching a section's name shows all of it
                let whole = !filter.is_empty() && title.to_lowercase().contains(&filter);
                let shown: Vec<Block> = blocks.into_iter().filter(|b| whole || b.matches(&filter)).collect();
                if shown.is_empty() {
                    continue;
                }
                let mut header =
                    egui::CollapsingHeader::new(egui::RichText::new(title).strong()).id_salt(title).default_open(open);
                if !filter.is_empty() {
                    header = header.open(Some(true));
                }
                header.show(ui, |ui| {
                    let tile = tile_for(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        for b in &shown {
                            self.block_tile(ui, b, tile);
                        }
                    });
                });
            }
            egui::CollapsingHeader::new(egui::RichText::new("Images").strong())
                .id_salt("images")
                .default_open(false)
                .show(ui, |ui| {
                    ui.weak("Pick a picture, or drop one on the view (a transparent PNG becomes a cut-out).");
                    ui.horizontal_wrapped(|ui| {
                        for (label, mode, tip) in [
                            ("Card", "card", "A standing picture: reference photo, poster, sign"),
                            ("Cut-out", "billboard", "Always faces the camera: cut-out people, trees"),
                            ("Backdrop", "backdrop", "Fills the camera's background, behind everything"),
                            ("Floor", "floor", "Lies flat: rug, floor plan, logo on the ground"),
                        ] {
                            if ui.add_sized([tile, 30.0], egui::Button::new(label)).on_hover_text(tip).clicked() {
                                self.image_mode = Some(mode);
                                crate::files::pick(&self.incoming, ui.ctx());
                            }
                        }
                    });
                });
            egui::CollapsingHeader::new(egui::RichText::new("Cameras & helpers").strong())
                .id_salt("helpers")
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_sized([tile, 30.0], egui::Button::new("Camera"))
                            .on_hover_text("A camera where you are looking from")
                            .clicked()
                        {
                            let eye = self.orbit.eye();
                            if let Some(r) =
                                self.run("entity.create", json!({"type": "camera", "position": eye.to_array()}))
                            {
                                let id = r["id"].as_str().unwrap_or_default().to_string();
                                self.run("camera.lookAt", json!({"id": id, "target": self.orbit.target.to_array()}));
                                self.select(Some(id));
                            }
                        }
                        if ui
                            .add_sized([tile, 30.0], egui::Button::new("Empty"))
                            .on_hover_text("A group / pivot without geometry")
                            .clicked()
                            && let Some(r) = self.run("entity.create", json!({"type": "empty"}))
                        {
                            self.select(r["id"].as_str().map(str::to_string));
                        }
                    });
                });
        });
    }

    fn block_tile(&mut self, ui: &mut egui::Ui, b: &Block, width: f32) {
        let thumb = b.thumb_key().and_then(|k| self.thumbs.as_ref()?.get(&k));
        let r = match thumb {
            // a picture with the label under it
            Some((tex, uv)) => {
                let height = (width - 8.0) * 0.75 + 24.0;
                let (rect, r) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
                let v = ui.style().interact(&r);
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 5.0, v.weak_bg_fill);
                p.rect_stroke(rect, 5.0, v.bg_stroke, egui::StrokeKind::Inside);
                let img =
                    Rect::from_min_size(rect.min + Vec2::new(4.0, 4.0), Vec2::new(width - 8.0, (width - 8.0) * 0.75));
                p.image(tex, img, uv, Color32::WHITE);
                p.text(
                    egui::pos2(rect.center().x, rect.bottom() - 11.0),
                    egui::Align2::CENTER_CENTER,
                    b.label(),
                    egui::FontId::proportional(11.5),
                    v.text_color(),
                );
                r
            }
            None => ui.add_sized(
                [width, 30.0],
                egui::Button::new(egui::RichText::new(b.label()).size(12.5)).sense(Sense::click_and_drag()),
            ),
        };
        let r = r.on_hover_cursor(CursorIcon::Grab).on_hover_text(b.description());
        if let Block::LightPreset(preset, ..) = b {
            if r.clicked() {
                let mut p = json!({"preset": preset});
                if let Some(s) = self.last_subject.clone().filter(|s| self.scene().contains(s)) {
                    p["subject"] = json!(s);
                }
                if self.run("light.preset", p).is_some() {
                    self.toast(format!("Lighting: {}", b.label()));
                }
            }
            return;
        }
        if r.drag_started() {
            let key = self.new_key("new");
            self.palette = Some(PaletteDrag { block: b.clone(), spawned: None, key });
        }
        if r.clicked() {
            let key = self.new_key("new");
            if let Some(id) = self.spawn(b, &key) {
                if let Some((rect, view)) = self.last_view {
                    self.place_on_ray(&key, &id, ray_at(rect, &view, rect.center()), Vec3::ZERO, false);
                } else {
                    self.run_key(&key, "entity.place", json!({"id": id}));
                }
                self.select(Some(id));
            }
        }
    }

    fn scene_tree(&mut self, ui: &mut egui::Ui) {
        let scene = self.scene();
        let selected = self.selected();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong("Scene");
            ui.weak(format!("{} objects", scene.entities.iter().filter(|e| e.parent.is_none()).count()));
        });
        egui::ScrollArea::vertical().id_salt("scene_tree").auto_shrink([false, false]).show(ui, |ui| {
            fn row(ui: &mut egui::Ui, scene: &Scene, e: &shotstage_core::Entity, selected: &Option<String>) -> bool {
                let icon = match e.kind {
                    EntityKind::Mesh { .. } => "■",
                    EntityKind::Camera(_) => "◉",
                    EntityKind::Light(_) => "☀",
                    EntityKind::Image(_) => "▨",
                    EntityKind::Empty if rig::is_joint(scene, &e.id) => "•",
                    EntityKind::Empty => "▣",
                };
                let active = if scene.active_camera.as_deref() == Some(&e.id) { " ★" } else { "" };
                let text = format!("{icon} {}{active}", e.name);
                let text = if e.visible { egui::RichText::new(text) } else { egui::RichText::new(text).weak() };
                ui.selectable_label(selected.as_deref() == Some(&e.id), text).clicked()
            }
            fn tree(
                ui: &mut egui::Ui,
                scene: &Scene,
                parent: Option<&str>,
                selected: &Option<String>,
                picked: &mut Option<Option<String>>,
            ) {
                for e in scene.children(parent) {
                    if scene.children(Some(&e.id)).next().is_none() {
                        if row(ui, scene, e, selected) {
                            *picked = Some(Some(e.id.clone()));
                        }
                        continue;
                    }
                    // groups start folded; open the ones holding the selection
                    let id = ui.make_persistent_id(("tree", &e.id));
                    let mut state =
                        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                    if selected.as_ref().is_some_and(|s| s != &e.id && spatial::subtree(scene, &e.id).contains(s)) {
                        state.set_open(true);
                    }
                    state
                        .show_header(ui, |ui| {
                            if row(ui, scene, e, selected) {
                                *picked = Some(Some(e.id.clone()));
                            }
                        })
                        .body(|ui| tree(ui, scene, Some(&e.id), selected, picked));
                }
            }
            let mut picked = None;
            tree(ui, &scene, None, &selected, &mut picked);
            if let Some(p) = picked {
                self.select(p);
            }
            if scene.entities.is_empty() {
                ui.weak("Empty scene. Add blocks above, or drive it from the console / API.");
            }
        });
    }

    fn properties(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let Some(id) = self.selected() else {
            ui.weak("Nothing selected.");
            ui.add_space(6.0);
            ui.label("• Drag a block from the left into the scene.");
            ui.label("• Click an object to select it; drag it to slide it over surfaces.");
            ui.label("• Click a person, then a body part, to pose it.");
            ui.label("• Pinch or scroll to zoom, drag empty space to orbit.");
            return;
        };
        let scene = self.scene();
        let Ok(e) = scene.get(&id) else { return };
        let frame = self.frame();
        let joint = rig::joint_name(&scene, &id);
        let person = rig::person_root(&scene, &id).ok().filter(|r| *r == id);
        let kind = match (&e.kind, joint, &person) {
            (_, Some(_), _) => "joint",
            (_, _, Some(_)) => "person",
            (EntityKind::Empty, ..) if scene.children(Some(&id)).next().is_some() => "group",
            (k, ..) => k.type_name(),
        };
        ui.heading(&e.name);
        let dims = matches!(e.kind, EntityKind::Mesh { .. } | EntityKind::Empty | EntityKind::Image(_))
            .then(|| spatial::bounds(&scene, &id, frame).ok().flatten())
            .flatten()
            .map(|b| {
                let s = b.size();
                format!(" · {:.2} × {:.2} × {:.2} m", s.x, s.y, s.z)
            })
            .unwrap_or_default();
        ui.weak(format!("{kind} · {}{dims}", e.id));
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            let mut visible = e.visible;
            if ui.checkbox(&mut visible, "Visible").changed() {
                self.run("entity.setVisible", json!({"id": id, "visible": visible}));
            }
            if joint.is_none() {
                let mut locked = e.locked;
                if ui.checkbox(&mut locked, "Locked").on_hover_text("Drags in the viewport pass through it").changed() {
                    self.run("entity.setLocked", json!({"id": id, "locked": locked}));
                }
                if ui.button("Duplicate").on_hover_text("Ctrl/Cmd+D").clicked()
                    && let Some(r) = self.run("entity.duplicate", json!({"id": id}))
                {
                    self.select(r["id"].as_str().map(str::to_string));
                }
                if ui.button("Delete").on_hover_text("Delete / Backspace").clicked() {
                    self.run("entity.delete", json!({"id": id}));
                }
            }
        });
        ui.add_space(4.0);

        if let Some(j) = joint {
            self.joint_section(ui, &scene, &id, j, frame);
            return;
        }
        if rig::is_joint(&scene, &id) {
            self.bone_section(ui, &scene, &id, frame);
            return;
        }
        if let Some(root) = &person {
            self.person_section(ui, &scene, root, frame);
        }

        if !matches!(e.kind, EntityKind::Camera(_) | EntityKind::Light(_)) {
            section(ui, "Place", true, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Drop to surface").on_hover_text("entity.place").clicked() {
                        self.run("entity.place", json!({"id": id, "frame": frame}));
                    }
                    if ui.button("↺ 45°").on_hover_text("R").clicked() {
                        self.run("entity.rotate", json!({"id": id, "degrees": 45, "frame": frame}));
                    }
                    if ui.button("↻ 45°").on_hover_text("Shift+R").clicked() {
                        self.run("entity.rotate", json!({"id": id, "degrees": -45, "frame": frame}));
                    }
                    if let Some(cam) = camera_id_at(&scene, frame)
                        && ui.button("Face camera").on_hover_text("entity.face").clicked()
                    {
                        self.run("entity.face", json!({"id": id, "target": cam, "frame": frame}));
                    }
                });
            });
            section(ui, "Frame it", true, |ui| {
                ui.weak("Moves the active camera (or adds one) and looks through it.");
                egui::Grid::new("frame_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label("Angle");
                    egui::ComboBox::from_id_salt("frame_angle").width(130.0).selected_text(self.frame_angle).show_ui(
                        ui,
                        |ui| {
                            for a in ["eyeLevel", "high", "low", "birdsEye", "wormsEye"] {
                                ui.selectable_value(&mut self.frame_angle, a, a);
                            }
                        },
                    );
                    ui.end_row();
                    ui.label("Side");
                    egui::ComboBox::from_id_salt("frame_side").width(130.0).selected_text(self.frame_side).show_ui(
                        ui,
                        |ui| {
                            for a in
                                ["front", "frontLeft", "left", "backLeft", "back", "backRight", "right", "frontRight"]
                            {
                                ui.selectable_value(&mut self.frame_side, a, a);
                            }
                        },
                    );
                    ui.end_row();
                });
                ui.horizontal_wrapped(|ui| {
                    for (label, size) in [
                        ("ECU", "extremeCloseUp"),
                        ("CU", "closeUp"),
                        ("MCU", "mediumCloseUp"),
                        ("MS", "medium"),
                        ("MFS", "mediumFull"),
                        ("FS", "full"),
                        ("WS", "wide"),
                        ("EWS", "extremeWide"),
                    ] {
                        if ui.button(label).on_hover_text(size).clicked() {
                            let mut p = json!({"subject": id, "size": size, "angle": self.frame_angle, "side": self.frame_side, "frame": frame});
                            if let Some(cam) = camera_id_at(&scene, frame) {
                                p["camera"] = json!(cam);
                            }
                            if self.run("camera.frame", p).is_some() {
                                self.look_through = true;
                                self.hub.borrow_mut().look_through = true;
                            }
                        }
                    }
                });
            });
        }

        self.animate_section(ui, &scene, &id, frame);
        let plain = person.is_none() && !matches!(e.kind, EntityKind::Empty);
        section(ui, "Transform", plain, |ui| {
            let animated = self.keyed(&scene, &id);
            let t = local_at(&scene, &id, frame).unwrap_or(e.transform);
            let (mut pos, mut rot, mut scale) = (t.translation, t.euler_degrees(), t.scale);
            let (mut changed, mut started) = (false, false);
            egui::Grid::new("transform_grid").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
                for (label, v, speed) in
                    [("Position", &mut pos, 0.02), ("Rotation", &mut rot, 0.5), ("Scale", &mut scale, 0.01)]
                {
                    ui.label(label);
                    for x in v.iter_mut() {
                        let r = ui.add_sized([56.0, 20.0], egui::DragValue::new(x).speed(speed).max_decimals(3));
                        changed |= r.changed();
                        started |= r.drag_started() || r.gained_focus();
                    }
                    ui.end_row();
                }
            });
            if started {
                self.gesture += 1;
            }
            if changed {
                let mut p = json!({"id": id, "position": pos, "rotationEuler": rot, "scale": scale});
                if animated {
                    p["keyframeAt"] = json!(frame); // auto-key: edits on an animated entity become keys
                }
                self.run_merged(&format!("transform:{id}"), "entity.setTransform", p);
            }
            ui.horizontal(|ui| {
                if ui.button(format!("Key @ {frame:.0}")).clicked() {
                    self.run("entity.setTransform", json!({"id": id, "position": pos, "rotationEuler": rot, "scale": scale, "keyframeAt": frame.round()}));
                }
                if animated && ui.button("Delete key").clicked() {
                    self.run("keyframe.delete", json!({"id": id, "property": "transform", "frame": frame.round()}));
                }
            });
            if animated {
                ui.weak("Animated: edits are keyed at the current frame.");
            }
        });

        match &e.kind {
            EntityKind::Mesh { modifiers, color, .. } => {
                section(ui, "Mesh", true, |ui| {
                    let mut c = *color;
                    ui.horizontal(|ui| {
                        ui.label("Colour");
                        let r = ui.color_edit_button_rgb(&mut c);
                        if r.clicked() {
                            self.gesture += 1;
                        }
                        if r.changed() && c != *color {
                            self.run_merged(&format!("color:{id}"), "mesh.setColor", json!({"id": id, "color": c}));
                        }
                    });
                    if let Some(stats) = self.run("mesh.stats", json!({"id": id})) {
                        ui.weak(format!(
                            "{} verts · {} faces · {} tris",
                            stats["vertices"], stats["faces"], stats["triangles"]
                        ));
                    }
                    for (i, m) in modifiers.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let label = match m {
                                Modifier::Mirror { axis } => {
                                    format!("Mirror {}", ["X", "Y", "Z"][(*axis).min(2) as usize])
                                }
                                Modifier::Array { count, offset } => {
                                    format!("Array ×{count} [{:.2}, {:.2}, {:.2}]", offset[0], offset[1], offset[2])
                                }
                                Modifier::Subdivide { levels, smooth } => {
                                    format!("Subdivide {levels}{}", if *smooth { " smooth" } else { "" })
                                }
                            };
                            ui.label(label);
                            if ui.small_button("✕").clicked() {
                                self.run("mesh.removeModifier", json!({"id": id, "index": i}));
                            }
                        });
                    }
                    ui.horizontal_wrapped(|ui| {
                        for (label, m) in [
                            ("+ Mirror X", json!({"type": "mirror", "axis": 0})),
                            ("+ Array", json!({"type": "array", "count": 3, "offset": [1.2, 0, 0]})),
                            ("+ Subdivide", json!({"type": "subdivide", "levels": 1, "smooth": true})),
                        ] {
                            if ui.button(label).clicked() {
                                self.run("mesh.addModifier", json!({"id": id, "modifier": m}));
                            }
                        }
                        if !modifiers.is_empty() && ui.button("Apply all").clicked() {
                            self.run("mesh.applyModifiers", json!({"id": id}));
                        }
                        if ui.button("Origin to centre").clicked() {
                            self.run("mesh.setOriginToCenter", json!({"id": id}));
                        }
                    });
                });
            }
            EntityKind::Camera(_) => {
                section(ui, "Camera", true, |ui| {
                    let lens = camera_data_at(&scene, &id, frame).unwrap_or_default();
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Frame");
                        for (label, a, tip) in [
                            ("16:9", 16.0 / 9.0, "Landscape video"),
                            ("9:16", 9.0 / 16.0, "Vertical video: Reels, TikTok, Shorts"),
                            ("1:1", 1.0, "Square"),
                            ("4:5", 0.8, "Portrait feed post"),
                            ("4:3", 4.0 / 3.0, "Classic"),
                            ("2.39:1", 2.39, "Cinemascope"),
                        ] {
                            let on = (lens.aspect - a).abs() < 0.01;
                            if ui.selectable_label(on, label).on_hover_text(tip).clicked() {
                                self.run("camera.setLens", json!({"id": id, "aspect": a}));
                            }
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button("Match current view")
                            .on_hover_text("Put this camera where you are looking from in the view")
                            .clicked()
                        {
                            let eye = self.orbit.eye();
                            let mut p = json!({"id": id, "position": eye.to_array(), "frame": frame});
                            if self.keyed(&scene, &id) {
                                p["keyframeAt"] = json!(frame.round());
                            }
                            self.run("entity.moveTo", p);
                            self.run("camera.lookAt", json!({"id": id, "target": self.orbit.target.to_array()}));
                            if self.keyed(&scene, &id) {
                                self.run(
                                    "keyframe.set",
                                    json!({"id": id, "property": "transform", "frame": frame.round()}),
                                );
                            }
                        }
                        ui.checkbox(&mut self.safe_areas, "Safe areas")
                            .on_hover_text("Action / title safe frames in the camera view");
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Reference");
                        if ui
                            .button("Load picture…")
                            .on_hover_text(
                                "Lay a storyboard frame or a reference photo over this camera's view to match it",
                            )
                            .clicked()
                        {
                            self.reference_for = Some(id.clone());
                            crate::files::pick(&self.incoming, ui.ctx());
                        }
                        if let Some(r) = scene.references.get(&id) {
                            let mut o = r.opacity;
                            let resp = ui.add(egui::Slider::new(&mut o, 0.0..=1.0).show_value(false));
                            if resp.drag_started() {
                                self.gesture += 1;
                            }
                            if resp.changed() {
                                self.run_merged(
                                    &format!("ref:{id}"),
                                    "camera.setReference",
                                    json!({"camera": id, "opacity": o}),
                                );
                            }
                            if ui.small_button("✕").on_hover_text("Remove the reference").clicked() {
                                self.run("camera.setReference", json!({"camera": id, "clear": true}));
                            }
                        }
                    });
                    let mut focal = lens.focal_length_mm;
                    let focal_animated = scene.timeline.track(&id, TrackProperty::FocalLength).is_some();
                    ui.horizontal(|ui| {
                        ui.label("Focal length");
                        let r = ui.add(egui::DragValue::new(&mut focal).range(4.0..=600.0).suffix(" mm").speed(0.3));
                        if r.drag_started() {
                            self.gesture += 1;
                        }
                        if r.changed() {
                            self.run_merged(
                                &format!("lens:{id}"),
                                "camera.setLens",
                                json!({"id": id, "focalLengthMm": focal}),
                            );
                            if focal_animated {
                                self.run(
                                    "keyframe.set",
                                    json!({"id": id, "property": "focalLength", "frame": frame.round()}),
                                );
                            }
                        }
                        if ui.button("Key").clicked() {
                            self.run(
                                "keyframe.set",
                                json!({"id": id, "property": "focalLength", "frame": frame.round()}),
                            );
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        for mm in [24.0, 35.0, 50.0, 85.0, 135.0] {
                            if ui.small_button(format!("{mm:.0}")).clicked() {
                                self.run("camera.setLens", json!({"id": id, "focalLengthMm": mm}));
                            }
                        }
                    });
                    ui.weak(format!("{:.1}° horizontal FOV, aspect {:.3}", lens.hfov().to_degrees(), lens.aspect));
                    ui.horizontal(|ui| {
                        let active = scene.active_camera.as_deref() == Some(id.as_str());
                        if ui.add_enabled(!active, egui::Button::new("Make active")).clicked() {
                            self.run("camera.setActive", json!({"id": id}));
                        }
                        if ui.button("Look at origin").clicked() {
                            self.run("camera.lookAt", json!({"id": id, "target": [0, 0.8, 0]}));
                        }
                    });
                    if let Some(d) = self.run("camera.describe", json!({"id": id, "frame": frame})) {
                        ui.weak(d["summary"].as_str().unwrap_or_default().to_string());
                    }
                });
            }
            EntityKind::Light(l) => self.light_section(ui, &id, l),
            EntityKind::Image(img) => {
                let img = img.clone();
                section(ui, "Image", true, |ui| {
                    let mut billboard = img.billboard;
                    if ui
                        .checkbox(&mut billboard, "Always face the camera (billboard)")
                        .on_hover_text("For cut-out people, trees and signs")
                        .changed()
                    {
                        self.run("image.set", json!({"id": id, "billboard": billboard}));
                    }
                    let mut h = img.size[1];
                    ui.horizontal(|ui| {
                        ui.label("Height");
                        let r = ui.add(egui::DragValue::new(&mut h).range(0.01..=500.0).speed(0.01).suffix(" m"));
                        if r.drag_started() {
                            self.gesture += 1;
                        }
                        if r.changed() {
                            self.run_merged(&format!("img:{id}"), "image.set", json!({"id": id, "height": h}));
                        }
                    });
                    if let Some(a) = scene.assets.get(&img.asset) {
                        ui.weak(format!("{} × {} px · {:.2} × {:.2} m", a.width, a.height, img.size[0], img.size[1]));
                    }
                });
            }
            EntityKind::Empty => {}
        }
    }

    /// A bone of an imported skeleton: plain rotation sliders.
    fn bone_section(&mut self, ui: &mut egui::Ui, scene: &Scene, id: &str, frame: f32) {
        let animated = self.keyed(scene, id);
        section(ui, "Bone", true, |ui| {
            ui.weak("Drag the model's surface in the view to bend this bone, or turn the coloured rings.");
            let mut r = local_at(scene, id, frame).map(|t| t.euler_degrees()).unwrap_or_default();
            let (mut changed, mut started) = (false, false);
            egui::Grid::new("bone_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                for (i, label) in ["X", "Y", "Z"].iter().enumerate() {
                    ui.label(*label);
                    let resp = ui.add(egui::Slider::new(&mut r[i], -180.0..=180.0).suffix("°").max_decimals(0));
                    changed |= resp.changed();
                    started |= resp.drag_started();
                    ui.end_row();
                }
            });
            if started {
                self.gesture += 1;
            }
            if changed {
                let mut p = json!({"id": id, "rotationEuler": r});
                if animated {
                    p["keyframeAt"] = json!(frame.round());
                }
                self.run_merged(&format!("bone:{id}"), "entity.setTransform", p);
            }
            if ui.button("Select model").clicked() {
                self.select(Some(root_of(scene, id)));
            }
        });
    }

    fn light_section(&mut self, ui: &mut egui::Ui, id: &str, l: &shotstage_core::LightData) {
        use shotstage_core::LightKind;
        section(ui, "Light", true, |ui| {
            let mut n = *l;
            let mut started = false;
            egui::Grid::new("light_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Kind");
                ui.horizontal(|ui| {
                    for (k, label) in [
                        (LightKind::Sun, "Sun"),
                        (LightKind::Point, "Point"),
                        (LightKind::Spot, "Spot"),
                        (LightKind::Area, "Area"),
                    ] {
                        ui.selectable_value(&mut n.kind, k, label);
                    }
                });
                ui.end_row();
                ui.label("Colour");
                ui.horizontal(|ui| {
                    started |= ui.color_edit_button_rgb(&mut n.color).clicked();
                    for (label, c) in
                        [("Warm", [1.0, 0.8, 0.6]), ("Neutral", [1.0, 1.0, 1.0]), ("Cool", [0.75, 0.85, 1.0])]
                    {
                        if ui.small_button(label).clicked() {
                            n.color = c;
                        }
                    }
                });
                ui.end_row();
                ui.label("Intensity");
                let r = ui.add(egui::Slider::new(&mut n.intensity, 0.0..=20.0).logarithmic(true).max_decimals(2));
                started |= r.drag_started();
                ui.end_row();
                if n.kind == LightKind::Spot {
                    ui.label("Cone");
                    started |= ui.add(egui::Slider::new(&mut n.cone_degrees, 5.0..=170.0).suffix("°")).drag_started();
                    ui.end_row();
                }
                if matches!(n.kind, LightKind::Spot | LightKind::Area) {
                    ui.label("Softness");
                    started |= ui.add(egui::Slider::new(&mut n.softness, 0.0..=1.0)).drag_started();
                    ui.end_row();
                }
                if n.kind == LightKind::Area {
                    ui.label("Size");
                    ui.horizontal(|ui| {
                        for v in n.size.iter_mut() {
                            started |= ui
                                .add(egui::DragValue::new(v).range(0.05..=20.0).speed(0.01).suffix(" m"))
                                .drag_started();
                        }
                    });
                    ui.end_row();
                }
            });
            if started {
                self.gesture += 1;
            }
            if n != *l {
                self.run_merged(
                    &format!("light:{id}"),
                    "light.set",
                    json!({"id": id, "kind": n.kind, "color": n.color, "intensity": n.intensity,
                        "coneDegrees": n.cone_degrees, "softness": n.softness, "size": n.size}),
                );
            }
            ui.horizontal_wrapped(|ui| {
                if let Some(target) = self.light_target()
                    && ui.button("Aim at selection's group").on_hover_text("camera.lookAt").clicked()
                {
                    self.run("camera.lookAt", json!({"id": id, "target": target}));
                }
                if ui.button("Aim at origin").clicked() {
                    self.run("camera.lookAt", json!({"id": id, "target": [0, 0.8, 0]}));
                }
            });
            ui.weak("A light shines along its -Z. Scenes with lights are lit by them; toggle Studio light in the view toolbar to compare.");
        });
    }

    /// What a light should aim at: the subject last framed, else nothing.
    fn light_target(&self) -> Option<Value> {
        self.last_subject.as_ref().filter(|s| self.scene().contains(s)).map(|s| json!(s))
    }

    fn person_section(&mut self, ui: &mut egui::Ui, scene: &Scene, root: &str, frame: f32) {
        let animated = self.auto_key || rig::JOINTS.iter().any(|j| is_animated(scene, &format!("{root}_{j}")));
        section(ui, "Pose", true, |ui| {
            ui.weak("Click a body part to select its joint, then drag it (or the rings) to bend it.");
            ui.horizontal_wrapped(|ui| {
                for p in rig::POSES {
                    if ui.button(p.name).on_hover_text(p.description).clicked() {
                        let mut req = json!({"id": root, "pose": p.name, "frame": frame});
                        if animated {
                            req["keyframeAt"] = json!(frame.round());
                        }
                        self.run("pose.set", req);
                    }
                }
            });
        });
        section(ui, "Hair", true, |ui| {
            ui.horizontal_wrapped(|ui| {
                for style in rig::HAIR_STYLES {
                    if ui.button(*style).clicked() {
                        self.run("person.setHair", json!({"id": root, "style": style}));
                    }
                }
            });
            let current = scene.get(&format!("{root}_hair")).ok().and_then(|e| match &e.kind {
                EntityKind::Mesh { color, .. } => Some(*color),
                _ => None,
            });
            if let Some(c) = current {
                let mut c2 = c;
                ui.horizontal(|ui| {
                    ui.label("Colour");
                    if ui.color_edit_button_rgb(&mut c2).changed() && c2 != c {
                        let style = if scene.contains(&format!("{root}_hair_back")) {
                            self.hair_style_of(scene, root)
                        } else {
                            "short"
                        };
                        self.run_merged(
                            &format!("hair:{root}"),
                            "person.setHair",
                            json!({"id": root, "style": style, "color": c2}),
                        );
                    }
                });
            }
        });
    }

    /// Best guess of the current hairstyle from the parts under the head.
    fn hair_style_of(&self, scene: &Scene, root: &str) -> &'static str {
        let Ok(back) = scene.get(&format!("{root}_hair_back")) else { return "short" };
        let s = back.transform.scale;
        match &back.kind {
            EntityKind::Mesh {
                source: shotstage_core::MeshSource::Primitive { primitive: shotstage_core::Primitive::Capsule { .. } },
                ..
            } => "ponytail",
            _ if s[1] > 0.3 => "long",
            _ if s[0] > 0.2 => "bob",
            _ => "bun",
        }
    }

    fn joint_section(&mut self, ui: &mut egui::Ui, scene: &Scene, id: &str, joint: &str, frame: f32) {
        let Ok(root) = rig::person_root(scene, id) else { return };
        let legs =
            joint.starts_with("hip") || joint.starts_with("knee") || joint.starts_with("ankle") || joint == "pelvis";
        let animated = self.keyed(scene, id);
        let set = |this: &mut Self, values: Value, merge: Option<&str>| {
            let mut req = json!({"id": root, "joints": values, "frame": frame, "drop": legs});
            if animated {
                req["keyframeAt"] = json!(frame.round());
            }
            match merge {
                Some(k) => this.run_merged(k, "pose.set", req),
                None => {
                    this.run("pose.set", req);
                }
            }
        };
        section(ui, "Joint", true, |ui| {
            ui.weak("Drag a body part in the view to bend it, or turn the coloured rings.");
            let mut r = local_at(scene, id, frame).map(|t| t.euler_degrees()).unwrap_or_default();
            let mut changed = false;
            let mut started = false;
            egui::Grid::new("joint_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                for (i, label) in ["Bend  X", "Twist  Y", "Side  Z"].iter().enumerate() {
                    ui.label(*label);
                    let resp = ui.add(egui::Slider::new(&mut r[i], -180.0..=180.0).suffix("°").max_decimals(0));
                    changed |= resp.changed();
                    started |= resp.drag_started();
                    ui.end_row();
                }
            });
            if started {
                self.gesture += 1;
            }
            if changed {
                set(self, json!({ joint: r }), Some(&format!("joint:{id}")));
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button("Reset").clicked() {
                    set(self, json!({ joint: [0, 0, 0] }), None);
                }
                let other = joint
                    .strip_suffix("_l")
                    .map(|b| format!("{b}_r"))
                    .or_else(|| joint.strip_suffix("_r").map(|b| format!("{b}_l")));
                if let Some(o) = other
                    && ui.button(format!("Mirror to {o}")).clicked()
                {
                    set(self, json!({ o: [r[0], -r[1], -r[2]] }), None);
                }
                if ui.button("Select person").clicked() {
                    self.select(Some(root.clone()));
                }
            });
        });
    }

    fn bottom(&mut self, ui: &mut egui::Ui) {
        let scene = self.scene();
        let t = scene.timeline.clone();
        let fps = t.fps.max(1.0);
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            if ui.button("⏮").on_hover_text("To the start (Home)").clicked() {
                self.hub.borrow_mut().frame = t.start as f32;
            }
            if ui
                .add_sized([62.0, 22.0], egui::Button::new(if self.playing { "⏸ Pause" } else { "▶ Play" }))
                .on_hover_text("Space")
                .clicked()
            {
                self.playing = !self.playing;
            }
            if ui.button("⏭").on_hover_text("To the end (End)").clicked() {
                self.hub.borrow_mut().frame = t.end as f32;
            }
            let has_cam = camera_id_at(&scene, t.start as f32).is_some();
            if ui
                .add_enabled(has_cam, egui::Button::new("🎥 Preview shot"))
                .on_hover_text("Play from the start, looking through the shot camera")
                .clicked()
            {
                self.hub.borrow_mut().frame = t.start as f32;
                self.look_through = true;
                self.hub.borrow_mut().look_through = true;
                self.playing = true;
            }
            ui.separator();
            ui.monospace(format!("{:>4.0}", self.frame()));
            ui.weak(format!("/ {} · {:.2} s", t.end, (self.frame() - t.start as f32) / fps));
            ui.separator();
            let sel = self.selected();
            if ui
                .add_enabled(sel.is_some(), egui::Button::new("◆ Key"))
                .on_hover_text("Keyframe the selection at this frame (K)")
                .clicked()
            {
                self.key_selection();
            }
            ui.toggle_value(&mut self.auto_key, "Auto-key")
                .on_hover_text("Every move, turn or pose change is keyed at the current frame");
            ui.separator();
            let (mut fps2, mut start, mut end) = (t.fps, t.start, t.end);
            let a = ui.add(egui::DragValue::new(&mut fps2).range(1.0..=120.0).suffix(" fps"));
            let b = ui.add(egui::DragValue::new(&mut start).range(0..=end));
            ui.label("–");
            let c = ui.add(egui::DragValue::new(&mut end).range(start..=100_000));
            if a.changed() || b.changed() || c.changed() {
                self.run("timeline.set", json!({"fps": fps2, "start": start, "end": end}));
            }
            if let Some(cam) = scene.active_camera.clone()
                && ui
                    .small_button("+ Shot")
                    .on_hover_text("A shot through the active camera over the full range")
                    .clicked()
            {
                self.run("shot.create", json!({"camera": cam, "start": t.start, "end": t.end}));
            }
            ui.separator();
            ui.toggle_value(&mut self.show_console, "Console")
                .on_hover_text("Type commands: the same API as window.shotstage, the CLI and MCP");
        });
        self.track_strip(ui, &scene);
        if self.show_console {
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("Command console");
                ui.weak("JSON request: {\"command\", \"params\"} or {\"batch\": [...]}");
                if ui.small_button("List commands").clicked() {
                    let list = self.hub.borrow().session.list();
                    self.console_out =
                        list.iter().map(|c| format!("{:<24} {}", c.id, c.summary)).collect::<Vec<_>>().join("\n");
                }
            });
            ui.columns(2, |cols| {
                cols[0].add(
                    egui::TextEdit::multiline(&mut self.console)
                        .code_editor()
                        .desired_rows(5)
                        .desired_width(f32::INFINITY),
                );
                if cols[0].button("Run").clicked() {
                    self.console_out = match serde_json::from_str::<Value>(&self.console) {
                        Ok(req) => match hub::dispatch(&self.hub, req) {
                            Ok(o) => serde_json::to_string_pretty(&o.result).unwrap_or_default(),
                            Err(e) => format!("error {}: {}", e.code, e.message),
                        },
                        Err(e) => format!("not JSON: {e}"),
                    };
                }
                egui::ScrollArea::vertical().id_salt("console_out").max_height(140.0).show(&mut cols[1], |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&self.console_out).monospace()).wrap());
                });
            });
        }
        ui.add_space(2.0);
    }

    /// Animated objects as rows of keyframe diamonds under a ruler you can scrub. Drag a
    /// diamond to retime it, right-click it for its easing, double-click a row to add a key.
    fn track_strip(&mut self, ui: &mut egui::Ui, scene: &Scene) {
        let t = &scene.timeline;
        let (t0, t1) = (t.start as f32, t.end.max(t.start + 1) as f32);
        let mut rows = timeline_rows(scene);
        let sel_root = self.selected().map(|s| root_of(scene, &s));
        if let Some(r) = &sel_root
            && !rows.iter().any(|row| &row.id == r)
            && let Ok(e) = scene.get(r)
        {
            rows.insert(0, TrackRow { id: r.clone(), label: e.name.clone(), tracks: vec![], frames: vec![] });
        }
        let label_w = 130.0;
        let row_h = 18.0;
        let full = ui.available_width();
        // ruler
        let (ruler, ruler_resp) = ui.allocate_exact_size(Vec2::new(full, 18.0), Sense::click_and_drag());
        let area = Rect::from_min_max(egui::pos2(ruler.left() + label_w, ruler.top()), ruler.max);
        let x_of = |f: f32| area.left() + (f - t0) / (t1 - t0) * area.width();
        let f_of = |x: f32| (t0 + (x - area.left()) / area.width() * (t1 - t0)).clamp(t0, t1).round();
        let p = ui.painter_at(ruler);
        p.rect_filled(ruler, 2.0, Color32::from_gray(30));
        // shots as coloured bands
        for (i, s) in t.shots.iter().enumerate() {
            let r = Rect::from_min_max(
                egui::pos2(x_of(s.start as f32), ruler.top()),
                egui::pos2(x_of(s.end as f32 + 1.0).min(area.right()), ruler.top() + 4.0),
            );
            p.rect_filled(
                r,
                1.0,
                [Color32::from_rgb(70, 150, 255), Color32::from_rgb(120, 200, 120), Color32::from_rgb(230, 170, 70)]
                    [i % 3],
            );
        }
        let fps = t.fps.max(1.0);
        let secs = (t1 - t0) / fps;
        let every = [0.5f32, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
            .into_iter()
            .find(|s| secs / s <= area.width() / 60.0)
            .unwrap_or(120.0);
        let mut s = 0.0;
        while s <= secs + 1e-3 {
            let x = x_of(t0 + s * fps);
            p.line_segment(
                [egui::pos2(x, ruler.bottom() - 6.0), egui::pos2(x, ruler.bottom())],
                (1.0, Color32::from_gray(110)),
            );
            p.text(
                egui::pos2(x + 2.0, ruler.top() + 4.0),
                egui::Align2::LEFT_TOP,
                format!("{s}s"),
                egui::FontId::proportional(10.0),
                Color32::from_gray(150),
            );
            s += every;
        }
        p.text(
            egui::pos2(ruler.left() + 4.0, ruler.center().y),
            egui::Align2::LEFT_CENTER,
            "Timeline",
            egui::FontId::proportional(11.0),
            Color32::from_gray(170),
        );
        if (ruler_resp.dragged() || ruler_resp.clicked())
            && let Some(pos) = ruler_resp.interact_pointer_pos()
            && pos.x >= area.left()
        {
            self.playing = false;
            self.hub.borrow_mut().frame = f_of(pos.x);
        }
        if ruler_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        let shots = t.shots.clone();
        ruler_resp.context_menu(|ui| {
            if shots.is_empty() {
                ui.weak("No shots yet: + Shot makes one through the active camera");
            }
            for s in &shots {
                ui.menu_button(format!("{} · {}–{} · {}", s.name, s.start, s.end, s.camera), |ui| {
                    if ui.button("Look through its camera").clicked() {
                        self.run("camera.setActive", json!({"id": s.camera}));
                        self.select(Some(s.camera.clone()));
                        self.hub.borrow_mut().frame = s.start as f32;
                        self.look_through = true;
                        self.hub.borrow_mut().look_through = true;
                        ui.close();
                    }
                    if ui.button("Delete shot").clicked() {
                        self.run("shot.delete", json!({"id": s.id}));
                        ui.close();
                    }
                });
            }
        });
        let mut playhead_bottom = ruler.bottom();
        egui::ScrollArea::vertical().id_salt("tracks").max_height(row_h * 5.0 + 2.0).auto_shrink([false, true]).show(ui, |ui| {
            if rows.is_empty() {
                ui.weak("No animation yet: select something and press ◆ Key, or use a preset under Animate on the right.");
            }
            for (ri, row) in rows.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(full, row_h), Sense::click_and_drag());
                playhead_bottom = rect.bottom();
                let p = ui.painter_at(rect);
                let selected = sel_root.as_deref() == Some(row.id.as_str());
                p.rect_filled(rect, 0.0, if selected { Color32::from_rgb(34, 44, 60) } else if ri % 2 == 0 { Color32::from_gray(24) } else { Color32::from_gray(28) });
                p.text(egui::pos2(rect.left() + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, &row.label, egui::FontId::proportional(11.5), if selected { Color32::WHITE } else { Color32::from_gray(190) });
                // segments between keys show their easing
                for w in row.frames.windows(2) {
                    let y = rect.center().y;
                    p.line_segment([egui::pos2(x_of(w[0]), y), egui::pos2(x_of(w[1]), y)], (2.0, Color32::from_gray(60)));
                }
                let near = resp.hover_pos().and_then(|h| row.frames.iter().copied().find(|f| (x_of(*f) - h.x).abs() < 6.0));
                for f in &row.frames {
                    let c = egui::pos2(x_of(*f), rect.center().y);
                    let hot = near == Some(*f) || self.key_drag.as_ref().is_some_and(|d| d.row == row.id && (d.frame - f).abs() < 0.5);
                    let r = if hot { 6.0 } else { 4.5 };
                    let pts = vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)];
                    p.add(egui::Shape::convex_polygon(pts, if hot { Color32::WHITE } else { ACCENT }, (1.0, Color32::from_gray(20))));
                }
                if resp.hovered() && near.is_some() {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                if resp.drag_started() && let Some(f) = near {
                    let key = self.new_key("retime");
                    self.key_drag = Some(KeyDrag { row: row.id.clone(), frame: f, key });
                }
                if resp.dragged()
                    && let (Some(d), Some(pos)) = (self.key_drag.clone(), resp.interact_pointer_pos())
                    && d.row == row.id
                {
                    let to = f_of(pos.x);
                    if (to - d.frame).abs() >= 1.0 && !row.frames.iter().any(|f| (*f - to).abs() < 0.5) {
                        for (id, prop) in &row.tracks {
                            let has = scene.timeline.track(id, *prop).is_some_and(|tr| tr.keys.iter().any(|k| (k.frame - d.frame).abs() < 1e-3));
                            if has {
                                self.run_key(&d.key, "keyframe.move", json!({"id": id, "property": prop, "from": d.frame, "to": to}));
                            }
                        }
                        self.key_drag = Some(KeyDrag { frame: to, ..d });
                        self.hub.borrow_mut().frame = to;
                    }
                }
                if resp.drag_stopped() {
                    self.key_drag = None;
                }
                if resp.clicked() {
                    self.select(Some(row.id.clone()));
                    if let Some(pos) = resp.interact_pointer_pos() {
                        self.hub.borrow_mut().frame = near.unwrap_or(f_of(pos.x));
                        self.playing = false;
                    }
                }
                if resp.double_clicked() && near.is_none() {
                    self.key_selection();
                }
                if resp.secondary_clicked() {
                    self.key_menu = near.map(|f| (row.id.clone(), f));
                }
                let menu_for = self.key_menu.clone().filter(|(r, _)| *r == row.id);
                resp.context_menu(|ui| {
                    let Some((_, f)) = menu_for.clone() else {
                        ui.weak("Right-click a key diamond");
                        return;
                    };
                    ui.weak(format!("Key at frame {f:.0}"));
                    ui.label("Ease into the next key");
                    for (label, interp) in [("Smooth (ease)", "ease"), ("Ease in", "easeIn"), ("Ease out", "easeOut"), ("Linear", "linear"), ("Overshoot", "overshoot"), ("Hold (step)", "step")] {
                        if ui.button(label).clicked() {
                            let steps: Vec<Value> = row.tracks.iter().filter(|(id, prop)| scene.timeline.track(id, *prop).is_some_and(|tr| tr.keys.iter().any(|k| (k.frame - f).abs() < 1e-3)))
                                .map(|(id, prop)| json!({"command": "keyframe.setInterp", "params": {"id": id, "property": prop, "frame": f, "interp": interp}})).collect();
                            let _ = hub::dispatch(&self.hub, json!({"batch": steps}));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("Delete key").clicked() {
                        let steps: Vec<Value> = row.tracks.iter().filter(|(id, prop)| scene.timeline.track(id, *prop).is_some_and(|tr| tr.keys.iter().any(|k| (k.frame - f).abs() < 1e-3)))
                            .map(|(id, prop)| json!({"command": "keyframe.delete", "params": {"id": id, "property": prop, "frame": f}})).collect();
                        let _ = hub::dispatch(&self.hub, json!({"batch": steps}));
                        ui.close();
                    }
                });
            }
        });
        // playhead across the ruler and the rows
        let x = x_of(self.frame());
        ui.painter().line_segment(
            [egui::pos2(x, ruler.top()), egui::pos2(x, playhead_bottom)],
            (1.5, Color32::from_rgb(255, 120, 90)),
        );
    }

    /// Keyframe the selection at the playhead (a whole person: every joint).
    fn key_selection(&mut self) {
        let Some(id) = self.selected() else { return };
        let scene = self.scene();
        let f = self.frame().round();
        let mut steps = vec![];
        let ids: Vec<String> = match rig::person_root(&scene, &id) {
            Ok(root) if root == id => {
                std::iter::once(root.clone()).chain(rig::JOINTS.iter().map(|j| format!("{root}_{j}"))).collect()
            }
            _ => vec![id.clone()],
        };
        for e in &ids {
            steps.push(json!({"command": "keyframe.set", "params": {"id": e, "property": "transform", "frame": f}}));
            if matches!(scene.get(e).map(|x| &x.kind), Ok(EntityKind::Camera(_))) {
                steps.push(
                    json!({"command": "keyframe.set", "params": {"id": e, "property": "focalLength", "frame": f}}),
                );
            }
        }
        match hub::dispatch(&self.hub, json!({"batch": steps})) {
            Ok(_) => {
                self.toast(format!("Keyed {} at frame {f:.0}", scene.get(&id).map(|e| e.name.clone()).unwrap_or(id)))
            }
            Err(e) => self.toast(format!("Could not key: {}", e.message)),
        }
    }

    fn viewport(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let full = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(full, Sense::click_and_drag());
        let scene = self.scene();
        let f = self.frame();
        let cam = if self.look_through { camera_id_at(&scene, f) } else { None };

        // Letterbox to the shot camera's aspect when looking through it.
        let (rect, view) = match cam
            .as_deref()
            .and_then(|c| Some((c, camera_data_at(&scene, c, f).ok()?, world_at(&scene, c, f).ok()?)))
        {
            Some((_, lens, world)) => {
                let a = lens.aspect;
                let mut size = full.size();
                if size.x / size.y > a {
                    size.x = size.y * a
                } else {
                    size.y = size.x / a
                }
                let rect = Rect::from_center_size(full.center(), size);
                (rect, View { view: world.inverse(), proj: lens.projection(a), depth_near: 0.0, depth_far: 0.0 })
            }
            None => {
                let aspect = (full.width() / full.height().max(1.0)).max(0.01);
                let proj = Mat4::perspective_rh(45f32.to_radians(), aspect, 0.05, 1000.0);
                (full, View { view: self.orbit.view(), proj, depth_near: 0.0, depth_far: 0.0 })
            }
        };

        self.last_view = Some((rect, view));
        let (pointer, modifiers, scroll, esc, released) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.modifiers,
                i.smooth_scroll_delta.y,
                i.key_pressed(egui::Key::Escape),
                i.pointer.any_released(),
            )
        });
        let inside = pointer.filter(|p| rect.contains(*p));
        let eye = view.view.inverse().w_axis.truncate();
        let clean = cam.is_some();

        // 1. Blocks dragged in from the palette: spawn on entering, follow the pointer, keep on
        //    release inside the viewport, undo when released outside or on Esc.
        if let Some(pd) = self.palette.as_mut() {
            let mut finish = None;
            if let Some(p) = inside {
                if pd.spawned.is_none() {
                    let (block, key) = (pd.block.clone(), pd.key.clone());
                    let id = self.spawn(&block, &key);
                    if let Some(pd) = self.palette.as_mut() {
                        pd.spawned = id;
                    }
                }
                if let Some(pd) = &self.palette
                    && let Some(id) = &pd.spawned
                {
                    self.place_on_ray(&pd.key, id, ray_at(rect, &view, p), Vec3::ZERO, modifiers.command);
                    self.hovered = None;
                }
            }
            let pd = self.palette.as_ref().expect("checked above");
            if released || esc {
                finish = Some((inside.is_some() && !esc, pd.spawned.clone()));
            }
            if let Some((keep, spawned)) = finish {
                match (keep, spawned) {
                    (true, Some(id)) => self.select(Some(id)),
                    (false, Some(_)) => {
                        self.run("history.undo", json!({}));
                    }
                    _ => {}
                }
                self.palette = None;
            } else if let Some(p) = pointer {
                let painter =
                    ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("block_drag")));
                let label = pd.block.label();
                let r = Rect::from_min_size(p + Vec2::new(14.0, 10.0), Vec2::new(8.0 + 7.0 * label.len() as f32, 20.0));
                painter.rect_filled(r, 4.0, Color32::from_rgba_unmultiplied(40, 40, 46, 230));
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
        }

        // 2. Gizmo handles of the selection (hidden in the clean camera view).
        let selected = self.selected().filter(|s| scene.contains(s));
        // Objects get move arrows, a turn ring and a scale box; a person's joint gets three
        // rotation rings.
        let gizmo = selected.as_ref().filter(|s| !clean && !scene.get(s).is_ok_and(|e| e.locked)).and_then(|id| {
            let o = world_at(&scene, id, f).ok()?.w_axis.truncate();
            let joint = rig::is_joint(&scene, id);
            let len = (eye - o).length().max(0.1) * if joint { 0.09 } else { 0.14 };
            let c = project(rect, &view, o)?;
            let axes: Vec<Option<Pos2>> = if joint {
                vec![]
            } else {
                [Vec3::X, Vec3::Y, Vec3::Z].iter().map(|a| project(rect, &view, o + *a * len)).collect()
            };
            let ring_axes: &[usize] = if joint { &[0, 1, 2] } else { &[1] };
            let rings: Vec<(usize, Vec<Pos2>)> = ring_axes
                .iter()
                .map(|&ax| {
                    let pts = (0..=48)
                        .filter_map(|i| {
                            let t = i as f32 / 48.0 * std::f32::consts::TAU;
                            let (u, v) = (t.cos() * len * 0.8, t.sin() * len * 0.8);
                            let p = match ax {
                                0 => Vec3::new(0.0, u, v),
                                1 => Vec3::new(u, 0.0, v),
                                _ => Vec3::new(u, v, 0.0),
                            };
                            project(rect, &view, o + p)
                        })
                        .collect();
                    (ax, pts)
                })
                .collect();
            Some((id.clone(), o, len, c, axes, rings, joint))
        });
        // The motion path of the selection (its group, or the camera): a line through every frame
        // and a dot per key you can drag.
        let path = selected.as_ref().filter(|_| !clean).and_then(|s| {
            let id = if matches!(scene.get(s).map(|e| &e.kind), Ok(EntityKind::Camera(_))) {
                s.clone()
            } else {
                root_of(&scene, s)
            };
            let track = scene.timeline.track(&id, TrackProperty::Transform).filter(|t| t.keys.len() >= 2)?;
            let (a, b) = (track.keys.first()?.frame, track.keys.last()?.frame);
            let n = ((b - a).ceil() as usize).clamp(2, 400);
            let line: Vec<Pos2> = (0..=n)
                .filter_map(|i| {
                    project(
                        rect,
                        &view,
                        world_at(&scene, &id, a + (b - a) * i as f32 / n as f32).ok()?.w_axis.truncate(),
                    )
                })
                .collect();
            let keys: Vec<(f32, Pos2, Vec3)> = track
                .keys
                .iter()
                .filter_map(|k| {
                    let w = world_at(&scene, &id, k.frame).ok()?.w_axis.truncate();
                    Some((k.frame, project(rect, &view, w)?, w))
                })
                .collect();
            Some((id, line, keys))
        });
        let path_key_at = |p: Pos2| -> Option<(String, f32, Vec3)> {
            let (id, _, keys) = path.as_ref()?;
            keys.iter()
                .filter(|(_, sp, _)| (*sp - p).length() < 8.0)
                .min_by(|a, b| (a.1 - p).length().total_cmp(&(b.1 - p).length()))
                .map(|(f, _, w)| (id.clone(), *f, *w))
        };
        let handle_at = |p: Pos2| -> Option<Handle> {
            let (_, _, _, c, axes, rings, joint) = gizmo.as_ref()?;
            if !joint && (p - (*c + Vec2::new(30.0, -30.0))).length() < 8.0 {
                return Some(Handle::Scale);
            }
            for (i, a) in axes.iter().enumerate() {
                if let Some(a) = a
                    && seg_dist(p, *c, *a) < 7.0
                    && (p - *c).length() > 6.0
                {
                    return Some(Handle::Axis(i));
                }
            }
            rings
                .iter()
                .map(|(ax, pts)| (*ax, pts.windows(2).map(|w| seg_dist(p, w[0], w[1])).fold(f32::MAX, f32::min)))
                .filter(|(_, d)| *d < 5.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(ax, _)| Handle::Ring(ax))
        };
        if matches!(self.drag, Gesture::Idle) {
            self.hot_handle = inside.and_then(handle_at);
        }

        // 3. Hover highlight (whole group unless a part of it is selected).
        if self.palette.is_none() && matches!(self.drag, Gesture::Idle) {
            self.hovered = inside.filter(|_| self.hot_handle.is_none()).and_then(|p| {
                let (o, d) = ray_at(rect, &view, p);
                let hit = pick_grabbable(&scene, f, o, d)?;
                let root = root_of(&scene, &hit);
                let in_group =
                    selected.as_ref().is_some_and(|s| *s != root && spatial::subtree(&scene, &root).contains(s));
                if !in_group {
                    return Some(root);
                }
                let point = pick_point(&scene, f, o, d).map(|p| p.1).unwrap_or(o);
                Some(joint_at(&scene, &hit, point, f).unwrap_or(hit))
            });
        }

        // 4. Drags in the viewport.
        if self.palette.is_none() {
            let primary = resp.drag_started_by(egui::PointerButton::Primary);
            if primary && let Some(start) = ui.input(|i| i.pointer.press_origin()).filter(|p| rect.contains(*p)) {
                let (o, d) = ray_at(rect, &view, start);
                let hit = pick_grabbable(&scene, f, o, d);
                // with a joint of this person selected, dragging a body part bends it
                let hit_point = pick_point(&scene, f, o, d).map(|p| p.1).unwrap_or(o);
                let pose_joint = hit
                    .as_ref()
                    .filter(|h| {
                        selected
                            .as_ref()
                            .is_some_and(|s| rig::is_joint(&scene, s) && root_of(&scene, s) == root_of(&scene, h))
                    })
                    .and_then(|h| joint_at(&scene, h, hit_point, f))
                    .map(|j| drive_joint(&scene, &j));
                self.drag = if let Some((id, frame, origin)) = path_key_at(start) {
                    let key = self.new_key("path");
                    self.hub.borrow_mut().frame = frame;
                    Gesture::PathKey { id, frame, origin, key }
                } else if let (Some(h), Some((id, origin, ..))) = (handle_at(start), gizmo.as_ref()) {
                    let key = self.new_key("gizmo");
                    match h {
                        Handle::Axis(i) => {
                            let axis = [Vec3::X, Vec3::Y, Vec3::Z][i];
                            let t0 = closest_on_line(*origin, axis, o, d).unwrap_or(0.0);
                            Gesture::Axis { id: id.clone(), axis, start: *origin, t0, key }
                        }
                        Handle::Ring(axis) => {
                            let c = gizmo.as_ref().map(|g| g.3).unwrap_or(start);
                            Gesture::Ring { id: id.clone(), axis, last: (-(start.y - c.y)).atan2(start.x - c.x), key }
                        }
                        Handle::Scale => Gesture::Scale { id: id.clone(), key },
                    }
                } else if let Some(joint) = pose_joint {
                    let key = self.new_key("pose");
                    self.select(Some(joint.clone()));
                    let grab = raycast(&scene, f, o, d, &RayOptions::default())
                        .map(|h| Vec3::from_array(h.point))
                        .unwrap_or(o + d * 3.0);
                    Gesture::Aim { joint, grab, key }
                } else if let Some(hit) = hit {
                    // grab the group, or the selected part of it
                    let root = root_of(&scene, &hit);
                    let mut target = match &selected {
                        Some(s) if spatial::subtree(&scene, &root).contains(s) => s.clone(),
                        _ => root,
                    };
                    let key = self.new_key("move");
                    if modifiers.alt
                        && let Some(r) =
                            self.run_key(&key, "entity.duplicate", json!({"id": target, "offset": [0, 0, 0]}))
                        && let Some(copy) = r["id"].as_str()
                    {
                        target = copy.to_string();
                    }
                    self.select(Some(target.clone()));
                    // Hold the object where it was grabbed: the pointer moves on the horizontal
                    // plane through the grab point, and the object drops onto what is below.
                    let scene = self.scene();
                    let grab = raycast(&scene, f, o, d, &RayOptions::default())
                        .map(|h| Vec3::from_array(h.point))
                        .unwrap_or(o + d * 5.0);
                    let centre = spatial::bounds(&scene, &target, f).ok().flatten().map(|b| b.center()).unwrap_or(grab);
                    let offset = Vec3::new(centre.x - grab.x, 0.0, centre.z - grab.z);
                    Gesture::Move { id: target, grab, offset, key, lift: None }
                } else if cam.is_none() {
                    Gesture::Navigate
                } else {
                    Gesture::Idle
                };
            }
            if esc && !matches!(self.drag, Gesture::Idle | Gesture::Navigate | Gesture::Cancelled) {
                self.run("history.undo", json!({}));
                self.drag = Gesture::Cancelled;
            }
            let ptr = pointer.unwrap_or(rect.center());
            let (o, d) = ray_at(rect, &view, ptr);
            match self.drag.clone() {
                Gesture::Move { id, grab, offset, key, lift } if resp.dragged() => {
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                    if modifiers.shift {
                        // lift straight up: follow the pointer along the vertical line through the object
                        let base = lift.map(|l| l.0).unwrap_or_else(|| {
                            world_at(&scene, &id, f).map(|m| m.w_axis.truncate()).unwrap_or_default()
                        });
                        let t0 = lift.map(|l| l.1).or_else(|| closest_on_line(base, Vec3::Y, o, d)).unwrap_or(0.0);
                        if let Some(t) = closest_on_line(base, Vec3::Y, o, d) {
                            let mut p =
                                json!({"id": id, "position": (base + Vec3::Y * (t - t0)).to_array(), "frame": f});
                            if self.keyed(&scene, &id) {
                                p["keyframeAt"] = json!(f.round());
                            }
                            self.run_key(&key, "entity.moveTo", p);
                        }
                        self.drag = Gesture::Move { id, grab, offset, key, lift: Some((base, t0)) };
                    } else {
                        let t = if d.y.abs() > 1e-4 { (grab.y - o.y) / d.y } else { -1.0 };
                        if t > 0.0 && t < 500.0 {
                            let q = o + d * t + offset;
                            let down = (Vec3::new(q.x, grab.y + 0.01, q.z), Vec3::NEG_Y);
                            self.place_on_ray(&key, &id, down, Vec3::ZERO, modifiers.command);
                        } else {
                            self.place_on_ray(&key, &id, (o, d), offset, modifiers.command);
                        }
                        self.drag = Gesture::Move { id: id.clone(), grab, offset, key: key.clone(), lift: None };
                        if scroll != 0.0 {
                            self.run_key(
                                &key,
                                "entity.rotate",
                                json!({"id": id, "degrees": scroll.signum() * 15.0, "frame": f}),
                            );
                        }
                    }
                }
                Gesture::Axis { id, axis, start, t0, key } if resp.dragged() => {
                    if let Some(t) = closest_on_line(start, axis, o, d) {
                        let mut step = t - t0;
                        if self.snap && !modifiers.command {
                            step = (step / self.snap_step).round() * self.snap_step;
                        }
                        let mut p = json!({"id": id, "position": (start + axis * step).to_array(), "frame": f});
                        if self.keyed(&scene, &id) {
                            p["keyframeAt"] = json!(f.round());
                        }
                        self.run_key(&key, "entity.moveTo", p);
                    }
                }
                Gesture::Ring { id, axis, last, key } if resp.dragged() => {
                    if let Some((_, go, _, c, ..)) = gizmo.as_ref() {
                        let a = (-(ptr.y - c.y)).atan2(ptr.x - c.x);
                        let mut delta = (a - last).to_degrees();
                        if delta > 180.0 {
                            delta -= 360.0;
                        } else if delta < -180.0 {
                            delta += 360.0;
                        }
                        // seen from the far side of the ring, screen rotation is mirrored
                        let ax = [Vec3::X, Vec3::Y, Vec3::Z][axis];
                        let sign = if (eye - *go).dot(ax) >= 0.0 { 1.0 } else { -1.0 };
                        self.run_key(
                            &key,
                            "entity.rotate",
                            json!({"id": id, "degrees": delta * sign, "axis": (["x", "y", "z"][axis]), "frame": f}),
                        );
                        self.drag = Gesture::Ring { id, axis, last: a, key };
                    }
                }
                Gesture::Aim { joint, grab, key } if resp.dragged() => {
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                    let n = (eye - grab).normalize_or_zero();
                    let denom = d.dot(n);
                    if denom.abs() > 1e-5 {
                        let t = (grab - o).dot(n) / denom;
                        if t > 0.0 {
                            let mut p = json!({"id": joint, "target": (o + d * t).to_array(), "frame": f});
                            if self.keyed(&scene, &joint) {
                                p["keyframeAt"] = json!(f.round());
                            }
                            self.run_key(&key, "pose.aim", p);
                        }
                    }
                }
                Gesture::PathKey { id, frame, origin, key } if resp.dragged() => {
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                    // slide on the horizontal plane through the key; Shift moves it up and down
                    let to = if modifiers.shift {
                        closest_on_line(origin, Vec3::Y, o, d).map(|_| {
                            let t0 = closest_on_line(origin, Vec3::Y, o, d).unwrap_or(0.0);
                            origin + Vec3::Y * t0
                        })
                    } else if d.y.abs() > 1e-4 {
                        let t = (origin.y - o.y) / d.y;
                        (t > 0.0).then(|| o + d * t)
                    } else {
                        None
                    };
                    if let Some(to) = to
                        && let Some(k) = scene
                            .timeline
                            .track(&id, TrackProperty::Transform)
                            .and_then(|tr| tr.keys.iter().find(|k| (k.frame - frame).abs() < 1e-3))
                        && let shotstage_core::KeyValue::Transform(mut t) = k.value
                    {
                        let parent = scene
                            .get(&id)
                            .ok()
                            .and_then(|e| e.parent.clone())
                            .and_then(|p| world_at(&scene, &p, frame).ok())
                            .unwrap_or(Mat4::IDENTITY);
                        t.translation = parent.inverse().transform_point3(to).to_array();
                        self.run_key(&key, "keyframe.set", json!({"id": id, "property": "transform", "frame": frame, "value": {"kind": "transform", "value": t}, "interp": k.interp}));
                    }
                }
                Gesture::Scale { id, key } if resp.dragged() => {
                    let dd = resp.drag_delta();
                    let k = (1.0 + (dd.x - dd.y) * 0.006).clamp(0.5, 2.0);
                    if let Ok(t) = local_at(&scene, &id, f) {
                        let sc = (t.scale() * k).to_array();
                        let mut p = json!({"id": id, "scale": sc});
                        if self.keyed(&scene, &id) {
                            p["keyframeAt"] = json!(f.round());
                        }
                        self.run_key(&key, "entity.setTransform", p);
                    }
                }
                Gesture::Navigate if resp.dragged_by(egui::PointerButton::Primary) && !modifiers.shift => {
                    let d = resp.drag_delta();
                    self.orbit.yaw -= d.x * 0.008;
                    self.orbit.pitch = (self.orbit.pitch + d.y * 0.008).clamp(-1.5, 1.5);
                }
                _ => {}
            }
            if resp.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
                self.drag = Gesture::Idle;
            }
        }
        if cam.is_none() && self.palette.is_none() {
            let pan = resp.dragged_by(egui::PointerButton::Secondary)
                || resp.dragged_by(egui::PointerButton::Middle)
                || (matches!(self.drag, Gesture::Navigate)
                    && resp.dragged_by(egui::PointerButton::Primary)
                    && modifiers.shift);
            if pan {
                let d = resp.drag_delta();
                let inv = self.orbit.view().inverse();
                let k = self.orbit.distance * 0.0015;
                self.orbit.target += (-inv.x_axis.xyz() * d.x + inv.y_axis.xyz() * d.y) * k;
            }
            if resp.hovered() && !matches!(self.drag, Gesture::Move { .. }) && scroll != 0.0 {
                self.orbit.distance = (self.orbit.distance * (1.0 - scroll * 0.0015)).clamp(0.3, 500.0);
            }
            // trackpad pinch (and Ctrl+wheel) arrives as a zoom factor, not as scrolling
            let pinch = ui.input(|i| i.zoom_delta());
            if resp.hovered() && pinch != 1.0 && pinch.is_finite() && pinch > 0.0 {
                self.orbit.distance = (self.orbit.distance / pinch).clamp(0.3, 500.0);
            }
            if resp.double_clicked()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let (o, d) = ray_at(rect, &view, pos);
                if let Some(hit) = pick(&scene, f, o, d)
                    && let Ok(Some(b)) = spatial::bounds(&scene, &root_of(&scene, &hit), f)
                {
                    self.orbit.target = b.center();
                    self.orbit.distance = (b.size().max_element() * 2.2).clamp(0.4, 200.0);
                }
            }
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos().filter(|p| rect.contains(*p))
        {
            let (o, d) = ray_at(rect, &view, pos);
            // lights and cameras are small: pick them by their icon on screen first
            let icon = (!clean)
                .then(|| {
                    scene
                        .entities
                        .iter()
                        .filter(|e| matches!(e.kind, EntityKind::Light(_) | EntityKind::Camera(_)))
                        .filter(|e| cam.as_deref() != Some(e.id.as_str()))
                        .filter_map(|e| {
                            let p = project(rect, &view, world_at(&scene, &e.id, f).ok()?.w_axis.truncate())?;
                            let dist = (p - pos).length();
                            (dist < 14.0).then(|| (dist, e.id.clone()))
                        })
                        .min_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|(_, id)| id)
                })
                .flatten();
            let sel = icon.or_else(|| {
                pick_point(&scene, f, o, d).map(|(hit, point)| {
                    let root = root_of(&scene, &hit);
                    // first click takes the whole group; clicking inside a selected group picks the part
                    match &selected {
                        Some(s) if spatial::subtree(&scene, &root).contains(s) => {
                            joint_at(&scene, &hit, point, f).unwrap_or(hit)
                        }
                        _ => root,
                    }
                })
            });
            self.select(sel);
        }
        if self.hovered.is_some() && matches!(self.drag, Gesture::Idle) && self.palette.is_none() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if self.hot_handle.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let scene = self.scene();
        let dragged_id = match (&self.drag, &self.palette) {
            (Gesture::Move { id, .. }, _) => Some(id.clone()),
            (_, Some(PaletteDrag { spawned: Some(id), .. })) => Some(id.clone()),
            _ => None,
        };

        let ppp = ui.ctx().pixels_per_point();
        let px = ((rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32);
        // Looking through the shot camera shows the clean reference frame: no grid, no gizmos.
        let overlay = Overlay {
            selected: if clean { None } else { self.selected() },
            grid: !clean,
            cameras: !clean,
            looking_through: cam.clone(),
            hovered: if clean { None } else { self.hovered.clone() },
            footprint: dragged_id,
            studio_light: self.studio_light,
            eye: None,
        };
        let pass = self.pass;
        let (Some(rs), Some(renderer)) = (frame.wgpu_render_state(), self.renderer.as_mut()) else {
            ui.painter().text(
                full.center(),
                egui::Align2::CENTER_CENTER,
                "GPU not available",
                egui::FontId::proportional(16.0),
                Color32::GRAY,
            );
            return;
        };
        if px.0 < 2 || px.1 < 2 {
            return;
        }
        renderer.render(&rs.device, &rs.queue, &scene, f, view, pass, &overlay, px);
        let generation = renderer.target_generation;
        if self.texture.is_none_or(|(_, g)| g != generation)
            && let Some(view_tex) = renderer.color_view()
        {
            let mut egui_renderer = rs.renderer.write();
            if let Some((old, _)) = self.texture.take() {
                egui_renderer.free_texture(&old);
            }
            let id = egui_renderer.register_native_texture(&rs.device, view_tex, wgpu::FilterMode::Linear);
            self.texture = Some((id, generation));
        }
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, Color32::from_gray(10));
        if let Some((id, _)) = self.texture {
            painter.image(id, rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        if let Some(c) = &cam {
            painter.rect_stroke(rect, 0.0, (1.0, Color32::from_rgb(90, 150, 255)), egui::StrokeKind::Inside);
            if self.safe_areas {
                // action safe (93 %) and title safe (90 %), plus a centre cross
                for (k, a) in [(0.93, 70u8), (0.9, 50u8)] {
                    let r = Rect::from_center_size(rect.center(), rect.size() * k);
                    painter.rect_stroke(r, 0.0, (1.0, Color32::from_white_alpha(a)), egui::StrokeKind::Inside);
                }
                let c = rect.center();
                let s = (1.0, Color32::from_white_alpha(70));
                painter.line_segment([c - Vec2::new(10.0, 0.0), c + Vec2::new(10.0, 0.0)], s);
                painter.line_segment([c - Vec2::new(0.0, 10.0), c + Vec2::new(0.0, 10.0)], s);
            }
            // rule-of-thirds guides
            for i in 1..3 {
                let x = rect.left() + rect.width() * i as f32 / 3.0;
                let y = rect.top() + rect.height() * i as f32 / 3.0;
                let s = (0.5, Color32::from_white_alpha(40));
                painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], s);
                painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], s);
            }
            if let Some(r) = scene.references.get(c).cloned()
                && let Some(tex) = self.reference_texture(ui.ctx(), &scene, &r.asset)
            {
                // fit the picture inside the frame, keeping its aspect
                let sz = tex.size_vec2();
                let k = (rect.width() / sz.x).min(rect.height() / sz.y);
                let img = Rect::from_center_size(rect.center(), sz * k);
                let a = (r.opacity.clamp(0.0, 1.0) * 255.0) as u8;
                painter.image(
                    tex.id(),
                    img,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::from_white_alpha(a),
                );
            }
            let hud = shotstage_core::eval::describe_shot(&scene, c, f, None)
                .map(|d| format!(" · {}", d.summary))
                .unwrap_or_default();
            painter.text(
                rect.left_top() + Vec2::new(8.0, 6.0),
                egui::Align2::LEFT_TOP,
                format!("{} · frame {f:.0}{hud}", scene.get(c).map(|e| e.name.as_str()).unwrap_or(c)),
                egui::FontId::monospace(12.0),
                Color32::from_white_alpha(180),
            );
        }
        if let Some((id, line, keys)) = &path {
            let cam = matches!(scene.get(id).map(|e| &e.kind), Ok(EntityKind::Camera(_)));
            let col = if cam { Color32::from_rgb(90, 170, 255) } else { Color32::from_rgb(255, 170, 80) };
            painter.add(egui::Shape::line(line.clone(), (1.5, col.gamma_multiply(0.8))));
            let hot = ui
                .input(|i| i.pointer.hover_pos())
                .and_then(|p| keys.iter().find(|(_, sp, _)| (*sp - p).length() < 8.0).map(|k| k.0));
            for (f, sp, _) in keys {
                let lit =
                    hot == Some(*f) || matches!(&self.drag, Gesture::PathKey { frame, .. } if (frame - f).abs() < 1e-3);
                painter.circle_filled(*sp, if lit { 6.0 } else { 4.0 }, if lit { Color32::WHITE } else { col });
                painter.circle_stroke(*sp, if lit { 6.0 } else { 4.0 }, (1.0, Color32::from_gray(20)));
            }
            if hot.is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            }
        }
        if let Some((_, _, _, c, axes, rings, joint)) = &gizmo {
            let hot = self.hot_handle;
            let active = match &self.drag {
                Gesture::Axis { axis, .. } => Some(Handle::Axis(axis.abs().max_position())),
                Gesture::Ring { axis, .. } => Some(Handle::Ring(*axis)),
                Gesture::Scale { .. } => Some(Handle::Scale),
                _ => None,
            };
            let lit = |h: Handle| hot == Some(h) || active == Some(h);
            let colors =
                [Color32::from_rgb(235, 80, 80), Color32::from_rgb(110, 210, 90), Color32::from_rgb(80, 140, 255)];
            for (ax, pts) in rings {
                let base = if *joint { colors[*ax] } else { Color32::from_rgb(230, 200, 90) };
                let (w, col) = if lit(Handle::Ring(*ax)) { (3.0, Color32::WHITE) } else { (1.8, base) };
                painter.add(egui::Shape::line(pts.clone(), (w, col)));
            }
            if *joint {
                painter.circle_filled(*c, 3.5, Color32::WHITE);
            }
            for (i, a) in axes.iter().enumerate() {
                if let Some(a) = a {
                    let w = if lit(Handle::Axis(i)) { 4.0 } else { 2.5 };
                    let col = if lit(Handle::Axis(i)) { Color32::WHITE } else { colors[i] };
                    painter.line_segment([*c, *a], (w, col));
                    painter.circle_filled(*a, 5.0, col);
                }
            }
            if !joint {
                let sq = Rect::from_center_size(*c + Vec2::new(30.0, -30.0), Vec2::splat(10.0));
                painter.rect_filled(sq, 1.0, if lit(Handle::Scale) { Color32::WHITE } else { Color32::from_gray(200) });
            }
        }
        self.toolbar(ui, full, cam.is_some() || camera_id_at(&scene, f).is_some());
        if scene.entities.is_empty() && self.palette.is_none() {
            self.welcome(ui, full);
        }
        // files dragged over the window
        if ui.input(|i| !i.raw.hovered_files.is_empty()) {
            painter.rect_filled(full.shrink(6.0), 8.0, Color32::from_rgba_unmultiplied(40, 98, 170, 60));
            painter.text(
                full.center(),
                egui::Align2::CENTER_CENTER,
                "Drop to import (glb, gltf, obj, stl, fbx, blend)",
                egui::FontId::proportional(18.0),
                Color32::WHITE,
            );
        }
        let now = ui.input(|i| i.time);
        if let Some((msg, t0)) = &mut self.toast {
            if t0.is_nan() {
                *t0 = now;
            }
            let age = now - *t0;
            if age < 5.0 {
                let alpha = ((5.0 - age).min(1.0) * 230.0) as u8;
                let font = egui::FontId::proportional(13.0);
                let galley = painter.layout(msg.clone(), font, Color32::from_white_alpha(alpha), full.width() - 80.0);
                let r = Rect::from_center_size(
                    egui::pos2(full.center().x, full.bottom() - 30.0 - galley.size().y / 2.0),
                    galley.size() + Vec2::new(24.0, 14.0),
                );
                painter.rect_filled(r, 6.0, Color32::from_rgba_unmultiplied(28, 30, 36, alpha));
                painter.galley(r.min + Vec2::new(12.0, 7.0), galley, Color32::WHITE);
                ui.ctx().request_repaint();
            } else {
                self.toast = None;
            }
        }
    }

    /// An egui texture of an image asset (decoded once).
    fn reference_texture(&mut self, ctx: &egui::Context, scene: &Scene, key: &str) -> Option<egui::TextureHandle> {
        use base64::Engine as _;
        let asset = scene.assets.get(key)?;
        let identity = asset.data.as_ptr() as usize ^ asset.data.len();
        if let Some((id, t)) = self.ref_textures.get(key)
            && *id == identity
        {
            return Some(t.clone());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(asset.data.as_bytes()).ok()?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
        let tex = ctx.load_texture(format!("ref:{key}"), color, egui::TextureOptions::LINEAR);
        self.ref_textures.insert(key.to_string(), (identity, tex.clone()));
        Some(tex)
    }

    /// The empty-scene card: start from a template or drag blocks in.
    fn welcome(&mut self, ui: &egui::Ui, full: Rect) {
        egui::Area::new(egui::Id::unique("welcome"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .constrain_to(full)
            .default_pos(full.center())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(16)).show(ui, |ui| {
                    ui.set_max_width(360.0);
                    ui.heading("Start a scene");
                    ui.weak("Pick a ready set (everything stays editable), or drag blocks in from the left.");
                    ui.add_space(6.0);
                    egui::Grid::new("welcome_grid").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
                        for (i, (name, label, desc)) in shotstage_commands::templates::TEMPLATES.iter().enumerate() {
                            if ui.add_sized([170.0, 34.0], egui::Button::new(*label)).on_hover_text(*desc).clicked() {
                                self.apply_template(name, label);
                            }
                            if i % 2 == 1 {
                                ui.end_row();
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.weak("Tip: drop a .glb / .fbx model or a picture on the view to bring it in.");
                });
            });
    }

    /// Floating view options at the top-left of the viewport.
    fn toolbar(&mut self, ui: &egui::Ui, full: Rect, has_cam: bool) {
        egui::Area::new(egui::Id::unique("viewport_toolbar"))
            .order(egui::Order::Foreground)
            .fixed_pos(full.left_top() + Vec2::new(8.0, 8.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                    // wrap rather than spill over the side panels in a narrow view
                    ui.set_max_width((full.width() - 28.0).max(120.0));
                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(&mut self.pass, Pass::Clay, "Clay").on_hover_text("Clay shading");
                        ui.selectable_value(&mut self.pass, Pass::Depth, "Depth")
                            .on_hover_text("Depth pass (near = white)");
                        ui.selectable_value(&mut self.pass, Pass::Normal, "Normal").on_hover_text("Normal pass");
                        ui.separator();
                        ui.toggle_value(&mut self.snap, "Snap")
                            .on_hover_text("Snap dragged blocks to a grid (hold Ctrl to place freely)");
                        ui.add_enabled(
                            self.snap,
                            egui::DragValue::new(&mut self.snap_step).range(0.01..=5.0).speed(0.01).suffix(" m"),
                        );
                        ui.separator();
                        ui.add_enabled_ui(has_cam, |ui| {
                            ui.toggle_value(&mut self.look_through, "🎥 Camera view")
                                .on_hover_text("Look through the shot camera (the clean reference frame)")
                        });
                        if !has_cam {
                            self.look_through = false;
                        }
                        ui.separator();
                        if ui
                            .add_enabled(has_cam, egui::Button::new("📷 Save frame"))
                            .on_hover_text("Save the shot camera's view at this frame as a PNG (1920 px wide)")
                            .clicked()
                        {
                            crate::files::save_frame(&self.hub, 1920, &self.saved);
                        }
                        ui.toggle_value(&mut self.studio_light, "Studio light").on_hover_text(
                            "Show the flat studio clay light instead of the scene's lights (scenes without lights always use it)",
                        );
                        ui.separator();
                        ui.label(egui::RichText::new(" ? ").strong()).on_hover_text(HELP);
                    });
                });
            });
    }
}
