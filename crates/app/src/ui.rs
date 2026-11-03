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
View: drag empty space to orbit · right-drag or Shift+drag to pan · scroll or pinch to zoom · double-click to focus.\n\
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
    let h = raycast(scene, frame, o, d, &RayOptions::default())?;
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
    let hit = pick(scene, frame, o, d)?;
    let locked = scene.get(&root_of(scene, &hit)).map(|e| e.locked).unwrap_or(false)
        || scene.get(&hit).map(|e| e.locked).unwrap_or(false);
    (!locked).then_some(hit)
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
        if is_animated(&scene, id) {
            p["keyframeAt"] = json!(f.round());
        }
        if p["snap"].is_null() {
            p.as_object_mut().map(|o| o.remove("snap"));
        }
        self.run_key(key, "entity.place", p);
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
        crate::files::take_dropped(ui.ctx(), &self.incoming);
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
            ui.separator();
            if ui
                .button("Import…")
                .on_hover_text("glTF / GLB, OBJ, STL, FBX and .blend (FBX / .blend on the desktop, through Blender). You can also drop files on the view.")
                .clicked()
            {
                crate::files::pick(&self.incoming, ui.ctx());
            }
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
            let cols = ((ui.available_width() + gap) / (74.0 + gap)).floor().max(2.0);
            let tile = ((ui.available_width() + gap) / cols - gap).floor();
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
                    ui.horizontal_wrapped(|ui| {
                        for b in &shown {
                            self.block_tile(ui, b, tile);
                        }
                    });
                });
            }
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
        let r = ui
            .add_sized(
                [width, 30.0],
                egui::Button::new(egui::RichText::new(b.label()).size(12.5)).sense(Sense::click_and_drag()),
            )
            .on_hover_cursor(CursorIcon::Grab)
            .on_hover_text(b.description());
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
        ui.weak(format!("{kind} · {}", e.id));
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

        let plain = person.is_none() && !matches!(e.kind, EntityKind::Empty);
        section(ui, "Transform", plain, |ui| {
            let animated = scene.timeline.track(&id, TrackProperty::Transform).is_some();
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
            EntityKind::Empty => {}
        }
    }

    /// A bone of an imported skeleton: plain rotation sliders.
    fn bone_section(&mut self, ui: &mut egui::Ui, scene: &Scene, id: &str, frame: f32) {
        let animated = is_animated(scene, id);
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
        let animated = rig::JOINTS.iter().any(|j| is_animated(scene, &format!("{root}_{j}")));
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
        let animated = is_animated(scene, id);
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
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            if ui
                .add_sized([64.0, 22.0], egui::Button::new(if self.playing { "⏸ Pause" } else { "▶ Play" }))
                .on_hover_text("Space")
                .clicked()
            {
                self.playing = !self.playing;
            }
            let mut f = self.frame();
            // keep room for the controls on the right
            let w = (ui.available_width() - 560.0).max(120.0);
            ui.spacing_mut().slider_width = w;
            ui.add(egui::Slider::new(&mut f, t.start as f32..=t.end as f32).step_by(1.0).show_value(false));
            if f != self.frame() {
                self.hub.borrow_mut().frame = f;
            }
            ui.monospace(format!("{:>4.0} / {}", self.frame(), t.end));
            ui.weak(format!("{:.1} s", (self.frame() - t.start as f32) / t.fps));
            ui.separator();
            let (mut fps, mut start, mut end) = (t.fps, t.start, t.end);
            let a = ui.add(egui::DragValue::new(&mut fps).range(1.0..=120.0).suffix(" fps"));
            let b = ui.add(egui::DragValue::new(&mut start).range(0..=end));
            ui.label("–");
            let c = ui.add(egui::DragValue::new(&mut end).range(start..=100_000));
            if a.changed() || b.changed() || c.changed() {
                self.run("timeline.set", json!({"fps": fps, "start": start, "end": end}));
            }
            ui.separator();
            let shots = match t.shots.as_slice() {
                [] => "no shots".to_string(),
                [one] => format!("{} {}–{}", one.name, one.start, one.end),
                many => format!("{} shots", many.len()),
            };
            let all = t
                .shots
                .iter()
                .map(|s| format!("{} {}–{} via {}", s.name, s.start, s.end, s.camera))
                .collect::<Vec<_>>();
            ui.weak(shots).on_hover_text(if all.is_empty() {
                "Shots: frame ranges seen through one camera".to_string()
            } else {
                all.join("\n")
            });
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
        // keyframe ticks for the selection
        if let Some(id) = self.selected() {
            let frames: Vec<f32> =
                t.tracks.iter().filter(|tr| tr.entity == id).flat_map(|tr| tr.keys.iter().map(|k| k.frame)).collect();
            if !frames.is_empty() {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), Sense::hover());
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 2.0, Color32::from_gray(38));
                let span = (t.end - t.start).max(1) as f32;
                for f in frames {
                    let x = rect.left() + (f - t.start as f32) / span * rect.width();
                    p.circle_filled(egui::pos2(x, rect.center().y), 3.5, ACCENT);
                }
                let x = rect.left() + (self.frame() - t.start as f32) / span * rect.width();
                p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], (1.5, Color32::WHITE));
            }
        }
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
                self.drag = if let (Some(h), Some((id, origin, ..))) = (handle_at(start), gizmo.as_ref()) {
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
                            if is_animated(&scene, &id) {
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
                        if is_animated(&scene, &id) {
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
                            if is_animated(&scene, &joint) {
                                p["keyframeAt"] = json!(f.round());
                            }
                            self.run_key(&key, "pose.aim", p);
                        }
                    }
                }
                Gesture::Scale { id, key } if resp.dragged() => {
                    let dd = resp.drag_delta();
                    let k = (1.0 + (dd.x - dd.y) * 0.006).clamp(0.5, 2.0);
                    if let Ok(t) = local_at(&scene, &id, f) {
                        let sc = (t.scale() * k).to_array();
                        let mut p = json!({"id": id, "scale": sc});
                        if is_animated(&scene, &id) {
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
            // rule-of-thirds guides
            for i in 1..3 {
                let x = rect.left() + rect.width() * i as f32 / 3.0;
                let y = rect.top() + rect.height() * i as f32 / 3.0;
                let s = (0.5, Color32::from_white_alpha(40));
                painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], s);
                painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], s);
            }
            painter.text(
                rect.left_top() + Vec2::new(8.0, 6.0),
                egui::Align2::LEFT_TOP,
                format!("{c} · frame {f:.0}"),
                egui::FontId::monospace(12.0),
                Color32::from_white_alpha(180),
            );
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

    /// Floating view options at the top-left of the viewport.
    fn toolbar(&mut self, ui: &egui::Ui, full: Rect, has_cam: bool) {
        egui::Area::new(egui::Id::unique("viewport_toolbar"))
            .order(egui::Order::Foreground)
            .fixed_pos(full.left_top() + Vec2::new(8.0, 8.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
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
