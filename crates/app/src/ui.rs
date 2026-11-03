//! The egui editor: outliner, properties, timeline, command console and the 3D viewport.
//! Every edit goes through [`crate::hub::dispatch`]; the UI keeps only view state.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Vec2};
use glam::{Mat4, Vec3, Vec4Swizzles};
use serde_json::{Value, json};
use shotstage_core::eval::{camera_data_at, camera_id_at, local_at, world_at};
use shotstage_core::spatial::{self, RayOptions, raycast};
use shotstage_core::{EntityKind, Modifier, Scene, TrackProperty};
use shotstage_render::{Overlay, Pass, Renderer, View, pick};

use crate::hub::{self, SharedHub};

/// Something in the block palette.
#[derive(Clone)]
enum Block {
    Shape(&'static str, Value, [f32; 3]),
    Prefab(&'static str, &'static str),
}

impl Block {
    fn label(&self) -> &'static str {
        match self {
            Block::Shape(l, ..) | Block::Prefab(_, l) => l,
        }
    }
}

fn palette() -> (Vec<Block>, Vec<Block>) {
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
    let props = vec![
        Block::Prefab("person", "Person"),
        Block::Prefab("table", "Table"),
        Block::Prefab("chair", "Chair"),
        Block::Prefab("sofa", "Sofa"),
        Block::Prefab("bed", "Bed"),
        Block::Prefab("lamp", "Lamp"),
        Block::Prefab("crate", "Crate"),
        Block::Prefab("stairs", "Stairs"),
        Block::Prefab("wall", "Wall"),
        Block::Prefab("doorway", "Doorway"),
        Block::Prefab("car", "Car"),
        Block::Prefab("tree", "Tree"),
    ];
    (shapes, props)
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
    /// Gizmo: turn about the vertical axis.
    Ring {
        id: String,
        last: f32,
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
    Ring,
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
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>, hub: SharedHub) -> Self {
        hub.borrow_mut().repaint = Some(cc.egui_ctx.clone());
        hub.borrow_mut().gpu = cc.wgpu_render_state.clone();
        cc.egui_ctx.set_theme(egui::Theme::Dark);
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
            Block::Prefab(kind, _) => self.run_key(key, "prefab.create", json!({"kind": kind, "drop": false}))?,
        };
        r["id"].as_str().map(str::to_string)
    }

    /// Drop `id` where a viewport ray lands.
    fn place_on_ray(&self, key: &str, id: &str, ray: (Vec3, Vec3), offset: Vec3, ctrl: bool) {
        let scene = self.scene();
        let f = self.frame();
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
        self.hub.borrow_mut().selected = id;
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
        {
            let mut h = self.hub.borrow_mut();
            (h.pass, h.look_through) = (self.pass, self.look_through);
        }
        egui::Panel::bottom("timeline").resizable(true).default_size(190.0).show(ui, |ui| self.bottom(ui));
        egui::Panel::left("outliner")
            .resizable(true)
            .default_size(215.0)
            .min_size(215.0)
            .show(ui, |ui| self.outliner(ui));
        egui::Panel::right("properties").resizable(true).default_size(290.0).min_size(290.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.properties(ui));
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.viewport(ui, frame));
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
            ui.strong("shotstage");
            ui.separator();
            if ui.button("New").clicked() {
                self.run("scene.new", json!({}));
            }
            if ui.button("Undo").clicked() {
                self.run("history.undo", json!({}));
            }
            if ui.button("Redo").clicked() {
                self.run("history.redo", json!({}));
            }
            ui.separator();
            ui.label("Pass");
            ui.selectable_value(&mut self.pass, Pass::Clay, "Clay");
            ui.selectable_value(&mut self.pass, Pass::Depth, "Depth");
            ui.selectable_value(&mut self.pass, Pass::Normal, "Normal");
            ui.separator();
            ui.toggle_value(&mut self.snap, "Snap")
                .on_hover_text("Snap dragged blocks to a grid (hold Ctrl to place freely)");
            ui.add_enabled(
                self.snap,
                egui::DragValue::new(&mut self.snap_step).range(0.01..=5.0).speed(0.01).suffix(" m"),
            );
            ui.separator();
            let has_cam = camera_id_at(&self.scene(), self.frame()).is_some();
            ui.add_enabled_ui(has_cam, |ui| ui.toggle_value(&mut self.look_through, "Look through camera"));
            if !has_cam {
                self.look_through = false;
            }
        });
    }

    fn outliner(&mut self, ui: &mut egui::Ui) {
        ui.heading("Blocks");
        ui.weak("Drag into the scene, or click to drop in the middle.");
        let (shapes, props) = palette();
        for (title, blocks) in [("Props (real size)", props), ("Shapes", shapes)] {
            ui.label(title);
            egui::Grid::new(title).num_columns(3).spacing([4.0, 4.0]).show(ui, |ui| {
                for (i, b) in blocks.iter().enumerate() {
                    let r = ui
                        .add(
                            egui::Button::new(b.label()).min_size(Vec2::new(62.0, 30.0)).sense(Sense::click_and_drag()),
                        )
                        .on_hover_cursor(CursorIcon::Grab);
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
                    if i % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.button("+ Camera").clicked() {
                let eye = self.orbit.eye();
                if let Some(r) = self.run("entity.create", json!({"type": "camera", "position": eye.to_array()})) {
                    let id = r["id"].as_str().unwrap_or_default().to_string();
                    self.run("camera.lookAt", json!({"id": id, "target": self.orbit.target.to_array()}));
                    self.select(Some(id));
                }
            }
            if ui.button("+ Empty").clicked()
                && let Some(r) = self.run("entity.create", json!({"type": "empty"}))
            {
                self.select(r["id"].as_str().map(str::to_string));
            }
        });
        ui.separator();
        ui.heading("Scene");
        let scene = self.scene();
        let selected = self.selected();
        egui::ScrollArea::vertical().show(ui, |ui| {
            fn tree(
                ui: &mut egui::Ui,
                scene: &Scene,
                parent: Option<&str>,
                depth: usize,
                selected: &Option<String>,
                picked: &mut Option<Option<String>>,
            ) {
                for e in scene.children(parent) {
                    ui.horizontal(|ui| {
                        ui.add_space(depth as f32 * 12.0);
                        let icon = match e.kind {
                            EntityKind::Mesh { .. } => "■",
                            EntityKind::Camera(_) => "◉",
                            EntityKind::Empty => "✚",
                        };
                        let active = if scene.active_camera.as_deref() == Some(&e.id) { " ★" } else { "" };
                        let text = format!("{icon} {}{active}", e.name);
                        let text = if e.visible { egui::RichText::new(text) } else { egui::RichText::new(text).weak() };
                        if ui.selectable_label(selected.as_deref() == Some(&e.id), text).clicked() {
                            *picked = Some(Some(e.id.clone()));
                        }
                    });
                    tree(ui, scene, Some(&e.id), depth + 1, selected, picked);
                }
            }
            let mut picked = None;
            tree(ui, &scene, None, 0, &selected, &mut picked);
            if let Some(p) = picked {
                self.select(p);
            }
            if scene.entities.is_empty() {
                ui.weak("Empty scene. Add something above, or drive it from the console / API.");
            }
        });
    }

    fn properties(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected() else {
            ui.weak("Select an entity.");
            return;
        };
        let scene = self.scene();
        let Ok(e) = scene.get(&id) else { return };
        let frame = self.frame();
        ui.heading(&e.name);
        ui.weak(format!("id: {}   type: {}", e.id, e.kind.type_name()));
        ui.horizontal(|ui| {
            let mut visible = e.visible;
            if ui.checkbox(&mut visible, "Visible").changed() {
                self.run("entity.setVisible", json!({"id": id, "visible": visible}));
            }
            let mut locked = e.locked;
            if ui.checkbox(&mut locked, "Locked").on_hover_text("Drags in the viewport pass through it").changed() {
                self.run("entity.setLocked", json!({"id": id, "locked": locked}));
            }
            if ui.button("Duplicate").clicked()
                && let Some(r) = self.run("entity.duplicate", json!({"id": id}))
            {
                self.select(r["id"].as_str().map(str::to_string));
            }
            if ui.button("Delete").clicked() {
                self.run("entity.delete", json!({"id": id}));
            }
        });

        if !matches!(e.kind, EntityKind::Camera(_)) {
            ui.separator();
            ui.strong("Place");
            ui.horizontal_wrapped(|ui| {
                if ui.button("Drop to surface").on_hover_text("entity.place").clicked() {
                    self.run("entity.place", json!({"id": id, "frame": frame}));
                }
                if ui.button("↺ 45°").clicked() {
                    self.run("entity.rotate", json!({"id": id, "degrees": 45, "frame": frame}));
                }
                if ui.button("↻ 45°").clicked() {
                    self.run("entity.rotate", json!({"id": id, "degrees": -45, "frame": frame}));
                }
                if let Some(cam) = camera_id_at(&scene, frame)
                    && ui.button("Face camera").on_hover_text("entity.face").clicked()
                {
                    self.run("entity.face", json!({"id": id, "target": cam, "frame": frame}));
                }
            });

            ui.separator();
            ui.strong("Frame it");
            ui.weak("Moves the active camera (or adds one) and looks through it.");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("frame_angle").selected_text(self.frame_angle).show_ui(ui, |ui| {
                    for a in ["eyeLevel", "high", "low", "birdsEye", "wormsEye"] {
                        ui.selectable_value(&mut self.frame_angle, a, a);
                    }
                });
                egui::ComboBox::from_id_salt("frame_side").selected_text(self.frame_side).show_ui(ui, |ui| {
                    for a in ["front", "frontLeft", "left", "backLeft", "back", "backRight", "right", "frontRight"] {
                        ui.selectable_value(&mut self.frame_side, a, a);
                    }
                });
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
        }

        ui.separator();
        ui.strong("Transform");
        let animated = scene.timeline.track(&id, TrackProperty::Transform).is_some();
        let t = local_at(&scene, &id, frame).unwrap_or(e.transform);
        let mut pos = t.translation;
        let mut rot = t.euler_degrees();
        let mut scale = t.scale;
        let mut changed = false;
        let mut started = false;
        let mut row = |ui: &mut egui::Ui, label: &str, v: &mut [f32; 3], speed: f64| {
            ui.horizontal(|ui| {
                ui.label(format!("{label:<9}"));
                for x in v.iter_mut() {
                    let r = ui.add(egui::DragValue::new(x).speed(speed).max_decimals(3));
                    changed |= r.changed();
                    started |= r.drag_started() || r.gained_focus();
                }
            });
        };
        row(ui, "Position", &mut pos, 0.02);
        row(ui, "Rotation", &mut rot, 0.5);
        row(ui, "Scale", &mut scale, 0.01);
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
            if ui.button(format!("Key transform @ {frame:.0}")).clicked() {
                self.run("entity.setTransform", json!({"id": id, "position": pos, "rotationEuler": rot, "scale": scale, "keyframeAt": frame.round()}));
            }
            if animated && ui.button("Delete key").clicked() {
                self.run("keyframe.delete", json!({"id": id, "property": "transform", "frame": frame.round()}));
            }
        });
        if animated {
            ui.weak("Animated: edits are keyed at the current frame.");
        }

        match &e.kind {
            EntityKind::Mesh { modifiers, color, .. } => {
                ui.separator();
                ui.strong("Mesh");
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
                        "{} verts · {} edges · {} faces · {} tris",
                        stats["vertices"], stats["edges"], stats["faces"], stats["triangles"]
                    ));
                }
                ui.label("Modifiers");
                for (i, m) in modifiers.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let label = match m {
                            Modifier::Mirror { axis } => format!("Mirror {}", ["X", "Y", "Z"][(*axis).min(2) as usize]),
                            Modifier::Array { count, offset } => format!("Array ×{count} {offset:?}"),
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
            }
            EntityKind::Camera(_) => {
                ui.separator();
                ui.strong("Camera");
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
                        self.run("keyframe.set", json!({"id": id, "property": "focalLength", "frame": frame.round()}));
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
            }
            EntityKind::Empty => {}
        }
    }

    fn bottom(&mut self, ui: &mut egui::Ui) {
        let scene = self.scene();
        let t = scene.timeline.clone();
        ui.horizontal(|ui| {
            if ui.button(if self.playing { "⏸ Pause" } else { "▶ Play" }).clicked() {
                self.playing = !self.playing;
            }
            let mut f = self.frame();
            ui.add(egui::Slider::new(&mut f, t.start as f32..=t.end as f32).step_by(1.0).text("frame"));
            if f != self.frame() {
                self.hub.borrow_mut().frame = f;
            }
            ui.separator();
            let (mut fps, mut start, mut end) = (t.fps, t.start, t.end);
            ui.label("fps");
            let a = ui.add(egui::DragValue::new(&mut fps).range(1.0..=120.0));
            ui.label("range");
            let b = ui.add(egui::DragValue::new(&mut start).range(0..=end));
            let c = ui.add(egui::DragValue::new(&mut end).range(start..=100_000));
            if a.changed() || b.changed() || c.changed() {
                self.run("timeline.set", json!({"fps": fps, "start": start, "end": end}));
            }
            ui.weak(format!("{:.2} s", t.duration_seconds()));
        });
        // keyframe ticks for the selection
        if let Some(id) = self.selected() {
            let frames: Vec<f32> =
                t.tracks.iter().filter(|tr| tr.entity == id).flat_map(|tr| tr.keys.iter().map(|k| k.frame)).collect();
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 2.0, Color32::from_gray(40));
            let span = (t.end - t.start).max(1) as f32;
            for f in frames {
                let x = rect.left() + (f - t.start as f32) / span * rect.width();
                p.circle_filled(egui::pos2(x, rect.center().y), 4.0, Color32::from_rgb(240, 180, 60));
            }
            let x = rect.left() + (self.frame() - t.start as f32) / span * rect.width();
            p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], (1.5, Color32::WHITE));
        }
        ui.horizontal(|ui| {
            ui.label(format!("Shots: {}", t.shots.len()));
            for s in &t.shots {
                ui.weak(format!("[{} {}–{} via {}]", s.name, s.start, s.end, s.camera));
            }
            if let Some(cam) = scene.active_camera.clone()
                && ui.button("+ Shot (active camera, full range)").clicked()
            {
                self.run("shot.create", json!({"camera": cam, "start": t.start, "end": t.end}));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong("Command console");
            ui.weak("same API as window.shotstage / CLI / MCP");
            if ui.button("List commands").clicked() {
                let list = self.hub.borrow().session.list();
                self.console_out =
                    list.iter().map(|c| format!("{:<24} {}", c.id, c.summary)).collect::<Vec<_>>().join("\n");
            }
        });
        ui.columns(2, |cols| {
            cols[0].add(
                egui::TextEdit::multiline(&mut self.console).code_editor().desired_rows(4).desired_width(f32::INFINITY),
            );
            if cols[0].button("Run (JSON request)").clicked() {
                self.console_out = match serde_json::from_str::<Value>(&self.console) {
                    Ok(req) => match hub::dispatch(&self.hub, req) {
                        Ok(o) => serde_json::to_string_pretty(&o.result).unwrap_or_default(),
                        Err(e) => format!("error {}: {}", e.code, e.message),
                    },
                    Err(e) => format!("not JSON: {e}"),
                };
            }
            egui::ScrollArea::vertical().id_salt("console_out").show(&mut cols[1], |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&self.console_out).monospace()).wrap());
            });
        });
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
        let gizmo = selected.as_ref().filter(|s| !clean && !scene.get(s).is_ok_and(|e| e.locked)).and_then(|id| {
            let o = world_at(&scene, id, f).ok()?.w_axis.truncate();
            let len = (eye - o).length().max(0.1) * 0.14;
            let c = project(rect, &view, o)?;
            let axes: Vec<Option<Pos2>> =
                [Vec3::X, Vec3::Y, Vec3::Z].iter().map(|a| project(rect, &view, o + *a * len)).collect();
            let ring: Vec<Pos2> = (0..=48)
                .filter_map(|i| {
                    let t = i as f32 / 48.0 * std::f32::consts::TAU;
                    project(rect, &view, o + Vec3::new(t.cos(), 0.0, t.sin()) * len * 0.8)
                })
                .collect();
            Some((id.clone(), o, len, c, axes, ring))
        });
        let handle_at = |p: Pos2| -> Option<Handle> {
            let (_, _, _, c, axes, ring) = gizmo.as_ref()?;
            if (p - (*c + Vec2::new(30.0, -30.0))).length() < 8.0 {
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
            ring.windows(2).any(|w| seg_dist(p, w[0], w[1]) < 5.0).then_some(Handle::Ring)
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
                Some(if in_group { hit } else { root })
            });
        }

        // 4. Drags in the viewport.
        if self.palette.is_none() {
            let primary = resp.drag_started_by(egui::PointerButton::Primary);
            if primary && let Some(start) = ui.input(|i| i.pointer.press_origin()).filter(|p| rect.contains(*p)) {
                let (o, d) = ray_at(rect, &view, start);
                self.drag = if let (Some(h), Some((id, origin, ..))) = (handle_at(start), gizmo.as_ref()) {
                    let key = self.new_key("gizmo");
                    match h {
                        Handle::Axis(i) => {
                            let axis = [Vec3::X, Vec3::Y, Vec3::Z][i];
                            let t0 = closest_on_line(*origin, axis, o, d).unwrap_or(0.0);
                            Gesture::Axis { id: id.clone(), axis, start: *origin, t0, key }
                        }
                        Handle::Ring => {
                            let c = gizmo.as_ref().map(|g| g.3).unwrap_or(start);
                            Gesture::Ring { id: id.clone(), last: (-(start.y - c.y)).atan2(start.x - c.x), key }
                        }
                        Handle::Scale => Gesture::Scale { id: id.clone(), key },
                    }
                } else if let Some(hit) = pick_grabbable(&scene, f, o, d) {
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
                Gesture::Ring { id, last, key } if resp.dragged() => {
                    if let Some((.., c, _, _)) = gizmo.as_ref() {
                        let a = (-(ptr.y - c.y)).atan2(ptr.x - c.x);
                        let mut delta = (a - last).to_degrees();
                        if delta > 180.0 {
                            delta -= 360.0;
                        } else if delta < -180.0 {
                            delta += 360.0;
                        }
                        // seen from below, screen rotation is mirrored
                        let sign = if eye.y >= gizmo.as_ref().map(|g| g.1.y).unwrap_or(0.0) { 1.0 } else { -1.0 };
                        self.run_key(&key, "entity.rotate", json!({"id": id, "degrees": delta * sign, "frame": f}));
                        self.drag = Gesture::Ring { id, last: a, key };
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
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos().filter(|p| rect.contains(*p))
        {
            let (o, d) = ray_at(rect, &view, pos);
            let sel = pick(&scene, f, o, d).map(|hit| {
                let root = root_of(&scene, &hit);
                // first click takes the whole group; clicking inside a selected group picks the part
                match &selected {
                    Some(s) if spatial::subtree(&scene, &root).contains(s) => hit,
                    _ => root,
                }
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
        if let Some((_, _, _, c, axes, ring)) = &gizmo {
            let hot = self.hot_handle;
            let active = match &self.drag {
                Gesture::Axis { axis, .. } => Some(Handle::Axis(axis.abs().max_position())),
                Gesture::Ring { .. } => Some(Handle::Ring),
                Gesture::Scale { .. } => Some(Handle::Scale),
                _ => None,
            };
            let lit = |h: Handle| hot == Some(h) || active == Some(h);
            let ring_c = if lit(Handle::Ring) {
                Color32::from_rgb(255, 230, 120)
            } else {
                Color32::from_rgba_unmultiplied(230, 200, 90, 170)
            };
            painter.add(egui::Shape::line(ring.clone(), (if lit(Handle::Ring) { 3.0 } else { 1.5 }, ring_c)));
            let colors =
                [Color32::from_rgb(235, 80, 80), Color32::from_rgb(110, 210, 90), Color32::from_rgb(80, 140, 255)];
            for (i, a) in axes.iter().enumerate() {
                if let Some(a) = a {
                    let w = if lit(Handle::Axis(i)) { 4.0 } else { 2.5 };
                    let col = if lit(Handle::Axis(i)) { Color32::WHITE } else { colors[i] };
                    painter.line_segment([*c, *a], (w, col));
                    painter.circle_filled(*a, 5.0, col);
                }
            }
            let sq = Rect::from_center_size(*c + Vec2::new(30.0, -30.0), Vec2::splat(10.0));
            painter.rect_filled(sq, 1.0, if lit(Handle::Scale) { Color32::WHITE } else { Color32::from_gray(200) });
        }
        if !clean {
            painter.text(
                full.left_bottom() + Vec2::new(8.0, -8.0),
                egui::Align2::LEFT_BOTTOM,
                "drag block in · drag object: slide on surfaces · Shift: lift · Alt: copy · wheel while dragging / R: turn · Ctrl: no snap · Esc: cancel · empty drag: orbit · right-drag: pan · scroll: zoom",
                egui::FontId::proportional(11.0),
                Color32::from_white_alpha(110),
            );
        }
    }
}
