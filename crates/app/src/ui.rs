//! The egui editor: outliner, properties, timeline, command console and the 3D viewport.
//! Every edit goes through [`crate::hub::dispatch`]; the UI keeps only view state.

use egui::{Color32, Rect, Sense, Vec2};
use glam::{Mat4, Vec3, Vec4Swizzles};
use serde_json::{Value, json};
use shotstage_core::eval::{camera_data_at, camera_id_at, local_at, world_at};
use shotstage_core::{EntityKind, Modifier, Scene, TrackProperty};
use shotstage_render::{Overlay, Pass, Renderer, View, pick};

use crate::hub::{self, SharedHub};

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
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>, hub: SharedHub) -> Self {
        hub.borrow_mut().repaint = Some(cc.egui_ctx.clone());
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
        egui::Panel::left("outliner").resizable(true).default_size(210.0).show(ui, |ui| self.outliner(ui));
        egui::Panel::right("properties").resizable(true).default_size(280.0).show(ui, |ui| {
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
        let (undo, redo, del, space) = ui.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && !i.modifiers.shift && i.key_pressed(egui::Key::Z),
                cmd && (i.key_pressed(egui::Key::Y) || (i.modifiers.shift && i.key_pressed(egui::Key::Z))),
                i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::X),
                i.key_pressed(egui::Key::Space),
            )
        });
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
            let has_cam = camera_id_at(&self.scene(), self.frame()).is_some();
            ui.add_enabled_ui(has_cam, |ui| ui.toggle_value(&mut self.look_through, "Look through camera"));
            if !has_cam {
                self.look_through = false;
            }
        });
    }

    fn outliner(&mut self, ui: &mut egui::Ui) {
        ui.heading("Add");
        ui.horizontal_wrapped(|ui| {
            let prims: [(&str, Value); 6] = [
                ("Cube", json!({"shape": "cube", "size": [1, 1, 1]})),
                ("Sphere", json!({"shape": "sphere", "radius": 0.5, "segments": 24, "rings": 12})),
                ("Cylinder", json!({"shape": "cylinder", "radius": 0.5, "height": 1, "segments": 24})),
                ("Cone", json!({"shape": "cone", "radius": 0.5, "height": 1, "segments": 24})),
                ("Plane", json!({"shape": "plane", "size": [4, 4]})),
                ("Torus", json!({"shape": "torus", "majorRadius": 0.5, "minorRadius": 0.15, "majorSegments": 32, "minorSegments": 12})),
            ];
            for (label, p) in prims {
                if ui.button(label).clicked() {
                    let y = if label == "Plane" { 0.0 } else { 0.5 };
                    if let Some(r) = self.run("entity.create", json!({"type": "mesh", "primitive": p, "position": [0, y, 0]})) {
                        self.select(r["id"].as_str().map(str::to_string));
                    }
                }
            }
            if ui.button("Camera").clicked() {
                let eye = self.orbit.eye();
                if let Some(r) = self.run("entity.create", json!({"type": "camera", "position": eye.to_array()})) {
                    let id = r["id"].as_str().unwrap_or_default().to_string();
                    self.run("camera.lookAt", json!({"id": id, "target": self.orbit.target.to_array()}));
                    self.select(Some(id));
                }
            }
            if ui.button("Empty").clicked()
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
            if ui.button("Duplicate").clicked()
                && let Some(r) = self.run("entity.duplicate", json!({"id": id}))
            {
                self.select(r["id"].as_str().map(str::to_string));
            }
            if ui.button("Delete").clicked() {
                self.run("entity.delete", json!({"id": id}));
            }
        });

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
                    if c != *color {
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

        // Navigation (orbit view only) and picking.
        if cam.is_none() {
            if resp.dragged_by(egui::PointerButton::Primary) && !ui.input(|i| i.modifiers.shift) {
                let d = resp.drag_delta();
                self.orbit.yaw -= d.x * 0.008;
                self.orbit.pitch = (self.orbit.pitch + d.y * 0.008).clamp(-1.5, 1.5);
            }
            if resp.dragged_by(egui::PointerButton::Secondary)
                || resp.dragged_by(egui::PointerButton::Middle)
                || (resp.dragged_by(egui::PointerButton::Primary) && ui.input(|i| i.modifiers.shift))
            {
                let d = resp.drag_delta();
                let inv = self.orbit.view().inverse();
                let right = inv.x_axis.xyz();
                let up = inv.y_axis.xyz();
                let k = self.orbit.distance * 0.0015;
                self.orbit.target += (-right * d.x + up * d.y) * k;
            }
            if resp.hovered() {
                let s = ui.input(|i| i.smooth_scroll_delta.y);
                if s != 0.0 {
                    self.orbit.distance = (self.orbit.distance * (1.0 - s * 0.0015)).clamp(0.3, 500.0);
                }
            }
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && rect.contains(pos)
        {
            let ndc = glam::Vec2::new(
                (pos.x - rect.left()) / rect.width() * 2.0 - 1.0,
                1.0 - (pos.y - rect.top()) / rect.height() * 2.0,
            );
            let inv = (view.proj * view.view).inverse();
            let near = inv.project_point3(Vec3::new(ndc.x, ndc.y, 0.0));
            let far = inv.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
            self.select(pick(&scene, f, near, (far - near).normalize_or_zero()));
        }

        let ppp = ui.ctx().pixels_per_point();
        let px = ((rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32);
        // Looking through the shot camera shows the clean reference frame: no grid, no gizmos.
        let clean = cam.is_some();
        let overlay = Overlay {
            selected: if clean { None } else { self.selected() },
            grid: !clean,
            cameras: !clean,
            looking_through: cam.clone(),
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
        } else {
            painter.text(
                full.left_bottom() + Vec2::new(8.0, -8.0),
                egui::Align2::LEFT_BOTTOM,
                "drag: orbit · shift/right-drag: pan · scroll: zoom · click: select",
                egui::FontId::proportional(11.0),
                Color32::from_white_alpha(110),
            );
        }
    }
}
