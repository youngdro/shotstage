//! The egui editor: outliner, properties, timeline, command console and the 3D viewport.
//! Every edit goes through [`crate::hub::dispatch`]; the UI keeps only view state.

use crate::i18n::tr;
use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Vec2};
use glam::{Mat4, Vec3, Vec4Swizzles};
use serde_json::{Value, json};
use shotstage_commands::{humans, prefabs, rig};
use shotstage_core::eval::{camera_data_at, camera_id_at, local_at, world_at};
use shotstage_core::spatial::{self, RayOptions, raycast};
use shotstage_core::{EntityKind, Modifier, Scene, TrackProperty};
use shotstage_render::{Overlay, Pass, Renderer, View, pick};

use crate::hub::{self, SharedHub};

#[path = "editmode.rs"]
mod editmode;
#[path = "material.rs"]
mod material;
#[path = "uveditor.rs"]
mod uveditor;

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
            // either language finds it
            crate::i18n::both(self.label()).to_lowercase().contains(filter)
                || key.to_lowercase().contains(filter)
                || crate::i18n::both(self.description()).to_lowercase().contains(filter)
        }
    }
}

/// What a scene-tree row asked for this frame.
enum TreeAct {
    /// id, add to the selection (Shift)
    Pick(String, bool),
    Visible(String, bool),
    Locked(String, bool),
    Rename(String, String),
    Focus(String),
    Duplicate(String),
    Delete(String),
}

struct TreeCtx<'a> {
    scene: &'a Scene,
    selected: &'a Option<String>,
    also: &'a [String],
    /// (id, name being typed)
    renaming: &'a mut Option<(String, String)>,
    acts: Vec<TreeAct>,
}

fn tree_level(ui: &mut egui::Ui, cx: &mut TreeCtx, parent: Option<&str>) {
    let scene = cx.scene;
    for e in scene.children(parent) {
        if scene.children(Some(&e.id)).next().is_none() {
            tree_row(ui, cx, e);
            continue;
        }
        // groups start folded; open the ones holding the selection
        let id = ui.make_persistent_id(("tree", &e.id));
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        if cx.selected.as_ref().is_some_and(|s| s != &e.id && spatial::subtree(scene, &e.id).contains(s)) {
            state.set_open(true);
        }
        state.show_header(ui, |ui| tree_row(ui, cx, e)).body(|ui| tree_level(ui, cx, Some(&e.id)));
    }
}

/// One row: icon and name (click selects, Shift+click adds, double-click renames, right-click
/// for more), with show/lock toggles on the right (bright when hidden or locked).
fn tree_row(ui: &mut egui::Ui, cx: &mut TreeCtx, e: &shotstage_core::Entity) {
    let scene = cx.scene;
    let joint = rig::is_joint(scene, &e.id);
    let icon = match e.kind {
        EntityKind::Mesh { .. } => "■",
        EntityKind::Camera(_) => "◉",
        EntityKind::Light(_) => "☀",
        EntityKind::Image(_) => "▨",
        EntityKind::Empty if joint => "•",
        EntityKind::Empty => "▣",
    };
    if let Some((rid, buf)) = cx.renaming.as_mut()
        && *rid == e.id
    {
        let r = ui.add(egui::TextEdit::singleline(buf).desired_width(ui.available_width() - 8.0));
        if r.lost_focus() {
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cx.acts.push(TreeAct::Rename(e.id.clone(), buf.clone()));
            }
            *cx.renaming = None;
        } else if !r.has_focus() {
            r.request_focus();
        }
        return;
    }
    let on = cx.selected.as_deref() == Some(&e.id) || cx.also.contains(&e.id);
    let active = if scene.active_camera.as_deref() == Some(&e.id) { " ★" } else { "" };
    let text = format!("{icon} {}{active}", e.name);
    let text = if e.visible { egui::RichText::new(text) } else { egui::RichText::new(text).weak() };
    ui.horizontal(|ui| {
        // toggles on the right, so the name gets what is left
        let n = if joint { 1.0 } else { 2.0 };
        let toggle_w = n * (18.0 + ui.spacing().item_spacing.x) + 6.0;
        let r = ui
            .allocate_ui_with_layout(
                Vec2::new((ui.available_width() - toggle_w).max(40.0), 18.0),
                egui::Layout::top_down_justified(egui::Align::LEFT),
                |ui| ui.add(egui::Button::selectable(on, text).truncate()),
            )
            .inner;
        if r.clicked() {
            cx.acts.push(TreeAct::Pick(e.id.clone(), ui.input(|i| i.modifiers.shift)));
        }
        if r.double_clicked() && !joint {
            *cx.renaming = Some((e.id.clone(), e.name.clone()));
        }
        r.context_menu(|ui| {
            if ui.button(tr("Focus")).clicked() {
                cx.acts.push(TreeAct::Focus(e.id.clone()));
                ui.close();
            }
            if !joint {
                if ui.button(tr("Rename")).clicked() {
                    *cx.renaming = Some((e.id.clone(), e.name.clone()));
                    ui.close();
                }
                if ui.button(tr("Duplicate")).clicked() {
                    cx.acts.push(TreeAct::Duplicate(e.id.clone()));
                    ui.close();
                }
            }
            if ui.button(tr(if e.visible { "Hide" } else { "Show" })).clicked() {
                cx.acts.push(TreeAct::Visible(e.id.clone(), !e.visible));
                ui.close();
            }
            if !joint {
                if ui.button(tr(if e.locked { "Unlock" } else { "Lock" })).clicked() {
                    cx.acts.push(TreeAct::Locked(e.id.clone(), !e.locked));
                    ui.close();
                }
                ui.separator();
                if ui.button(tr("Delete")).clicked() {
                    cx.acts.push(TreeAct::Delete(e.id.clone()));
                    ui.close();
                }
            }
        });
        {
            let small = |s: &str, active: bool| {
                let t = egui::RichText::new(tr(s)).size(12.0);
                egui::Button::new(if active { t } else { t.weak() }).frame(false).min_size(Vec2::new(18.0, 18.0))
            };
            if ui
                .add(small(if e.visible { "👁" } else { "⊘" }, !e.visible))
                .on_hover_text(tr(if e.visible { "Hide" } else { "Show" }))
                .clicked()
            {
                cx.acts.push(TreeAct::Visible(e.id.clone(), !e.visible));
            }
            if !joint
                && ui
                    .add(small(if e.locked { "🔒" } else { "🔓" }, e.locked))
                    .on_hover_text(tr(if e.locked {
                        "Unlock (drags pass through locked objects)"
                    } else {
                        "Lock: drags pass through it"
                    }))
                    .clicked()
            {
                cx.acts.push(TreeAct::Locked(e.id.clone(), !e.locked));
            }
        }
    });
}

#[derive(Clone, Copy, PartialEq)]
enum ScrollMode {
    /// Trackpad: orbit; mouse wheel: zoom.
    Auto,
    Zoom,
    Orbit,
}

/// Whether this frame's wheel input came from a trackpad (None: no wheel input). Native macOS
/// says so; the web does not, so there fingers are told apart from a wheel by their sideways or
/// fractional movement.
fn wheel_from_trackpad(events: &[egui::Event]) -> Option<bool> {
    let mut out = None;
    for e in events {
        if let egui::Event::MouseWheel { unit, delta, source, .. } = e {
            let t = match source {
                egui::MouseWheelSource::Trackpad | egui::MouseWheelSource::Momentum => Some(true),
                egui::MouseWheelSource::Wheel => Some(false),
                _ if *unit != egui::MouseWheelUnit::Point => Some(false),
                _ if delta.x != 0.0 || delta.y.fract() != 0.0 => Some(true),
                // a big, whole, vertical step is a wheel notch; small ones keep the last guess
                _ if delta.y.abs() >= 50.0 => Some(false),
                _ => None,
            };
            out = t.or(out);
        }
    }
    out
}

/// Offscreen renderer and texture of the shot-camera preview; `key` says what it last showed.
struct Pip {
    renderer: Renderer,
    tex: Option<(egui::TextureId, u64)>,
    key: Option<String>,
    at: f64,
    /// Where it was drawn last frame (None when hidden).
    rect: Option<Rect>,
}

impl Pip {
    fn new(device: &wgpu::Device) -> Self {
        Self { renderer: Renderer::new(device), tex: None, rect: None, key: None, at: 0.0 }
    }

    /// Render `camera` at `frame` when that picture changed (at most ~20 times a second);
    /// the texture to show, once there is one. `look`: pass, studio light, shadows, scene
    /// revision, time now.
    fn update(
        &mut self,
        rs: &eframe::egui_wgpu::RenderState,
        scene: &Scene,
        camera: &str,
        frame: f32,
        px: Vec2,
        (pass, studio_light, shadows, revision, now): (Pass, bool, bool, u64, f64),
    ) -> Option<egui::TextureId> {
        let ppx = (px.x.max(1.0) as u32, px.y.max(1.0) as u32);
        let key = format!("{revision}|{frame}|{camera}|{:?}|{ppx:?}|{studio_light}|{shadows}", pass as u32);
        if self.key.as_deref() != Some(key.as_str()) && (now - self.at >= 0.05 || self.key.is_none()) {
            let (Ok(lens), Ok(world)) = (camera_data_at(scene, camera, frame), world_at(scene, camera, frame)) else {
                return None;
            };
            let a = lens.aspect.clamp(0.3, 4.0);
            let view = View { view: world.inverse(), proj: lens.projection(a), depth: None };
            let mut ov = shotstage_render::clean_overlay(camera);
            ov.studio_light = studio_light;
            ov.no_shadows = !shadows;
            self.renderer.render(&rs.device, &rs.queue, scene, frame, view, pass, &ov, ppx);
            self.key = Some(key);
            self.at = now;
            let generation = self.renderer.target_generation;
            if self.tex.is_none_or(|(_, g)| g != generation)
                && let Some(v) = self.renderer.color_view()
            {
                let mut egui_renderer = rs.renderer.write();
                if let Some((old, _)) = self.tex.take() {
                    egui_renderer.free_texture(&old);
                }
                self.tex =
                    Some((egui_renderer.register_native_texture(&rs.device, v, wgpu::FilterMode::Linear), generation));
            }
        }
        self.tex.map(|(t, _)| t)
    }
}

/// A small monitor over the view: a picture with a label, clickable.
fn monitor(
    ctx: &egui::Context,
    id: &str,
    tex: egui::TextureId,
    rect: Rect,
    label: &str,
    accent: Color32,
) -> egui::Response {
    egui::Area::new(egui::Id::unique(id))
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            let (r, mut p) = ui.allocate_painter(rect.size(), Sense::click());
            p.set_clip_rect(r.rect.expand(2.0));
            p.image(tex, r.rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            let stroke = if r.hovered() { (1.5, accent) } else { (1.0, Color32::from_white_alpha(60)) };
            p.rect_stroke(r.rect, 3.0, stroke, egui::StrokeKind::Outside);
            let g = p.layout_no_wrap(label.to_string(), egui::FontId::proportional(11.0), Color32::WHITE);
            let lr = Rect::from_min_size(r.rect.min + Vec2::new(4.0, 4.0), g.size() + Vec2::new(8.0, 4.0));
            p.rect_filled(lr, 3.0, Color32::from_black_alpha(140));
            p.galley(lr.min + Vec2::new(4.0, 2.0), g, Color32::WHITE);
            r.on_hover_cursor(CursorIcon::PointingHand)
        })
        .inner
}

/// The frames where the picture switches to another shot (each later shot's first frame), in
/// order.
fn cuts(scene: &Scene) -> Vec<(f32, shotstage_core::Shot)> {
    let mut shots = scene.timeline.shots.clone();
    shots.sort_by_key(|s| s.start);
    let mut out = Vec::new();
    for w in shots.windows(2) {
        if w[1].start > scene.timeline.start {
            out.push((w[1].start as f32, w[1].clone()));
        }
    }
    out
}

/// The first cut after `frame`.
fn next_cut(scene: &Scene, frame: f32) -> Option<(f32, shotstage_core::Shot)> {
    cuts(scene).into_iter().find(|(f, _)| *f > frame + 0.01)
}

/// A stretch being played on its own (a cut preview).
#[derive(Clone, Copy)]
struct PlayRange {
    end: f32,
    /// Look through the cameras only for this preview: back to the free view after it.
    restore_free_view: bool,
}

/// One entry of the quick-actions box.
struct QuickItem {
    label: String,
    kind: &'static str,
    hint: String,
    run: QuickRun,
}

enum QuickRun {
    Block(Block),
    Template(&'static str, &'static str),
    /// preset, target, subject
    Motion(&'static shotstage_commands::motion::Preset, String, Option<String>),
    /// person root, pose
    Pose(String, &'static str),
    /// a shot's first frame
    Shot(u32),
    LookThrough(String),
    Select(String),
    Action(QuickAction),
}

#[derive(Clone, Copy)]
enum QuickAction {
    Save,
    Open,
    Undo,
    Redo,
    New,
    Shadows,
    Focus,
    Key,
    Duplicate,
    Delete,
    CameraHere,
    SaveFrame,
    ExportBoard,
    ExportSheet,
    Play,
    Start,
    CameraView,
    Board,
    Preview,
    FitTimeline,
    SafeAreas,
}

/// `holdBasket` → `Hold basket`, `tPose` → `T pose`.
fn humanize(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else if c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Order quick items for a query: label starts with it, then a word in the label does, then the
/// label contains it, then the kind or hint does. Every word of the query must match somewhere.
fn rank_quick(items: &[QuickItem], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut scored: Vec<(u8, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| {
            // scored on the label as shown; found by either language's wording
            let label = tr(&it.label).to_lowercase();
            let all = format!(
                "{label} {} {} {}",
                crate::i18n::both(&it.label).to_lowercase(),
                crate::i18n::both(it.kind).to_lowercase(),
                crate::i18n::both(&it.hint).to_lowercase()
            );
            if !words.iter().all(|w| all.contains(w)) {
                return None;
            }
            let score = if q.is_empty() {
                // nothing typed: what fits the selection first, blocks and templates last
                match it.kind {
                    "Pose" | "Motion" | "Camera move" => 1,
                    "Action" => 2,
                    "Shot" | "Camera" => 3,
                    _ => 4,
                }
            } else if label.starts_with(&q) {
                0
            } else if label.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(words[0])) {
                1
            } else if label.contains(&q) {
                2
            } else {
                3
            };
            Some((score, i))
        })
        .collect();
    scored.sort_by_key(|&(s, i)| (s, i));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Poses shown as buttons in the person panel; the rest are under "More poses".
const COMMON_POSES: &[&str] = &[
    "stand",
    "relaxed",
    "walk",
    "run",
    "sit",
    "crouch",
    "wave",
    "point",
    "reach",
    "pickUp",
    "phone",
    "armsCrossed",
    "handsOnHips",
    "thinking",
    "browse",
    "carryBag",
];

/// The one accent colour of the UI.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(70, 150, 255);

const HELP: &str = "Quick actions: Cmd/Ctrl+K or / opens a search box for every block, move, pose, template and action.\n\
Blocks: drag from the left panel into the view (or click to drop in the middle).\n\
Object: click to select (a second click picks a part); drag to slide it over surfaces.\n\
Shift+drag: lift · Alt+drag: copy · R / Shift+R or wheel while dragging: turn · Ctrl: no snap · Esc: cancel.\n\
People: click a body part to select its joint, then drag the part to bend it, or turn the coloured rings.\n\
View: drag empty space to orbit · right-drag or Shift+drag to pan · pinch or mouse wheel to zoom · trackpad two-finger scroll orbits (Shift: pan) · double-click or F to focus.\n\
Keys: Cmd/Ctrl+Z undo · Cmd/Ctrl+Shift+Z redo · Cmd/Ctrl+C / X / V copy, cut, paste (also between scenes) · Cmd/Ctrl+D duplicate · Delete · Space play · K key · F focus · Cmd/Ctrl+S save.\n\
Several objects: Shift+click to add · Cmd/Ctrl+drag on empty space to box-select · Cmd/Ctrl+G group · Cmd/Ctrl+Shift+G ungroup.";

/// A collapsible properties section.
fn section(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(egui::RichText::new(tr(title)).strong())
        .id_salt(title)
        .default_open(open)
        .show(ui, add);
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

/// A view-cube axis on screen: (depth, position, label, colour, (yaw, pitch) to look along it).
type CubeAxis<'a> = (f32, Pos2, &'a str, Color32, (f32, f32));

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
        "keyframe.retime" => "Retime animation".into(),
        "entity.paste" => "Paste".into(),
        "shot.cut" => "Cut".into(),
        "keyframe.delete" => "Delete key".into(),
        "camera.frame" => "Frame shot".into(),
        "model.import" => "Import model".into(),
        "image.import" => "Add picture".into(),
        "light.preset" => "Lighting preset".into(),
        "scene.template" => "Template".into(),
        "keyframe.setInterp" => "Easing".into(),
        "entity.rename" => "Rename".into(),
        "entity.setVisible" => "Show / hide".into(),
        "entity.setLocked" => "Lock / unlock".into(),
        "entity.group" => "Group".into(),
        "entity.ungroup" => "Ungroup".into(),
        "entity.align" | "entity.distribute" => "Arrange".into(),
        "entity.attach" => "Hold in hand".into(),
        "camera.lookAt" | "camera.setLens" | "camera.setActive" => "Camera".into(),
        "shot.create" => "Add shot".into(),
        "shot.set" => "Edit shot".into(),
        "shot.delete" => "Delete shot".into(),
        "light.set" => "Light".into(),
        "light.shadows" => "Shadows".into(),
        "light.fromEnvironment" => "Sun from sky".into(),
        "environment.set" => "Sky".into(),
        "environment.import" => "Import HDRI".into(),
        "scene.load" => "Open scene".into(),
        "scene.new" => "New scene".into(),
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
        Block::Shape(
            "Vase",
            json!({"shape": "lathe", "segments": 32, "profile": [[0, 0], [0.07, 0], [0.11, 0.08], [0.12, 0.16], [0.07, 0.27], [0.045, 0.32], [0.06, 0.36]]}),
            grey,
        ),
        Block::Shape(
            "Bowl",
            json!({"shape": "lathe", "segments": 32, "profile": [[0, 0], [0.05, 0], [0.06, 0.01], [0.13, 0.05], [0.16, 0.09], [0.15, 0.09], [0.12, 0.055], [0.0, 0.02]]}),
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
        ("Fresh produce", true, of(Category::Fresh)),
        ("Product shoot", true, of(Category::Product)),
        ("Furniture & set", false, of(Category::Furniture)),
        (
            "Lights",
            false,
            vec![
                Block::Light("sun", "Sun", "Parallel sunlight; aim it to set the time of day"),
                Block::Light("point", "Point light", "A bare bulb: light in every direction"),
                Block::Light("spot", "Spot", "A cone of light (stage light, rim light)"),
                Block::Light("area", "Area", "A soft panel: softbox, window"),
                Block::LightPreset(
                    "threePoint",
                    "3-point",
                    "Key, fill and rim around the selection (or the whole scene)",
                ),
                Block::LightPreset("loop", "Loop", "Everyday portrait: key a little to the side, soft fill"),
                Block::LightPreset("rembrandt", "Rembrandt", "One high side key light, moody"),
                Block::LightPreset("butterfly", "Butterfly", "Key high in front, soft fill from below"),
                Block::LightPreset("clamshell", "Clamshell", "Soft light above and below the face: beauty"),
                Block::LightPreset("split", "Split", "Hard side light: half lit, half shadow"),
                Block::LightPreset(
                    "productSoftbox",
                    "Product",
                    "Two side softboxes and one overhead: even product light",
                ),
                Block::LightPreset("highKey", "High key", "Bright and even on a white backdrop: catalogue"),
                Block::LightPreset("freshCounter", "Counter", "Shop counter: overhead panels, cool front fill"),
                Block::LightPreset("lowKey", "Low key", "One narrow light in the dark: dramatic"),
                Block::LightPreset("backlight", "Backlight", "Rim light from behind"),
                Block::LightPreset("window", "Window", "Big soft window light from the side"),
                Block::LightPreset("neon", "Neon", "Magenta and cyan side lights at night"),
                Block::LightPreset("stage", "Stage", "One hard spotlight from above in the dark"),
                Block::LightPreset("goldenHour", "Golden hr", "Warm low sun with a sunset sky"),
                Block::LightPreset("daylight", "Daylight", "High sun with a blue sky"),
                Block::LightPreset("overcast", "Overcast", "Soft cloudy sky light, no sun"),
                Block::LightPreset("night", "Night", "Moonlight under a night sky"),
                Block::LightPreset(
                    "clear",
                    "No lights",
                    "Remove all lights and the sky: back to the flat studio light",
                ),
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
    /// Cmd/Ctrl+drag on empty space: select what falls inside a rectangle.
    Marquee {
        start: Pos2,
    },
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
    /// Drag where a light points (a handle on the surface it shines at).
    LightAim {
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
    /// Edit mode: a tool follows this drag and applies on release.
    EditTool,
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

/// A keyframe diamond (or, with `all`, a row's whole animation by its bar) being dragged along
/// the track strip.
#[derive(Clone)]
struct KeyDrag {
    row: String,
    frame: f32,
    key: String,
    all: bool,
}

/// A shot bar being dragged on the timeline: what part, where it was grabbed.
#[derive(Clone)]
struct ShotDrag {
    id: String,
    /// 0 = move, 1 = start edge, 2 = end edge
    part: u8,
    grab: f32,
    start: u32,
    end: u32,
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
    /// The depth map the viewport last drew with (shown when switching to a manual range).
    depth_seen: shotstage_render::DepthMap,
    texture: Option<(egui::TextureId, u64)>,
    /// The small shot-camera preview in the corner of the free view.
    pip: Option<Pip>,
    /// The next shot's monitor beside it.
    pip_next: Option<Pip>,
    /// Playing a stretch only: stop (or loop back) at its end.
    play_range: Option<PlayRange>,
    /// Playback loops inside the shot under the playhead.
    loop_shot: bool,
    cam_preview: bool,
    /// The frames the timeline shows when zoomed in (None: all of it).
    tl_view: Option<(f32, f32)>,
    /// The storyboard strip above the timeline.
    show_board: bool,
    /// The curve editor under the keyframe rows.
    show_curves: bool,
    /// The shots row is hovered: keep the storyboard pictures ready for its tooltips.
    board_wanted: bool,
    curves: crate::curves::CurveEditor,
    board: Option<crate::storyboard::Storyboard>,
    /// What scrolling does in the view, and whether the last wheel input looked like a trackpad.
    scroll_mode: ScrollMode,
    /// The selection rectangle being dragged, in screen points.
    marquee: Option<Rect>,
    /// Scrolling not yet turned into a 15° turn of the dragged object.
    turn_acc: f32,
    wheel_trackpad: bool,
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
    /// Autosave: the revision last written and when (seconds), and a found autosave offered for
    /// restoring: (scene JSON, object count).
    autosaved: (u64, f64),
    autosave_warned: bool,
    restore_offer: Option<(std::rc::Rc<Scene>, usize)>,
    /// Draw shadows in the view.
    shadows: bool,
    /// More selected objects (Shift+click), besides the hub's selection.
    also: Vec<String>,
    /// Scene tree: name filter and the row being renamed (id, text).
    tree_filter: String,
    renaming: Option<(String, String)>,
    /// Properties header: the name being edited (id, text).
    name_edit: Option<(String, String)>,
    /// The camera the next picked picture becomes the reference of.
    reference_for: Option<String>,
    /// The mesh whose texture the next picked picture becomes.
    texture_for: Option<String>,
    /// The next picture picked becomes the sky (a panorama).
    sky_next: bool,
    /// egui textures of reference pictures: asset key -> (identity of the pixels, texture).
    ref_textures: std::collections::HashMap<String, (usize, egui::TextureHandle)>,
    /// Palette thumbnails (rendered in the background).
    thumbs: Option<crate::thumbs::Thumbs>,
    /// Draw action / title safe frames when looking through a camera.
    safe_areas: bool,
    /// Result of a background "save frame".
    saved: crate::files::Outbox,
    /// The video being exported, if any.
    video: crate::files::VideoProgress,
    /// Key every edit at the current frame.
    auto_key: bool,
    key_drag: Option<KeyDrag>,
    shot_drag: Option<ShotDrag>,
    /// The key a context menu on the track strip is about: (row, frame).
    key_menu: Option<(String, f32)>,
    motion_filter: String,
    /// The quick-actions box when open: (query, highlighted row).
    quick: Option<(String, usize)>,

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
    /// Mesh edit mode: the edited mesh's geometry, the running tool, view options.
    edit_cache: Option<editmode::EditCache>,
    /// The UV editor panel's view, cache and gesture.
    uv: uveditor::UvEditor,
    edit_modal: Option<editmode::Modal>,
    edit_xray: bool,
    edit_amounts: editmode::EditAmounts,
    edit_scroll: f32,
    edit_hot_axis: Option<usize>,
    edit_hover: Option<editmode::Elem>,
    /// The press that applied a tool: its click must not also select.
    edit_swallow: bool,
    /// Frames left to clear the keyboard focus a Tab gave to some button.
    edit_unfocus: u8,
    /// Proportional editing (O).
    edit_prop: bool,
    /// Boolean panel: the other mesh and the operation.
    bool_with: Option<String>,
    bool_op: shotstage_core::csg::BoolOp,
    /// Small screen (phone, narrow tablet): the side panels slide over the view one at a time.
    compact: bool,
    /// The side panel open over the view in the compact layout.
    drawer: Option<Drawer>,
    /// The blocks drawer shows the scene list instead of the blocks.
    drawer_tree: bool,
    /// Fingers, not a mouse: bigger buttons and handles.
    touch: bool,
    /// Two fingers are on the view (pan, pinch, twist); the one-finger drag waits until both lift.
    two_finger: bool,
    /// Undo steps there were when the current viewport drag began (a second finger landing
    /// soon after takes back what the first one did).
    drag_base: Option<(usize, f64)>,
}

/// A side panel shown over the view in the compact layout.
#[derive(Clone, Copy, PartialEq)]
enum Drawer {
    Blocks,
    Properties,
}

/// Narrower than this (in points), the side panels become drawers over the view.
const COMPACT_WIDTH: f32 = 900.0;

/// Is the main pointer a finger (a phone or tablet)? Known before the first touch.
fn coarse_pointer() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        thread_local! {
            static COARSE: bool = web_sys::window()
                .and_then(|w| w.match_media("(pointer: coarse)").ok().flatten())
                .is_some_and(|m| m.matches());
        }
        COARSE.with(|c| *c)
    }
    #[cfg(not(target_arch = "wasm32"))]
    false
}

/// One row of buttons that scrolls sideways when the screen is too narrow for it.
fn transport_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::horizontal().id_salt("transport").show(ui, |ui| {
        ui.horizontal(add);
    });
}

/// Roomier buttons and rows for fingers (the mouse sizes otherwise).
fn finger_spacing(s: &mut egui::Style, touch: bool) {
    if touch {
        s.spacing.item_spacing = egui::vec2(8.0, 7.0);
        s.spacing.button_padding = egui::vec2(10.0, 6.0);
        s.spacing.interact_size.y = 32.0;
        s.spacing.icon_width = 18.0;
        s.spacing.icon_width_inner = 10.0;
    } else {
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size.y = 22.0;
        s.spacing.icon_width = egui::Spacing::default().icon_width;
        s.spacing.icon_width_inner = egui::Spacing::default().icon_width_inner;
    }
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
        crate::i18n::init();
        crate::i18n::install_fonts(&cc.egui_ctx);
        let renderer = cc.wgpu_render_state.as_ref().map(|rs| Renderer::new(&rs.device));
        Self {
            hub,
            renderer,
            texture: None,
            pip: None,
            pip_next: None,
            play_range: None,
            loop_shot: false,
            cam_preview: true,
            show_board: false,
            show_curves: false,
            board_wanted: false,
            curves: Default::default(),
            tl_view: None,
            board: None,
            scroll_mode: ScrollMode::Auto,
            turn_acc: 0.0,
            marquee: None,
            wheel_trackpad: false,
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
            video: Default::default(),
            safe_areas: false,
            thumbs: None,
            reference_for: None,
            texture_for: None,
            sky_next: false,
            also: vec![],
            shadows: true,
            depth_seen: Default::default(),
            autosaved: (u64::MAX, 0.0),
            autosave_warned: false,
            restore_offer: crate::files::autosave_read().and_then(|j| {
                let sc: Scene = serde_json::from_str(&j).ok()?;
                let n = sc.entities.iter().filter(|e| e.parent.is_none()).count();
                (n > 0).then(|| (std::rc::Rc::new(sc), n))
            }),
            ref_textures: Default::default(),
            auto_key: false,
            key_drag: None,
            shot_drag: None,
            key_menu: None,
            motion_filter: String::new(),
            tree_filter: String::new(),
            renaming: None,
            name_edit: None,
            quick: None,

            motion_secs: 0.0,
            motion_strength: 1.0,
            studio_light: false,
            last_subject: None,
            toast: None,
            incoming: Default::default(),
            edit_cache: None,
            uv: Default::default(),
            edit_modal: None,
            edit_xray: false,
            edit_amounts: Default::default(),
            edit_scroll: 0.0,
            edit_hot_axis: None,
            edit_hover: None,
            edit_swallow: false,
            edit_unfocus: 0,
            edit_prop: false,
            bool_with: None,
            bool_op: Default::default(),
            compact: false,
            drawer: None,
            drawer_tree: false,
            touch: false,
            two_finger: false,
            drag_base: None,
        }
    }

    fn run(&self, command: &str, params: Value) -> Option<Value> {
        hub::run(&self.hub, command, params)
    }

    /// How many steps Undo can take back now.
    fn undo_depth(&self) -> usize {
        self.run("history.info", json!({})).and_then(|v| v["undo"].as_array().map(Vec::len)).unwrap_or(0)
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
                self.toast(crate::lf!(
                    "{}: {} objects, {} shots · press Preview shot to play it",
                    "{}：{} 个物体，{} 个镜头 · 按“预览镜头”播放",
                    tr(label),
                    o.result["entities"],
                    o.result["shots"]
                ));
            }
            Err(e) => self.toast(format!("{}: {}", tr(label), e.message)),
        }
    }

    /// Write the scene to the autosave a couple of seconds after it last changed (not while an
    /// autosave is waiting to be restored, so it is not overwritten).
    fn autosave(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if self.restore_offer.is_some() {
            return;
        }
        let rev = self.hub.borrow().session.revision();
        if self.autosaved.0 == u64::MAX {
            // first frame: remember where we start; save after the next change
            self.autosaved = (rev, now);
            return;
        }
        if rev == self.autosaved.0 {
            return;
        }
        if now - self.autosaved.1 < 2.0 {
            // egui only draws on input: come back when the save is due
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(2.0 - (now - self.autosaved.1) + 0.05));
            return;
        }
        let json = serde_json::to_string(&*self.scene()).unwrap_or_default();
        match crate::files::autosave_write(&json) {
            Ok(()) => self.autosave_warned = false,
            Err(e) => {
                log::warn!("autosave: {e}");
                // say it once, not every two seconds
                if !self.autosave_warned {
                    self.autosave_warned = true;
                    self.toast(crate::lf!(
                        "Autosave is not keeping this scene ({e}): use Save (Cmd/Ctrl+S)",
                        "自动保存没能保存这个场景（{e}）：请手动保存（Cmd/Ctrl+S）"
                    ));
                }
            }
        }
        self.autosaved = (rev, now);
    }

    fn save_scene(&mut self) {
        let json = serde_json::to_string_pretty(&*self.scene()).unwrap_or_default();
        match crate::files::save("scene.shotstage.json", json.as_bytes(), "application/json") {
            Ok(Some(w)) => self.toast(crate::lf!("Saved the scene: {w}", "场景已保存：{w}")),
            Ok(None) => {}
            Err(e) => self.toast(crate::lf!("Could not save: {e}", "无法保存：{e}")),
        }
    }

    /// Show a short status message at the bottom of the view.
    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((tr(msg.into()), f64::NAN));
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
            if ext == "json" {
                // a saved scene
                let opened = serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()).and_then(|scene| {
                    hub::dispatch(&self.hub, json!({"command": "scene.load", "params": {"scene": scene}}))
                        .map_err(|e| e.message)
                });
                match opened {
                    Ok(_) => self.toast(crate::lf!(
                        "Opened {name} (Undo goes back to the previous scene)",
                        "已打开 {name}（撤销可回到之前的场景）"
                    )),
                    Err(e) => self
                        .toast(crate::lf!("{name} is not a shotstage scene: {e}", "{name} 不是 shotstage 场景：{e}")),
                }
                continue;
            }
            // an HDRI is always a sky; a picture becomes one when asked for
            if ext == "hdr" || (self.sky_next && matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp")) {
                self.sky_next = false;
                let req = json!({"command": "environment.import", "params": {"data": encode(&bytes), "name": stem}});
                match hub::dispatch(&self.hub, req) {
                    Ok(o) => {
                        let warn = o.result["warning"].as_str().map(|w| format!(" ({w})")).unwrap_or_default();
                        self.toast(crate::lf!(
                            "{name} is now the sky{warn}. Add a matching sun under Sky ▾.",
                            "{name} 已设为天空{warn}。可在“天空 ▾”里添加匹配的太阳光。"
                        ));
                    }
                    Err(e) => self.toast(crate::lf!(
                        "Could not use {name} as the sky: {}",
                        "无法把 {name} 用作天空：{}",
                        e.message
                    )),
                }
                continue;
            }
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp")
                && let Some(mesh) = self.texture_for.take()
            {
                let req = json!({"command": "mesh.setMaterial", "params": {"id": mesh, "textureData": encode(&bytes)}});
                match hub::dispatch(&self.hub, req) {
                    Ok(_) => {
                        self.toast(crate::lf!("{name} is now the texture of {mesh}", "{name} 已成为 {mesh} 的贴图"))
                    }
                    Err(e) => self.toast(crate::lf!("Could not use {name}: {}", "无法使用 {name}：{}", e.message)),
                }
                continue;
            }
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp")
                && let Some(cam) = self.reference_for.take()
            {
                let req = json!({"command": "camera.setReference", "params": {"camera": cam, "data": encode(&bytes), "opacity": 0.5}});
                match hub::dispatch(&self.hub, req) {
                    Ok(_) => {
                        self.look_through = true;
                        self.hub.borrow_mut().look_through = true;
                        self.toast(crate::lf!(
                            "{name} is laid over the camera view: line the shot up with it",
                            "{name} 已叠在镜头画面上：对着它对齐构图"
                        ));
                    }
                    Err(e) => self.toast(crate::lf!("Could not use {name}: {}", "无法使用 {name}：{}", e.message)),
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
                        self.toast(crate::lf!(
                            "Added {name} ({} × {} px)",
                            "已添加 {name}（{} × {} 像素）",
                            o.result["pixels"][0],
                            o.result["pixels"][1]
                        ));
                    }
                    Err(e) => self.toast(crate::lf!("Could not add {name}: {}", "无法添加 {name}：{}", e.message)),
                }
                continue;
            }
            let Some(format) = ModelFormat::from_extension(&ext) else {
                self.toast(crate::lf!(
                    "{name}: not a model shotstage can import (glb, gltf, obj, stl, fbx, blend)",
                    "{name}：不是 shotstage 能导入的模型（glb、gltf、obj、stl、fbx、blend）"
                ));
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
                    let mut msg =
                        crate::lf!("Imported {name}: {} parts", "已导入 {name}：{} 个部件", o.result["entities"]);
                    if skins > 0 {
                        msg.push_str(" with a skeleton (click it, then a part, to pose)");
                    }
                    if let Some(w) = o.result["warnings"].as_array().and_then(|w| w.first()).and_then(Value::as_str) {
                        msg.push_str(&format!(" · {w}"));
                    }
                    self.toast(tr(msg));
                }
                Err(e) => self.toast(crate::lf!("Could not import {name}: {}", "无法导入 {name}：{}", e.message)),
            }
        }
    }

    fn export_model(&mut self, format: &str) {
        use base64::Engine as _;
        let req = json!({"command": "model.export", "params": {"format": format, "frame": self.frame()}});
        let r = match hub::dispatch(&self.hub, req) {
            Ok(o) => o.result,
            Err(e) => return self.toast(crate::lf!("Export failed: {}", "导出失败：{}", e.message)),
        };
        let name = r["fileName"].as_str().unwrap_or("scene.glb").to_string();
        let bytes = match (r["data"].as_str(), r["text"].as_str()) {
            (Some(d), _) => base64::engine::general_purpose::STANDARD.decode(d).unwrap_or_default(),
            (None, Some(t)) => t.as_bytes().to_vec(),
            _ => return self.toast(tr("Export failed: no data")),
        };
        let mime = r["mimeType"].as_str().unwrap_or("application/octet-stream").to_string();
        match crate::files::save(&name, &bytes, &mime) {
            Ok(Some(where_)) => {
                if let Some(mtl) = r["mtl"].as_str() {
                    let mtl_name = name.replace(".obj", ".mtl");
                    let _ = crate::files::save(&mtl_name, mtl.as_bytes(), "text/plain");
                }
                self.toast(crate::lf!(
                    "Exported {} meshes, {} triangles: {where_}",
                    "已导出 {} 个网格、{} 个三角面：{where_}",
                    r["meshes"],
                    r["triangles"]
                ));
            }
            Ok(None) => {}
            Err(e) => self.toast(crate::lf!("Export failed: {e}", "导出失败：{e}")),
        }
    }

    /// Motion presets for the selection: camera moves for a camera, actions for a person,
    /// motion for anything else. One click writes editable keyframes from the playhead.
    fn animate_section(&mut self, ui: &mut egui::Ui, scene: &Scene, id: &str, frame: f32) {
        use shotstage_commands::motion::{Group, PRESETS};
        let is_cam = matches!(scene.get(id).map(|e| &e.kind), Ok(EntityKind::Camera(_)));
        if matches!(scene.get(id).map(|e| &e.kind), Ok(EntityKind::Light(_))) {
            return;
        }
        let person = rig::person_root(scene, id).ok();
        let groups: &[Group] = if is_cam {
            &[Group::Camera]
        } else if person.is_some() {
            &[Group::Character, Group::Object]
        } else {
            &[Group::Object]
        };
        let target = person.clone().unwrap_or_else(|| if is_cam { id.to_string() } else { root_of(scene, id) });
        section(ui, &tr("Animate"), true, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.motion_filter)
                    .hint_text(tr("Search moves…"))
                    .desired_width(f32::INFINITY),
            );
            egui::Grid::new("motion_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label(tr("Length"));
                ui.add(egui::DragValue::new(&mut self.motion_secs).range(0.0..=60.0).speed(0.05).custom_formatter(
                    |v, _| if v <= 0.0 { tr("preset") } else { crate::lf!("{v:.1} s", "{v:.1} 秒") },
                ))
                .on_hover_text(tr("How long the move takes (preset = its own length)"));
                ui.end_row();
                ui.label(tr("Strength"));
                ui.add(egui::Slider::new(&mut self.motion_strength, 0.25..=3.0).max_decimals(2));
                ui.end_row();
            });
            let subject = self.last_subject.clone().filter(|s| scene.contains(s) && *s != target);
            if is_cam {
                match &subject {
                    Some(s) => ui.weak(crate::lf!(
                        "Moves around / looks at: {}",
                        "围绕 / 看向：{}",
                        scene.get(s).map(|e| e.name.as_str()).unwrap_or(s)
                    )),
                    None => {
                        ui.weak(tr("Select an object first to move around it (otherwise: what the camera looks at)."))
                    }
                };
            }
            let filter = self.motion_filter.trim().to_lowercase();
            ui.horizontal_wrapped(|ui| {
                // a person's own actions first, then general motion
                let ordered = groups.iter().flat_map(|g| PRESETS.iter().filter(move |p| p.group == *g));
                for p in ordered.filter(|p| p.name != "pathFollow") {
                    if !filter.is_empty()
                        && !p.label.to_lowercase().contains(&filter)
                        && !p.description.to_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    if ui
                        .button(tr(p.label))
                        .on_hover_text(crate::lf!("{} ({} s)", "{}（{} 秒）", p.description, p.seconds))
                        .clicked()
                    {
                        self.apply_motion(p, &target, if is_cam { subject.as_deref() } else { None }, frame);
                    }
                }
            });
        });
    }

    /// The selection with its Shift+clicked extras, leaving out anything inside another of them
    /// (it goes along with its parent).
    fn selection_roots(&self) -> Vec<String> {
        let Some(id) = self.selected() else { return vec![] };
        let scene = self.scene();
        let all: Vec<String> = std::iter::once(id).chain(self.also.iter().cloned()).collect();
        all.iter()
            .filter(|i| !all.iter().any(|o| o != *i && spatial::subtree(&scene, o).contains(i)))
            .cloned()
            .collect()
    }

    /// A new 16:9 camera where the free view looks from, aimed at the orbit target, with a lens
    /// whose frame fits inside what the view shows (the view is 45° tall); it becomes the active
    /// camera if there is none.
    fn camera_from_view(&mut self) {
        let scene = self.scene();
        let id = scene.unique_id("cam");
        let aspect = 16.0_f32 / 9.0;
        let view_aspect = self.last_view.map(|(r, _)| r.width() / r.height().max(1.0)).unwrap_or(aspect);
        let mut tan_half = 22.5_f32.to_radians().tan();
        if view_aspect < aspect {
            // a narrow view: match its width instead, so the frame stays inside it
            tan_half *= view_aspect / aspect;
        }
        let focal = ((36.0 / aspect) / (2.0 * tan_half)).clamp(8.0, 300.0);
        let n = scene.entities.iter().filter(|e| matches!(e.kind, EntityKind::Camera(_))).count();
        let name = format!("Cam {}", (b'A' + (n as u8).min(25)) as char);
        let steps = json!([
            {"command": "entity.create", "params": {"type": "camera", "id": id, "name": name,
                "position": self.orbit.eye().to_array(),
                "camera": {"focalLengthMm": (focal * 10.0).round() / 10.0, "aspect": aspect},
                "makeActive": scene.active_camera.is_none()}},
            {"command": "camera.lookAt", "params": {"id": id, "target": self.orbit.target.to_array()}},
        ]);
        match hub::dispatch(&self.hub, json!({"batch": steps})) {
            Ok(_) => {
                self.also.clear();
                self.select(Some(id));
                self.toast(crate::lf!(
                    "{name} placed where you are looking from: + Shot or the Shots row uses it",
                    "{name} 已放在你当前的视角：+ Shot 或镜头轨会用它"
                ));
            }
            Err(e) => self.toast(crate::lf!("Could not add a camera: {}", "无法添加相机：{}", e.message)),
        }
    }

    /// Clear the scene (undoable) and start the view over: playhead at the start, whole
    /// timeline, free view.
    fn new_scene(&mut self) {
        if self.run("scene.new", json!({})).is_some() {
            self.also.clear();
            self.select(None);
            self.playing = false;
            self.tl_view = None;
            self.look_through = false;
            let start = self.scene().timeline.start as f32;
            let mut h = self.hub.borrow_mut();
            h.frame = start;
            h.look_through = false;
        }
    }

    /// Put the selection on the clipboard as a shotstage clip (and delete it for Cut).
    fn copy_selection(&mut self, ctx: &egui::Context, cut: bool) {
        let ids = self.selection_roots();
        if ids.is_empty() {
            return;
        }
        let Some(clip) = self.run("entity.copy", json!({"ids": ids})) else { return };
        ctx.copy_text(clip.to_string());
        let n = ids.len();
        if cut {
            self.delete_selection();
        }
        self.toast(if crate::i18n::zh() {
            format!(
                "{}{n} 个物体（用 Cmd/Ctrl+V 粘贴，可贴到本场景或其他场景）",
                if cut { "已剪切 " } else { "已复制 " }
            )
        } else {
            format!(
                "{} {n} object{} (paste with Cmd/Ctrl+V, here or in another scene)",
                if cut { "Cut" } else { "Copied" },
                if n == 1 { "" } else { "s" }
            )
        });
    }

    /// Paste a shotstage clip from the clipboard; other text is ignored. Pasting next to the
    /// originals shifts the copies a little so they do not hide each other.
    fn paste(&mut self, text: &str) {
        let Ok(clip) = serde_json::from_str::<Value>(text.trim()) else { return };
        if clip["shotstageClip"].is_null() {
            return;
        }
        let scene = self.scene();
        let all_here = clip["entities"].as_array().is_some_and(|es| {
            es.iter().filter(|e| e["parent"].is_null()).all(|e| e["id"].as_str().is_some_and(|id| scene.contains(id)))
        });
        let mut params = json!({"clip": clip});
        if all_here {
            params["offset"] = json!([0.5, 0.0, 0.0]);
        }
        match hub::dispatch(&self.hub, json!({"command": "entity.paste", "params": params})) {
            Ok(o) => {
                let mut ids: Vec<String> = o.result["ids"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                if !ids.is_empty() {
                    let first = ids.remove(0);
                    self.select(Some(first));
                    self.also = ids;
                }
                self.toast(tr("Pasted (Undo takes it back)"));
            }
            Err(e) => self.toast(crate::lf!("Could not paste: {}", "无法粘贴：{}", e.message)),
        }
    }

    /// Duplicate everything selected as one undo step and select the copies.
    fn duplicate_selection(&mut self) {
        let ids = self.selection_roots();
        if ids.is_empty() {
            return;
        }
        let steps: Vec<Value> =
            ids.iter().map(|i| json!({"command": "entity.duplicate", "params": {"id": i}})).collect();
        match hub::dispatch(&self.hub, json!({"batch": steps})) {
            Ok(o) => {
                let mut new: Vec<String> = o
                    .result
                    .as_array()
                    .map(|a| a.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                if !new.is_empty() {
                    let first = new.remove(0);
                    self.select(Some(first));
                    self.also = new;
                }
            }
            Err(e) => self.toast(crate::lf!("Could not duplicate: {}", "无法复制：{}", e.message)),
        }
    }

    /// Select every visible, unlocked top-level object whose middle shows inside `r` (screen).
    fn select_in_rect(&mut self, r: Rect, view_rect: Rect, view: &View, f: f32) {
        let scene = self.scene();
        let mut hits: Vec<String> = scene
            .entities
            .iter()
            .filter(|e| e.parent.is_none() && e.visible && !e.locked)
            .filter(|e| {
                let mid = spatial::bounds(&scene, &e.id, f)
                    .ok()
                    .flatten()
                    .map(|b| b.center())
                    .or_else(|| world_at(&scene, &e.id, f).ok().map(|w| w.w_axis.truncate()));
                mid.and_then(|m| project(view_rect, view, m)).is_some_and(|p| r.contains(p))
            })
            .map(|e| e.id.clone())
            .collect();
        if hits.is_empty() {
            self.also.clear();
            self.select(None);
            return;
        }
        let first = hits.remove(0);
        self.select(Some(first));
        self.also = hits;
    }

    /// Delete everything selected (with Shift+clicked extras) as one undo step.
    fn delete_selection(&mut self) {
        let ids = self.selection_roots();
        if ids.is_empty() {
            return;
        }
        self.also.clear();
        let steps: Vec<Value> = ids.iter().map(|i| json!({"command": "entity.delete", "params": {"id": i}})).collect();
        if let Err(e) = hub::dispatch(&self.hub, json!({"batch": steps})) {
            self.toast(crate::lf!("Could not delete: {}", "无法删除：{}", e.message));
        }
    }

    /// Write a motion preset's keys on `target` from `frame`, then play it back.
    fn apply_motion(
        &mut self,
        p: &shotstage_commands::motion::Preset,
        target: &str,
        subject: Option<&str>,
        frame: f32,
    ) {
        let mut req =
            json!({"preset": p.name, "target": target, "start": frame.round(), "intensity": self.motion_strength});
        if self.motion_secs > 0.0 {
            req["duration"] = json!(self.motion_secs);
        }
        if let Some(s) = subject {
            req["subject"] = json!(s);
        }
        match hub::dispatch(&self.hub, json!({"command": "motion.apply", "params": req})) {
            Ok(o) => {
                // play it back right away
                self.hub.borrow_mut().frame = o.result["start"].as_f64().unwrap_or(0.0) as f32;
                self.playing = true;
                let (st, en) = (o.result["start"].as_f64().unwrap_or(0.0), o.result["end"].as_f64().unwrap_or(0.0));
                self.toast(crate::lf!(
                    "{}: frames {st:.0}–{en:.0}, {} keys · drag them on the timeline to retime",
                    "{}：第 {st:.0}–{en:.0} 帧，{} 个关键帧 · 在时间轴上拖动可改时间",
                    tr(p.label),
                    o.result["keys"]
                ));
            }
            Err(e) => self.toast(format!("{}: {}", p.label, e.message)),
        }
    }

    /// Everything the quick-actions box can do right now, with the selection in mind.
    fn quick_items(&self) -> Vec<QuickItem> {
        use shotstage_commands::motion::{Group, PRESETS};
        let mut out = Vec::new();
        let scene = self.scene();
        let sel = self.selected().filter(|s| scene.contains(s));
        let act = |label: &'static str, hint: &'static str, a: QuickAction| QuickItem {
            label: label.into(),
            kind: "Action",
            hint: hint.into(),
            run: QuickRun::Action(a),
        };
        out.push(act("Save scene", "Cmd/Ctrl+S", QuickAction::Save));
        out.push(act("Open / import a file…", "scene, model or picture", QuickAction::Open));
        out.push(act("Undo", "Cmd/Ctrl+Z", QuickAction::Undo));
        out.push(act("Redo", "Cmd/Ctrl+Shift+Z", QuickAction::Redo));
        out.push(act("New scene", "clear everything (undoable)", QuickAction::New));
        out.push(act("Toggle shadows", "", QuickAction::Shadows));
        out.push(act("Play / pause", "Space", QuickAction::Play));
        out.push(act("Go to the start", "Home", QuickAction::Start));
        out.push(act("Toggle camera view", "look through the shot camera", QuickAction::CameraView));
        out.push(act("Toggle storyboard", "every shot as a picture", QuickAction::Board));
        out.push(act("Toggle camera preview", "the small view in the corner", QuickAction::Preview));
        out.push(act("Fit the timeline", "show all frames", QuickAction::FitTimeline));
        out.push(act("Toggle safe areas", "action / title safe frames in camera view", QuickAction::SafeAreas));
        out.push(act("New camera from this view", "where you are looking from", QuickAction::CameraHere));
        if scene.active_camera.is_some() || !scene.timeline.shots.is_empty() {
            out.push(act("Save frame as PNG", "the shot camera at the playhead, 1920 px", QuickAction::SaveFrame));
        }
        if !scene.timeline.shots.is_empty() {
            out.push(act("Export storyboard", "every shot as one HTML page", QuickAction::ExportBoard));
            out.push(act(
                "Export shot sheet (PDF)",
                "shot size, lens, camera move and notes of every shot",
                QuickAction::ExportSheet,
            ));
        }
        if sel.is_some() {
            out.push(act("Focus the selection", "F", QuickAction::Focus));
            out.push(act("Key the selection", "K: keyframe at the playhead", QuickAction::Key));
            out.push(act("Duplicate", "Cmd/Ctrl+D", QuickAction::Duplicate));
            out.push(act("Delete", "Delete", QuickAction::Delete));
        }
        if let Some(id) = &sel {
            let is_cam = matches!(scene.get(id).map(|e| &e.kind), Ok(EntityKind::Camera(_)));
            let person = rig::person_root(&scene, id).ok();
            if let Some(root) = &person {
                for p in rig::POSES {
                    out.push(QuickItem {
                        label: humanize(p.name),
                        kind: "Pose",
                        hint: p.description.into(),
                        run: QuickRun::Pose(root.clone(), p.name),
                    });
                }
            }
            if !matches!(scene.get(id).map(|e| &e.kind), Ok(EntityKind::Light(_))) {
                let target = person.clone().unwrap_or_else(|| if is_cam { id.clone() } else { root_of(&scene, id) });
                let subject = self.last_subject.clone().filter(|s| scene.contains(s) && *s != target);
                for p in PRESETS.iter().filter(|p| p.name != "pathFollow") {
                    let fits = match p.group {
                        Group::Camera => is_cam,
                        Group::Character => person.is_some(),
                        Group::Object => !is_cam,
                    };
                    if fits {
                        let subject = if is_cam { subject.clone() } else { None };
                        out.push(QuickItem {
                            label: p.label.into(),
                            kind: if is_cam { "Camera move" } else { "Motion" },
                            hint: p.description.into(),
                            run: QuickRun::Motion(p, target.clone(), subject),
                        });
                    }
                }
            }
        }
        // getting around: shots, cameras, and every object by name (before the blocks, so typing
        // an object's name selects it rather than adding another one)
        let fps = scene.timeline.fps.max(1.0);
        for (i, sh) in scene.timeline.shots.iter().enumerate() {
            let cam = scene.get(&sh.camera).map(|e| e.name.clone()).unwrap_or(sh.camera.clone());
            out.push(QuickItem {
                label: crate::lf!("Go to shot {}: {}", "跳到镜头 {}：{}", i + 1, sh.name),
                kind: "Shot",
                hint: crate::lf!("{:.1}s through {cam}", "{:.1} 秒，通过 {cam}", (sh.end + 1 - sh.start) as f32 / fps),
                run: QuickRun::Shot(sh.start),
            });
        }
        for e in scene.entities.iter().filter(|e| e.parent.is_none()) {
            if matches!(e.kind, EntityKind::Camera(_)) {
                out.push(QuickItem {
                    label: crate::lf!("Look through {}", "透过 {} 看", e.name),
                    kind: "Camera",
                    hint: "makes it the active camera".into(),
                    run: QuickRun::LookThrough(e.id.clone()),
                });
            }
            out.push(QuickItem {
                label: e.name.clone(),
                kind: "Select",
                hint: e.id.clone(),
                run: QuickRun::Select(e.id.clone()),
            });
        }
        for (_, _, blocks) in palette() {
            for b in blocks {
                let kind = if matches!(b, Block::LightPreset(..)) { "Lighting" } else { "Add" };
                out.push(QuickItem {
                    label: b.label().into(),
                    kind,
                    hint: b.description().into(),
                    run: QuickRun::Block(b),
                });
            }
        }
        for (name, label, desc) in shotstage_commands::templates::TEMPLATES {
            out.push(QuickItem {
                label: (*label).into(),
                kind: "Template",
                hint: (*desc).into(),
                run: QuickRun::Template(name, label),
            });
        }
        out
    }

    fn run_quick(&mut self, item: QuickItem, ctx: &egui::Context) {
        let frame = self.frame();
        match item.run {
            QuickRun::Block(b) => self.use_block(&b),
            QuickRun::Template(name, label) => self.apply_template(name, label),
            QuickRun::Motion(p, target, subject) => self.apply_motion(p, &target, subject.as_deref(), frame),
            QuickRun::Pose(root, pose) => {
                let animated =
                    self.auto_key || rig::JOINTS.iter().any(|j| is_animated(&self.scene(), &format!("{root}_{j}")));
                let mut req = json!({"id": root, "pose": pose, "frame": frame});
                if animated {
                    req["keyframeAt"] = json!(frame.round());
                }
                self.run("pose.set", req);
            }
            QuickRun::Shot(start) => {
                self.playing = false;
                self.look_through = true;
                let mut h = self.hub.borrow_mut();
                h.frame = start as f32;
                h.look_through = true;
            }
            QuickRun::LookThrough(cam) => {
                self.run("camera.setActive", json!({"id": cam}));
                self.look_through = true;
                self.hub.borrow_mut().look_through = true;
            }
            QuickRun::Select(id) => {
                self.also.clear();
                self.select(Some(id));
                self.focus_selection();
            }
            QuickRun::Action(a) => match a {
                QuickAction::Save => self.save_scene(),
                QuickAction::Open => crate::files::pick(&self.incoming, ctx),
                QuickAction::Undo => _ = self.run("history.undo", json!({})),
                QuickAction::Redo => _ = self.run("history.redo", json!({})),
                QuickAction::New => self.new_scene(),
                QuickAction::Shadows => self.shadows = !self.shadows,
                QuickAction::Focus => self.focus_selection(),
                QuickAction::Key => self.key_selection(),
                QuickAction::Duplicate => self.duplicate_selection(),
                QuickAction::Delete => self.delete_selection(),
                QuickAction::CameraHere => self.camera_from_view(),
                QuickAction::SaveFrame => crate::files::save_frame(&self.hub, 1920, &self.saved),
                QuickAction::ExportBoard => self.export_board(),
                QuickAction::ExportSheet => self.export_sheet("pdf", "table"),
                QuickAction::Play => self.playing = !self.playing,
                QuickAction::Start => {
                    self.playing = false;
                    let start = self.scene().timeline.start as f32;
                    self.hub.borrow_mut().frame = start;
                }
                QuickAction::CameraView => {
                    self.look_through = !self.look_through;
                    self.hub.borrow_mut().look_through = self.look_through;
                }
                QuickAction::Board => self.show_board = !self.show_board,
                QuickAction::Preview => self.cam_preview = !self.cam_preview,
                QuickAction::FitTimeline => self.tl_view = None,
                QuickAction::SafeAreas => self.safe_areas = !self.safe_areas,
            },
        }
    }

    /// The quick-actions box (Cmd/Ctrl+K or /): type a few letters, Enter runs the top match.
    fn quick_box(&mut self, ctx: &egui::Context, full: Rect) {
        let Some((mut query, mut pick)) = self.quick.take() else { return };
        let items = self.quick_items();
        let ranked = rank_quick(&items, &query);
        let (up, down, enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if down {
            pick = (pick + 1).min(ranked.len().saturating_sub(1));
        }
        if up {
            pick = pick.saturating_sub(1);
        }
        let mut chosen = enter.then(|| ranked.get(pick).copied()).flatten();
        let mut keep = !esc && !enter;
        let width = 460.0_f32.min(full.width() - 24.0);
        let resp = egui::Area::new(egui::Id::unique("quick_actions"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(full.center().x - width / 2.0, full.top() + 48.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                    ui.set_width(width);
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut query)
                            .hint_text(tr("Add a block, apply a move or pose, run an action…"))
                            .desired_width(f32::INFINITY),
                    );
                    edit.request_focus();
                    if edit.changed() {
                        pick = 0;
                    }
                    ui.add_space(4.0);
                    if ranked.is_empty() {
                        ui.weak(tr("Nothing matches."));
                    }
                    egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (row, &i) in ranked.iter().enumerate().take(40) {
                            let it = &items[i];
                            let on = row == pick;
                            let r = ui
                                .horizontal(|ui| {
                                    ui.set_width(ui.available_width());
                                    let label = egui::RichText::new(tr(&it.label));
                                    ui.label(if on { label.strong() } else { label });
                                    ui.weak(egui::RichText::new(tr(it.kind)).small());
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.add(
                                            egui::Label::new(egui::RichText::new(tr(&it.hint)).weak().small())
                                                .truncate(),
                                        );
                                    });
                                })
                                .response
                                .interact(Sense::click());
                            if on {
                                ui.painter().rect_filled(r.rect.expand(2.0), 4.0, ACCENT.gamma_multiply(0.22));
                                if up || down {
                                    r.scroll_to_me(None);
                                }
                            }
                            if r.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
                                pick = row;
                            }
                            if r.clicked() {
                                chosen = Some(i);
                                keep = false;
                            }
                        }
                    });
                    ui.add_space(2.0);
                    ui.weak(egui::RichText::new(tr("↑↓ choose · Enter run · Esc close")).small());
                });
            })
            .response;
        if ctx.input(|i| i.pointer.any_pressed()) && !resp.contains_pointer() {
            keep = false;
        }
        if let Some(i) = chosen {
            let label = items[i].label.clone();
            let kind = items[i].kind;
            self.run_quick(items.into_iter().nth(i).unwrap(), ctx);
            if kind != "Action" && kind != "Lighting" && kind != "Template" {
                log::debug!("quick: {label}");
            }
        }
        if keep {
            self.quick = Some((query, pick));
        }
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

    /// The current scene, shared (copied once per change, not per call).
    fn scene(&self) -> std::rc::Rc<Scene> {
        let mut h = self.hub.borrow_mut();
        let rev = h.session.revision();
        if let Some((r, s)) = &h.scene_cache
            && *r == rev
        {
            return s.clone();
        }
        let s = std::rc::Rc::new(h.session.scene().clone());
        h.scene_cache = Some((rev, s.clone()));
        s
    }
    fn frame(&self) -> f32 {
        self.hub.borrow().frame
    }

    /// Play through the cut at frame `at`: from a moment before it to a moment after, through
    /// the shot cameras, then stop (back in the free view if that is where it started).
    fn preview_cut(&mut self, at: f32) {
        let t = self.scene().timeline.clone();
        let pad = (t.fps * 1.5).max(1.0);
        let restore = !self.look_through;
        let mut h = self.hub.borrow_mut();
        h.frame = (at - pad).max(t.start as f32);
        h.look_through = true;
        drop(h);
        self.look_through = true;
        self.play_range = Some(PlayRange { end: (at + pad).min(t.end as f32), restore_free_view: restore });
        self.playing = true;
    }

    /// Put the playhead on the previous (`dir` < 0) or next cut between shots.
    fn jump_cut(&mut self, dir: i32) {
        let scene = self.scene();
        let now = self.frame();
        let mut stops: Vec<f32> = cuts(&scene).into_iter().map(|(f, _)| f).collect();
        stops.push(scene.timeline.start as f32);
        stops.sort_by(f32::total_cmp);
        let to = if dir < 0 {
            stops.iter().rev().find(|f| **f < now - 0.5).copied()
        } else {
            stops.iter().find(|f| **f > now + 0.5).copied()
        };
        if let Some(f) = to {
            self.playing = false;
            self.hub.borrow_mut().frame = f;
        }
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
        self.video_progress(ui.ctx());
        self.autosave(ui.ctx());
        let saved = self.saved.borrow_mut().take();
        match saved {
            Some(Ok(w)) => self.toast(crate::lf!("Saved: {w}", "已保存：{w}")),
            Some(Err(e)) => self.toast(crate::lf!("Could not save: {e}", "无法保存：{e}")),
            None => {}
        }
        if !self.incoming.borrow().is_empty() {
            self.import_incoming();
        }
        {
            let scene = self.scene();
            let primary = self.selected();
            self.also.retain(|a| scene.contains(a) && Some(a) != primary.as_ref());
            if primary.is_none() {
                self.also.clear();
            }
        }
        if let Some(sel) = self.selected() {
            let scene = self.scene();
            if matches!(scene.get(&sel).map(|e| &e.kind), Ok(EntityKind::Mesh { .. } | EntityKind::Empty)) {
                self.last_subject = Some(root_of(&scene, &sel));
            }
        }
        if !self.playing {
            self.play_range = None;
        }
        if self.playing {
            let dt = ui.input(|i| i.stable_dt).min(0.1);
            let scene = self.scene();
            let t = &scene.timeline;
            let mut h = self.hub.borrow_mut();
            let before = h.frame;
            h.frame += dt * t.fps;
            let looping = self.loop_shot.then(|| t.shot_at(before)).flatten();
            if let Some(r) = self.play_range {
                if h.frame >= r.end {
                    h.frame = r.end;
                    self.playing = false;
                    self.play_range = None;
                    if r.restore_free_view {
                        self.look_through = false;
                        h.look_through = false;
                    }
                }
            } else if let Some(sh) = looping
                && h.frame >= sh.end as f32 + 1.0
            {
                h.frame = sh.start as f32;
            } else if h.frame > t.end as f32 {
                h.frame = t.start as f32;
            }
            ui.ctx().request_repaint();
        }
        self.shortcuts(ui);
        self.quick_box(ui.ctx(), ui.ctx().content_rect());

        {
            let h = self.hub.borrow();
            (self.pass, self.look_through) = (h.pass, h.look_through);
        }
        let screen = ui.ctx().content_rect();
        self.compact = screen.width() < COMPACT_WIDTH;
        if !self.compact {
            self.drawer = None;
        }
        let touch = coarse_pointer() || ui.input(|i| i.has_touch_screen());
        if touch != self.touch {
            self.touch = touch;
            ui.ctx().all_styles_mut(|s| finger_spacing(s, touch));
        }
        // a block carried out of the drawer: clear the view for it
        if self.palette.is_some() && self.drawer == Some(Drawer::Blocks) {
            self.drawer = None;
        }
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        // a drawer covers everything under the top bar, the timeline too
        let under_top = ui.available_rect_before_wrap();
        egui::Panel::bottom("timeline").resizable(false).show(ui, |ui| self.bottom(ui));
        if self.board_wanted && !self.show_board {
            self.update_board(ui.ctx(), frame);
        }
        self.board_wanted = false;
        if self.show_board {
            egui::Panel::bottom("storyboard").resizable(false).show(ui, |ui| self.storyboard_strip(ui, frame));
        }
        if !self.compact {
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
        }
        if self.hub.borrow().uv.is_some() {
            let max = if self.compact { (screen.width() * 0.6).max(240.0) } else { 1400.0 };
            egui::Panel::right("uv_editor")
                .resizable(true)
                .default_size(440.0f32.min(max))
                .min_size(240.0f32.min(max))
                .max_size(max)
                .show(ui, |ui| self.uv_panel(ui));
        } else {
            self.uv.hovered = false;
            self.uv.gesture = Default::default();
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.viewport(ui, frame));
        if let Some(d) = self.drawer {
            self.drawer_panel(ui.ctx(), d, under_top);
        }
        {
            let mut h = self.hub.borrow_mut();
            (h.pass, h.look_through) = (self.pass, self.look_through);
        }
    }
}

impl EditorApp {
    fn shortcuts(&mut self, ui: &egui::Ui) {
        if self.edit_unfocus > 0 {
            self.edit_unfocus -= 1;
            ui.ctx().memory_mut(|m| {
                if let Some(f) = m.focused() {
                    m.surrender_focus(f);
                }
            });
            ui.ctx().request_repaint();
        }
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        if (self.uv.hovered || matches!(self.uv.gesture, uveditor::UvGesture::Modal(_))) && self.uv_shortcuts(ui) {
            return;
        }
        if self.edit_shortcuts(ui) {
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
        if dup {
            self.duplicate_selection();
        }
        if undo {
            self.run("history.undo", json!({}));
        }
        if redo {
            self.run("history.redo", json!({}));
        }
        if del {
            self.delete_selection();
        }
        if space {
            self.playing = !self.playing;
        }
        if ui.input(|i| {
            (i.modifiers.command && i.key_pressed(egui::Key::K))
                || i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "/"))
        }) {
            self.quick = Some((String::new(), 0));
        }
        // Cmd/Ctrl+C, X, V: objects travel through the system clipboard as JSON (between scenes,
        // tabs, and to or from agents)
        let (copy, cut, pasted) = ui.input(|i| {
            let mut out = (false, false, None);
            for e in &i.events {
                match e {
                    egui::Event::Copy => out.0 = true,
                    egui::Event::Cut => out.1 = true,
                    egui::Event::Paste(t) => out.2 = Some(t.clone()),
                    _ => {}
                }
            }
            out
        });
        if copy || cut {
            self.copy_selection(ui.ctx(), cut);
        }
        if let Some(text) = pasted {
            self.paste(&text);
        }
        // Esc with nothing in progress clears the selection
        if ui.input(|i| i.key_pressed(egui::Key::Escape))
            && matches!(self.drag, Gesture::Idle)
            && self.palette.is_none()
            && self.quick.is_none()
            && !egui::Popup::is_any_open(ui.ctx())
        {
            self.also.clear();
            self.select(None);
        }
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.save_scene();
        }
        if ui.input(|i| i.key_pressed(egui::Key::F)) {
            self.focus_selection();
        }
        let (group, ungroup) = ui.input(|i| {
            let g = i.modifiers.command && i.key_pressed(egui::Key::G);
            (g && !i.modifiers.shift, g && i.modifiers.shift)
        });
        if group && !self.also.is_empty() {
            self.group_selection();
        }
        if ungroup && let Some(id) = self.selected() {
            match hub::dispatch(&self.hub, json!({"command": "entity.ungroup", "params": {"id": id}})) {
                Ok(_) => {
                    self.select(None);
                    self.toast(tr("Ungrouped"));
                }
                Err(e) => self.toast(crate::lf!("Could not ungroup: {}", "无法解组：{}", e.message)),
            }
        }
        let cut_step = ui.input(|i| {
            if i.key_pressed(egui::Key::OpenBracket) {
                -1
            } else if i.key_pressed(egui::Key::CloseBracket) {
                1
            } else {
                0
            }
        });
        if cut_step != 0 {
            self.jump_cut(cut_step);
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
                !i.modifiers.command && i.key_pressed(egui::Key::K),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
                step * if i.modifiers.shift { 10.0 } else { 1.0 },
            )
        });
        let t = self.scene().timeline.clone();
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
        if self.compact {
            self.top_bar_compact(ui);
            return;
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr("shotstage")).strong().color(ACCENT));
            ui.separator();
            if ui.button(tr("Open…")).on_hover_text(tr("Open a saved scene (.json), or import models and pictures")).clicked() {
                crate::files::pick(&self.incoming, ui.ctx());
            }
            if ui.button(tr("Save")).on_hover_text(tr("Save the scene as a .shotstage.json file (Cmd/Ctrl+S)")).clicked() {
                self.save_scene();
            }
            if ui.button(tr("New")).on_hover_text(tr("Clear the scene (undoable)")).clicked() {
                self.new_scene();
            }
            if ui.button(tr("Undo")).on_hover_text(tr("Cmd/Ctrl+Z")).clicked() {
                self.run("history.undo", json!({}));
            }
            if ui.button(tr("Redo")).on_hover_text(tr("Cmd/Ctrl+Shift+Z")).clicked() {
                self.run("history.redo", json!({}));
            }
            if ui.button(tr("🔍 Actions")).on_hover_text(tr("Search every block, move, pose, template and action (Cmd/Ctrl+K or /)")).clicked() {
                self.quick = Some((String::new(), 0));
            }
            ui.menu_button(tr("History"), |ui| self.history_menu(ui));
            ui.separator();
            if ui
                .button(tr("Import…"))
                .on_hover_text(tr("glTF / GLB, OBJ, STL, FBX and .blend (FBX / .blend on the desktop, through Blender). You can also drop files on the view."))
                .clicked()
            {
                crate::files::pick(&self.incoming, ui.ctx());
            }
            ui.menu_button(tr("Templates"), |ui| self.templates_menu(ui));
            ui.menu_button(tr("Export"), |ui| self.export_menu(ui));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // the language switch, in both languages so it is always findable
                self.language_switch(ui);
                ui.separator();
                let buttons = self.hub.borrow().host_buttons.clone();
                if buttons.is_empty() {
                    ui.weak(tr("3D previs for AI images and video"));
                }
                self.host_buttons(ui, true);
            });
        });
    }

    /// The top bar on a small screen: the file actions fold into a menu, and two buttons slide
    /// the blocks and the properties over the view.
    fn top_bar_compact(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.menu_button(tr("☰ Menu"), |ui| {
                if ui.button(tr("Open…")).clicked() {
                    crate::files::pick(&self.incoming, ui.ctx());
                    ui.close();
                }
                if ui.button(tr("Save")).clicked() {
                    self.save_scene();
                    ui.close();
                }
                if ui.button(tr("New")).clicked() {
                    self.new_scene();
                    ui.close();
                }
                if ui.button(tr("Import…")).clicked() {
                    crate::files::pick(&self.incoming, ui.ctx());
                    ui.close();
                }
                ui.separator();
                ui.menu_button(tr("Templates"), |ui| self.templates_menu(ui));
                ui.menu_button(tr("Export"), |ui| self.export_menu(ui));
                ui.menu_button(tr("History"), |ui| self.history_menu(ui));
                ui.separator();
                ui.horizontal(|ui| self.language_switch(ui));
                if !self.hub.borrow().host_buttons.is_empty() {
                    ui.separator();
                    self.host_buttons(ui, false);
                }
            });
            if ui.button(tr("Undo")).clicked() {
                self.run("history.undo", json!({}));
            }
            if ui.button(tr("Redo")).clicked() {
                self.run("history.redo", json!({}));
            }
            if ui.button(tr("🔍")).on_hover_text(tr("Search every block, move, pose, template and action")).clicked()
            {
                self.quick = Some((String::new(), 0));
            }
            ui.separator();
            for (d, label) in [(Drawer::Blocks, "＋ Blocks"), (Drawer::Properties, "Properties")] {
                if ui.selectable_label(self.drawer == Some(d), tr(label)).clicked() {
                    self.drawer = if self.drawer == Some(d) { None } else { Some(d) };
                }
            }
        });
    }

    fn history_menu(&mut self, ui: &mut egui::Ui) {
        let info = self.run("history.info", json!({})).unwrap_or(Value::Null);
        let undo: Vec<String> = info["undo"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if undo.is_empty() {
            ui.weak(tr("Nothing to undo yet"));
        } else {
            ui.weak(tr("Click a step to go back to before it"));
        }
        for (i, label) in undo.iter().take(20).enumerate() {
            if ui.button(format!("{}. {}", i + 1, tr(friendly(label)))).clicked() {
                for _ in 0..=i {
                    self.run("history.undo", json!({}));
                }
                ui.close();
            }
        }
    }

    fn templates_menu(&mut self, ui: &mut egui::Ui) {
        ui.weak(tr("Start from a ready scene (replaces this one; Undo brings it back)"));
        use shotstage_commands::templates::{PRODUCT_LOOKS, TEMPLATES};
        for (heading, products) in [("Scenes", false), ("Product close-ups", true)] {
            ui.separator();
            ui.label(egui::RichText::new(tr(heading)).strong());
            for (name, label, desc) in TEMPLATES.iter().filter(|t| PRODUCT_LOOKS.contains(&t.0) == products) {
                if ui.button(tr(*label)).on_hover_text(tr(*desc)).clicked() {
                    self.apply_template(name, label);
                    ui.close();
                }
            }
        }
    }

    fn export_menu(&mut self, ui: &mut egui::Ui) {
        ui.weak(tr("The scene posed at the current frame"));
        for (label, format, tip) in [
            ("GLB (glTF binary)", "glb", "Meshes, colours, cameras and lights: Blender, Unreal, Unity, three.js"),
            ("glTF (text)", "gltf", "The same as GLB, as one JSON file"),
            ("OBJ + MTL", "obj", "Meshes and colours, for almost any 3D tool"),
            ("STL", "stl", "Geometry only, for 3D printing and CAD"),
            ("FBX (via Blender)", "fbx", "Needs Blender on this computer (desktop editor)"),
        ] {
            if ui.button(tr(label)).on_hover_text(tr(tip)).clicked() {
                self.export_model(format);
                ui.close();
            }
        }
        ui.separator();
        self.sheet_menu(ui);
    }

    fn language_switch(&mut self, ui: &mut egui::Ui) {
        let zh = crate::i18n::zh();
        if ui.selectable_label(zh, tr("中文")).on_hover_text(tr("界面语言：中文")).clicked() && !zh {
            crate::i18n::set_zh(true);
        }
        if ui.selectable_label(!zh, tr("EN")).on_hover_text(tr("Interface language: English")).clicked() && zh {
            crate::i18n::set_zh(false);
        }
    }

    /// The buttons the host page added to the top bar (`reversed` in a right-to-left row).
    fn host_buttons(&mut self, ui: &mut egui::Ui, reversed: bool) {
        let mut buttons = self.hub.borrow().host_buttons.clone();
        if reversed {
            buttons.reverse();
        }
        for b in buttons.iter() {
            let mut r = ui.button(b.label());
            if let Some(tip) = b.tooltip() {
                r = r.on_hover_text(tip);
            }
            if r.clicked() {
                hub::emit(&self.hub, &json!({ "type": "toolbar", "id": b.id }));
            }
        }
    }

    /// A side panel slid over the view (compact layout): the blocks from the left, the
    /// properties from the right.
    fn drawer_panel(&mut self, ctx: &egui::Context, d: Drawer, middle: Rect) {
        let w = (middle.width() * 0.88).min(340.0);
        let rect = match d {
            Drawer::Blocks => Rect::from_min_size(middle.min, Vec2::new(w, middle.height())),
            Drawer::Properties => {
                Rect::from_min_size(egui::pos2(middle.right() - w, middle.top()), Vec2::new(w, middle.height()))
            }
        };
        egui::Area::new(egui::Id::unique("drawer"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                let frame = egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::same(8))
                    .shadow(ui.style().visuals.popup_shadow);
                frame.show(ui, |ui| {
                    let inner = rect.size() - Vec2::splat(16.0);
                    ui.set_min_size(inner);
                    ui.set_max_size(inner);
                    ui.horizontal(|ui| {
                        match d {
                            // the blocks and the scene list take turns: each gets the full height
                            Drawer::Blocks => {
                                ui.selectable_value(&mut self.drawer_tree, false, tr("Blocks"));
                                ui.selectable_value(&mut self.drawer_tree, true, tr("Scene"));
                            }
                            Drawer::Properties => {
                                ui.strong(tr("Properties"));
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(tr("✕ Close")).clicked() {
                                self.drawer = None;
                            }
                        });
                    });
                    match d {
                        Drawer::Blocks if self.drawer_tree => self.scene_tree(ui),
                        Drawer::Blocks => self.blocks(ui),
                        Drawer::Properties => {
                            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.properties(ui));
                        }
                    }
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
            ui.strong(tr("Blocks"));
            ui.add(
                egui::TextEdit::singleline(&mut self.block_filter)
                    .hint_text(tr("Search blocks…"))
                    .desired_width(f32::INFINITY),
            );
        });
        ui.weak(if self.touch {
            tr("Tap to drop one in the middle of the view, or drag it sideways into the scene.")
        } else {
            tr("Drag into the scene, or click to drop it in the middle.")
        });
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
                let whole = !filter.is_empty() && crate::i18n::both(title).to_lowercase().contains(&filter);
                let shown: Vec<Block> = blocks.into_iter().filter(|b| whole || b.matches(&filter)).collect();
                if shown.is_empty() {
                    continue;
                }
                let mut header = egui::CollapsingHeader::new(egui::RichText::new(tr(title)).strong())
                    .id_salt(title)
                    .default_open(open);
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
            egui::CollapsingHeader::new(egui::RichText::new(tr("Images")).strong())
                .id_salt("images")
                .default_open(false)
                .show(ui, |ui| {
                    ui.weak(tr("Pick a picture, or drop one on the view (a transparent PNG becomes a cut-out)."));
                    ui.horizontal_wrapped(|ui| {
                        for (label, mode, tip) in [
                            ("Card", "card", "A standing picture: reference photo, poster, sign"),
                            ("Cut-out", "billboard", "Always faces the camera: cut-out people, trees"),
                            ("Backdrop", "backdrop", "Fills the camera's background, behind everything"),
                            ("Floor", "floor", "Lies flat: rug, floor plan, logo on the ground"),
                        ] {
                            if ui.add_sized([tile, 30.0], egui::Button::new(tr(label))).on_hover_text(tr(tip)).clicked()
                            {
                                self.image_mode = Some(mode);
                                crate::files::pick(&self.incoming, ui.ctx());
                            }
                        }
                    });
                });
            egui::CollapsingHeader::new(egui::RichText::new(tr("Cameras & helpers")).strong())
                .id_salt("helpers")
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_sized([tile, 30.0], egui::Button::new(tr("Camera")))
                            .on_hover_text(tr("A camera where you are looking from"))
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
                            .add_sized([tile, 30.0], egui::Button::new(tr("Empty")))
                            .on_hover_text(tr("A group / pivot without geometry"))
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
                    tr(b.label()),
                    egui::FontId::proportional(11.5),
                    v.text_color(),
                );
                r
            }
            None => ui.add_sized(
                [width, 30.0],
                egui::Button::new(egui::RichText::new(tr(b.label())).size(12.5)).sense(Sense::click_and_drag()),
            ),
        };
        let r = r.on_hover_cursor(CursorIcon::Grab).on_hover_text(tr(b.description()));
        // With fingers a drag up or down scrolls the list; a sideways one carries the block out.
        let sideways = || {
            let d = ui.input(|i| Some(i.pointer.latest_pos()? - i.pointer.press_origin()?)).unwrap_or_default();
            d.x.abs() > d.y.abs()
        };
        if r.drag_started() && !matches!(b, Block::LightPreset(..)) && (!self.touch || sideways()) {
            let key = self.new_key("new");
            self.palette = Some(PaletteDrag { block: b.clone(), spawned: None, key });
        }
        if self.touch && r.dragged() && self.palette.is_none() {
            ui.scroll_with_delta(Vec2::new(0.0, r.drag_delta().y));
        }
        if r.clicked() {
            self.use_block(b);
        }
    }

    /// Clicking a block: drop it in the middle of the view (or apply a lighting preset).
    fn use_block(&mut self, b: &Block) {
        if let Block::LightPreset(preset, ..) = b {
            let mut p = json!({"preset": preset});
            if let Some(s) = self.last_subject.clone().filter(|s| self.scene().contains(s)) {
                p["subject"] = json!(s);
            }
            if self.run("light.preset", p).is_some() {
                self.toast(crate::lf!("Lighting: {}", "灯光：{}", tr(b.label())));
            }
            return;
        }
        let key = self.new_key("new");
        if let Some(id) = self.spawn(b, &key) {
            if let Some((rect, view)) = self.last_view {
                self.place_on_ray(&key, &id, ray_at(rect, &view, rect.center()), Vec3::ZERO, false);
            } else {
                self.run_key(&key, "entity.place", json!({"id": id}));
            }
            self.select(Some(id));
            // on a small screen, get out of the way so the new block shows
            if self.compact {
                self.drawer = None;
            }
        }
    }

    fn scene_tree(&mut self, ui: &mut egui::Ui) {
        let scene = self.scene();
        let selected = self.selected();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong(tr("Scene"));
            ui.weak(crate::lf!(
                "{} objects",
                "{} 个物体",
                scene.entities.iter().filter(|e| e.parent.is_none()).count()
            ));
            if scene.entities.len() > 8 {
                ui.add(
                    egui::TextEdit::singleline(&mut self.tree_filter)
                        .hint_text(tr("Filter…"))
                        .desired_width(ui.available_width()),
                );
            }
        });
        let filter = self.tree_filter.trim().to_lowercase();
        // with a filter, show the matching objects flat
        let flat: Option<Vec<&shotstage_core::Entity>> = (!filter.is_empty()).then(|| {
            scene
                .entities
                .iter()
                .filter(|e| !rig::is_joint(&scene, &e.id) && e.name.to_lowercase().contains(&filter))
                .collect()
        });
        let mut ctx = TreeCtx {
            scene: &scene,
            selected: &selected,
            also: &self.also,
            renaming: &mut self.renaming,
            acts: Vec::new(),
        };
        egui::ScrollArea::vertical().id_salt("scene_tree").auto_shrink([false, false]).show(ui, |ui| {
            match &flat {
                Some(list) => {
                    for e in list {
                        tree_row(ui, &mut ctx, e);
                    }
                    if list.is_empty() {
                        ui.weak(tr("Nothing matches."));
                    }
                }
                None => tree_level(ui, &mut ctx, None),
            }
            if scene.entities.is_empty() {
                ui.weak(tr("Empty scene. Add blocks above, or drive it from the console / API."));
            }
        });
        let acts = std::mem::take(&mut ctx.acts);
        for act in acts {
            match act {
                TreeAct::Pick(id, false) => {
                    self.also.clear();
                    self.select(Some(id));
                }
                TreeAct::Pick(id, true) => match self.selected() {
                    None => self.select(Some(id)),
                    Some(p) if p == id => {}
                    Some(_) => match self.also.iter().position(|a| *a == id) {
                        Some(i) => {
                            self.also.remove(i);
                        }
                        None => self.also.push(id),
                    },
                },
                TreeAct::Visible(id, v) => _ = self.run("entity.setVisible", json!({"id": id, "visible": v})),
                TreeAct::Locked(id, v) => _ = self.run("entity.setLocked", json!({"id": id, "locked": v})),
                TreeAct::Rename(id, name) => {
                    let name = name.trim();
                    if !name.is_empty() {
                        self.run("entity.rename", json!({"id": id, "name": name}));
                    }
                }
                TreeAct::Focus(id) => {
                    self.also.clear();
                    self.select(Some(id));
                    self.focus_selection();
                }
                TreeAct::Duplicate(id) => {
                    if let Some(r) = self.run("entity.duplicate", json!({"id": id})) {
                        self.select(r["id"].as_str().map(str::to_string));
                    }
                }
                TreeAct::Delete(id) => _ = self.run("entity.delete", json!({ "id": id })),
            }
        }
    }

    /// The selection's name as a heading; click it to rename.
    fn name_header(&mut self, ui: &mut egui::Ui, id: &str, name: &str, editable: bool) {
        if let Some((eid, buf)) = self.name_edit.as_mut()
            && eid == id
        {
            let r = ui.add(egui::TextEdit::singleline(buf).font(egui::TextStyle::Heading).desired_width(f32::INFINITY));
            // (lost_focus is read before asking for focus again, or it never reports)
            if r.lost_focus() {
                let new = buf.trim().to_string();
                self.name_edit = None;
                if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && !new.is_empty() && new != name {
                    self.run("entity.rename", json!({"id": id, "name": new}));
                }
            } else if !r.has_focus() {
                r.request_focus();
            }
            return;
        }
        let r = ui.add(egui::Label::new(egui::RichText::new(name).heading()).sense(if editable {
            Sense::click()
        } else {
            Sense::hover()
        }));
        if editable && r.on_hover_cursor(CursorIcon::Text).on_hover_text(tr("Click to rename")).clicked() {
            self.name_edit = Some((id.to_string(), name.to_string()));
        }
    }

    fn properties(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let Some(id) = self.selected() else {
            ui.weak(tr("Nothing selected."));
            ui.add_space(6.0);
            ui.label(tr("• Drag a block from the left into the scene."));
            ui.label(tr("• Click an object to select it; drag it to slide it over surfaces."));
            ui.label(tr("• Click a person, then a body part, to pose it."));
            ui.label(tr("• Drag empty space (or two fingers on a trackpad) to orbit; pinch or wheel to zoom."));
            ui.add_space(8.0);
            section(ui, "Sky & shadows", true, |ui| self.world_panel(ui));
            return;
        };
        let scene = self.scene();
        let Ok(e) = scene.get(&id) else { return };
        let frame = self.frame();
        if !self.also.is_empty() {
            self.multi_section(ui, &scene, &id);
            return;
        }
        let joint = rig::joint_name(&scene, &id);
        let person = rig::person_root(&scene, &id).ok().filter(|r| *r == id);
        let kind = match (&e.kind, joint, &person) {
            (_, Some(_), _) => "joint",
            (_, _, Some(_)) => "person",
            (EntityKind::Empty, ..) if scene.children(Some(&id)).next().is_some() => "group",
            (k, ..) => k.type_name(),
        };
        self.name_header(ui, &id, &e.name, joint.is_none());
        let dims = matches!(e.kind, EntityKind::Mesh { .. } | EntityKind::Empty | EntityKind::Image(_))
            .then(|| spatial::bounds(&scene, &id, frame).ok().flatten())
            .flatten()
            .map(|b| {
                let s = b.size();
                format!(" · {:.2} × {:.2} × {:.2} m", s.x, s.y, s.z)
            })
            .unwrap_or_default();
        ui.weak(format!("{} · {}{dims}", tr(kind), e.id));
        // edit mode: the mesh tools come first, the object panels wait until you are done
        if self.edit_state().is_some_and(|s| s.id == id) {
            ui.add_space(4.0);
            section(ui, "Edit mesh", true, |ui| self.edit_panel(ui, &id));
            return;
        }
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            let mut visible = e.visible;
            if ui.checkbox(&mut visible, tr("Visible")).changed() {
                self.run("entity.setVisible", json!({"id": id, "visible": visible}));
            }
            if joint.is_none() {
                let mut locked = e.locked;
                if ui
                    .checkbox(&mut locked, tr("Locked"))
                    .on_hover_text(tr("Drags in the viewport pass through it"))
                    .changed()
                {
                    self.run("entity.setLocked", json!({"id": id, "locked": locked}));
                }
                if ui.button(tr("Duplicate")).on_hover_text(tr("Ctrl/Cmd+D")).clicked()
                    && let Some(r) = self.run("entity.duplicate", json!({"id": id}))
                {
                    self.select(r["id"].as_str().map(str::to_string));
                }
                if ui.button(tr("Delete")).on_hover_text(tr("Delete / Backspace")).clicked() {
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
            section(ui, &tr("Place"), true, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button(tr("Drop to surface")).on_hover_text(tr("entity.place")).clicked() {
                        self.run("entity.place", json!({"id": id, "frame": frame}));
                    }
                    // props can be held by a person (they follow the hand)
                    if person.is_none() {
                        let held_by = e.parent.as_ref().filter(|p| rig::is_joint(&scene, p)).cloned();
                        match held_by {
                            Some(_) => {
                                if ui
                                    .button(tr("Let go"))
                                    .on_hover_text(tr("Stop following the hand (stays where it is)"))
                                    .clicked()
                                {
                                    self.run("entity.attach", json!({"id": id, "to": null}));
                                }
                            }
                            None => {
                                let people: Vec<(String, String)> = scene
                                    .entities
                                    .iter()
                                    .filter(|x| {
                                        x.parent.is_none() && rig::person_root(&scene, &x.id).is_ok_and(|r| r == x.id)
                                    })
                                    .map(|x| (x.id.clone(), x.name.clone()))
                                    .collect();
                                if !people.is_empty() {
                                    ui.menu_button(tr("Hold in hand"), |ui| {
                                        for (pid, pname) in &people {
                                            for (hand, label) in [("right", "right hand"), ("left", "left hand")] {
                                                if ui.button(format!("{pname} · {}", tr(label))).clicked() {
                                                    self.run(
                                                        "entity.attach",
                                                        json!({"id": id, "to": pid, "hand": hand}),
                                                    );
                                                    self.toast(tr(
                                                        "It now follows the hand: pose or animate the person",
                                                    ));
                                                    ui.close();
                                                }
                                            }
                                        }
                                    });
                                }
                            }
                        }
                    }
                    if ui.button(tr("↺ 45°")).on_hover_text(tr("R")).clicked() {
                        self.run("entity.rotate", json!({"id": id, "degrees": 45, "frame": frame}));
                    }
                    if ui.button(tr("↻ 45°")).on_hover_text(tr("Shift+R")).clicked() {
                        self.run("entity.rotate", json!({"id": id, "degrees": -45, "frame": frame}));
                    }
                    if let Some(cam) = camera_id_at(&scene, frame)
                        && ui.button(tr("Face camera")).on_hover_text(tr("entity.face")).clicked()
                    {
                        self.run("entity.face", json!({"id": id, "target": cam, "frame": frame}));
                    }
                });
            });
            section(ui, &tr("Frame it"), true, |ui| {
                ui.weak(tr("Moves the active camera (or adds one) and looks through it."));
                egui::Grid::new("frame_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label(tr("Angle"));
                    egui::ComboBox::from_id_salt("frame_angle")
                        .width(130.0)
                        .selected_text(tr(self.frame_angle))
                        .show_ui(ui, |ui| {
                            for a in ["eyeLevel", "high", "low", "birdsEye", "wormsEye"] {
                                ui.selectable_value(&mut self.frame_angle, a, tr(a));
                            }
                        });
                    ui.end_row();
                    ui.label(tr("Side"));
                    egui::ComboBox::from_id_salt("frame_side").width(130.0).selected_text(tr(self.frame_side)).show_ui(
                        ui,
                        |ui| {
                            for a in
                                ["front", "frontLeft", "left", "backLeft", "back", "backRight", "right", "frontRight"]
                            {
                                ui.selectable_value(&mut self.frame_side, a, tr(a));
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
                        if ui.button(tr(label)).on_hover_text(tr(size)).clicked() {
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
        section(ui, &tr("Transform"), plain, |ui| {
            let animated = self.keyed(&scene, &id);
            let t = local_at(&scene, &id, frame).unwrap_or(e.transform);
            let (mut pos, mut rot, mut scale) = (t.translation, t.euler_degrees(), t.scale);
            let (mut changed, mut started) = (false, false);
            egui::Grid::new("transform_grid").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
                for (label, v, speed) in
                    [("Position", &mut pos, 0.02), ("Rotation", &mut rot, 0.5), ("Scale", &mut scale, 0.01)]
                {
                    ui.label(tr(label));
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
                if ui.button(crate::lf!("Key @ {frame:.0}", "关键帧 @ {frame:.0}")).clicked() {
                    self.run("entity.setTransform", json!({"id": id, "position": pos, "rotationEuler": rot, "scale": scale, "keyframeAt": frame.round()}));
                }
                if animated && ui.button(tr("Delete key")).clicked() {
                    self.run("keyframe.delete", json!({"id": id, "property": "transform", "frame": frame.round()}));
                }
            });
            if animated {
                ui.weak(tr("Animated: edits are keyed at the current frame."));
            }
        });

        match &e.kind {
            EntityKind::Mesh { modifiers, color, source, material } => {
                if !matches!(
                    source,
                    shotstage_core::MeshSource::Skinned { .. } | shotstage_core::MeshSource::Human { .. }
                ) {
                    section(ui, "Edit mesh", true, |ui| self.edit_panel(ui, &id));
                    section(ui, "Boolean", false, |ui| self.boolean_panel(ui, &id));
                }
                section(ui, "Mesh", true, |ui| {
                    let mut c = *color;
                    ui.horizontal(|ui| {
                        ui.label(tr("Colour"));
                        let r = ui.color_edit_button_rgb(&mut c);
                        if r.clicked() {
                            self.gesture += 1;
                        }
                        if r.changed() && c != *color {
                            self.run_merged(&format!("color:{id}"), "mesh.setColor", json!({"id": id, "color": c}));
                        }
                    });
                    if let Some(stats) = self.run("mesh.stats", json!({"id": id})) {
                        ui.weak(crate::lf!(
                            "{} verts · {} faces · {} tris",
                            "{} 顶点 · {} 面 · {} 三角面",
                            stats["vertices"],
                            stats["faces"],
                            stats["triangles"]
                        ));
                    }
                    for (i, m) in modifiers.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let label = match m {
                                Modifier::Mirror { axis } => {
                                    crate::lf!("Mirror {}", "镜像 {}", ["X", "Y", "Z"][(*axis).min(2) as usize])
                                }
                                Modifier::Array { count, offset } => {
                                    format!(
                                        "{} ×{count} [{:.2}, {:.2}, {:.2}]",
                                        tr("Array"),
                                        offset[0],
                                        offset[1],
                                        offset[2]
                                    )
                                }
                                Modifier::Subdivide { levels, smooth } => {
                                    format!(
                                        "{} {levels}{}",
                                        tr("Subdivide"),
                                        if *smooth { tr(" smooth") } else { String::new() }
                                    )
                                }
                            };
                            ui.label(tr(label));
                            if ui.small_button(tr("✕")).clicked() {
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
                            if ui.button(tr(label)).clicked() {
                                self.run("mesh.addModifier", json!({"id": id, "modifier": m}));
                            }
                        }
                        if !modifiers.is_empty() && ui.button(tr("Apply all")).clicked() {
                            self.run("mesh.applyModifiers", json!({"id": id}));
                        }
                        if ui.button(tr("Origin to centre")).clicked() {
                            self.run("mesh.setOriginToCenter", json!({"id": id}));
                        }
                    });
                });
                section(ui, "Texture", material.is_some(), |ui| self.material_panel(ui, &id, material.as_ref()));
            }
            EntityKind::Camera(_) => {
                section(ui, &tr("Camera"), true, |ui| {
                    let lens = camera_data_at(&scene, &id, frame).unwrap_or_default();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr("Frame"));
                        for (label, a, tip) in [
                            ("16:9", 16.0 / 9.0, "Landscape video"),
                            ("9:16", 9.0 / 16.0, "Vertical video: Reels, TikTok, Shorts"),
                            ("1:1", 1.0, "Square"),
                            ("4:5", 0.8, "Portrait feed post"),
                            ("4:3", 4.0 / 3.0, "Classic"),
                            ("2.39:1", 2.39, "Cinemascope"),
                        ] {
                            let on = (lens.aspect - a).abs() < 0.01;
                            if ui.selectable_label(on, tr(label)).on_hover_text(tr(tip)).clicked() {
                                self.run("camera.setLens", json!({"id": id, "aspect": a}));
                            }
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(tr("Match current view"))
                            .on_hover_text(tr("Put this camera where you are looking from in the view"))
                            .clicked()
                        {
                            // one undo step: move, aim, and key it if it is animated
                            let eye = self.orbit.eye();
                            let mut steps = vec![
                                json!({"command": "entity.moveTo", "params": {"id": id, "position": eye.to_array(), "frame": frame}}),
                                json!({"command": "camera.lookAt", "params": {"id": id, "target": self.orbit.target.to_array()}}),
                            ];
                            if self.keyed(&scene, &id) {
                                steps.push(json!({"command": "keyframe.set", "params": {"id": id, "property": "transform", "frame": frame.round()}}));
                            }
                            if let Err(e) = hub::dispatch(&self.hub, json!({"batch": steps})) {
                                self.toast(crate::lf!("Could not match the view: {}", "无法匹配视角：{}", e.message));
                            }
                        }
                        ui.checkbox(&mut self.safe_areas, tr("Safe areas"))
                            .on_hover_text(tr("Action / title safe frames in the camera view"));
                    });
                    // Dutch angle: roll the camera about its view axis
                    let t = local_at(&scene, &id, frame).unwrap_or(e.transform);
                    let mut e3 = t.euler_degrees();
                    ui.horizontal(|ui| {
                        ui.label(tr("Roll"));
                        let r = ui
                            .add(egui::Slider::new(&mut e3[2], -45.0..=45.0).suffix("°").max_decimals(0))
                            .on_hover_text(tr("Tilt the horizon (a Dutch angle)"));
                        if r.drag_started() {
                            self.gesture += 1;
                        }
                        if r.changed() {
                            let mut p = json!({"id": id, "rotationEuler": e3});
                            if self.keyed(&scene, &id) {
                                p["keyframeAt"] = json!(frame.round());
                            }
                            self.run_merged(&format!("roll:{id}"), "entity.setTransform", p);
                        }
                        if ui.small_button(tr("Level")).clicked() {
                            e3[2] = 0.0;
                            self.run("entity.setTransform", json!({"id": id, "rotationEuler": e3}));
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr("Reference"));
                        if ui
                            .button(tr("Load picture…"))
                            .on_hover_text(tr(
                                "Lay a storyboard frame or a reference photo over this camera's view to match it",
                            ))
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
                            if ui.small_button(tr("✕")).on_hover_text(tr("Remove the reference")).clicked() {
                                self.run("camera.setReference", json!({"camera": id, "clear": true}));
                            }
                        }
                    });
                    let mut focal = lens.focal_length_mm;
                    let focal_animated = scene.timeline.track(&id, TrackProperty::FocalLength).is_some();
                    ui.horizontal(|ui| {
                        ui.label(tr("Focal length"));
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
                        if ui.button(tr("Key")).clicked() {
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
                    ui.weak(crate::lf!(
                        "{:.1}° horizontal FOV, aspect {:.3}",
                        "水平视角 {:.1}°，画幅比 {:.3}",
                        lens.hfov().to_degrees(),
                        lens.aspect
                    ));
                    ui.horizontal(|ui| {
                        let active = scene.active_camera.as_deref() == Some(id.as_str());
                        if ui.add_enabled(!active, egui::Button::new(tr("Make active"))).clicked() {
                            self.run("camera.setActive", json!({"id": id}));
                        }
                        if ui.button(tr("Look at origin")).clicked() {
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
                section(ui, &tr("Image"), true, |ui| {
                    let mut billboard = img.billboard;
                    if ui
                        .checkbox(&mut billboard, tr("Always face the camera (billboard)"))
                        .on_hover_text(tr("For cut-out people, trees and signs"))
                        .changed()
                    {
                        self.run("image.set", json!({"id": id, "billboard": billboard}));
                    }
                    let mut h = img.size[1];
                    ui.horizontal(|ui| {
                        ui.label(tr("Height"));
                        let r = ui.add(egui::DragValue::new(&mut h).range(0.01..=500.0).speed(0.01).suffix(" m"));
                        if r.drag_started() {
                            self.gesture += 1;
                        }
                        if r.changed() {
                            self.run_merged(&format!("img:{id}"), "image.set", json!({"id": id, "height": h}));
                        }
                    });
                    if let Some(a) = scene.assets.get(&img.asset) {
                        ui.weak(crate::lf!(
                            "{} × {} px · {:.2} × {:.2} m",
                            "{} × {} 像素 · {:.2} × {:.2} 米",
                            a.width,
                            a.height,
                            img.size[0],
                            img.size[1]
                        ));
                    }
                });
            }
            EntityKind::Empty => {}
        }
    }

    /// A bone of an imported skeleton: plain rotation sliders.
    fn bone_section(&mut self, ui: &mut egui::Ui, scene: &Scene, id: &str, frame: f32) {
        let animated = self.keyed(scene, id);
        section(ui, &tr("Bone"), true, |ui| {
            ui.weak(tr("Drag the model's surface in the view to bend this bone, or turn the coloured rings."));
            let mut r = local_at(scene, id, frame).map(|t| t.euler_degrees()).unwrap_or_default();
            let (mut changed, mut started) = (false, false);
            egui::Grid::new("bone_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                for (i, label) in ["X", "Y", "Z"].iter().enumerate() {
                    ui.label(tr(*label));
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
            if ui.button(tr("Select model")).clicked() {
                self.select(Some(root_of(scene, id)));
            }
        });
    }

    fn light_section(&mut self, ui: &mut egui::Ui, id: &str, l: &shotstage_core::LightData) {
        use shotstage_core::LightKind;
        section(ui, &tr("Light"), true, |ui| {
            let mut n = *l;
            let mut started = false;
            egui::Grid::new("light_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label(tr("Kind"));
                ui.horizontal(|ui| {
                    for (k, label) in [
                        (LightKind::Sun, "Sun"),
                        (LightKind::Point, "Point light"),
                        (LightKind::Spot, "Spot"),
                        (LightKind::Area, "Area"),
                    ] {
                        ui.selectable_value(&mut n.kind, k, tr(label));
                    }
                });
                ui.end_row();
                ui.label(tr("Colour"));
                ui.horizontal(|ui| {
                    started |= ui.color_edit_button_rgb(&mut n.color).clicked();
                    for (label, c) in
                        [("Warm", [1.0, 0.8, 0.6]), ("Neutral", [1.0, 1.0, 1.0]), ("Cool", [0.75, 0.85, 1.0])]
                    {
                        if ui.small_button(tr(label)).clicked() {
                            n.color = c;
                        }
                    }
                });
                ui.end_row();
                ui.label(tr("Temperature"));
                ui.horizontal(|ui| {
                    for (k, label, tip) in [
                        (2700.0, "2700K", "Warm bulb"),
                        (3200.0, "3200K", "Tungsten film light"),
                        (4500.0, "4500K", "Shop fluorescent"),
                        (5600.0, "5600K", "Daylight, flash"),
                        (7500.0, "7500K", "Shade, overcast"),
                    ] {
                        if ui.small_button(tr(label)).on_hover_text(tr(tip)).clicked() {
                            n.color = shotstage_core::light::kelvin_to_rgb(k);
                        }
                    }
                });
                ui.end_row();
                ui.label(tr("Intensity"));
                let r = ui.add(egui::Slider::new(&mut n.intensity, 0.0..=20.0).logarithmic(true).max_decimals(2));
                started |= r.drag_started();
                ui.end_row();
                ui.label(tr("Shadows"));
                ui.checkbox(&mut n.cast_shadows, tr("Casts shadows"))
                    .on_hover_text(tr("Up to four lights cast shadows at once (the brightest)"));
                ui.end_row();
                if n.kind == LightKind::Spot {
                    ui.label(tr("Beam angle"));
                    started |= ui.add(egui::Slider::new(&mut n.cone_degrees, 5.0..=170.0).suffix("°")).drag_started();
                    ui.end_row();
                }
                if matches!(n.kind, LightKind::Spot | LightKind::Area) {
                    ui.label(tr("Softness"));
                    started |= ui.add(egui::Slider::new(&mut n.softness, 0.0..=1.0)).drag_started();
                    ui.end_row();
                }
                if n.kind == LightKind::Area {
                    ui.label(tr("Size"));
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
                        "coneDegrees": n.cone_degrees, "softness": n.softness, "size": n.size,
                        "castShadows": n.cast_shadows}),
                );
            }
            ui.horizontal_wrapped(|ui| {
                if let Some(target) = self.light_target()
                    && ui.button(tr("Aim at selection's group")).on_hover_text(tr("camera.lookAt")).clicked()
                {
                    self.run("camera.lookAt", json!({"id": id, "target": target}));
                }
                if ui.button(tr("Aim at origin")).clicked() {
                    self.run("camera.lookAt", json!({"id": id, "target": [0, 0.8, 0]}));
                }
            });
            ui.weak(tr("A light shines along its -Z. Scenes with lights are lit by them; toggle Studio light in the view toolbar to compare. The sky and shadow settings are under Sky ▾."));
        });
    }

    /// The sky around the scene and the shadow settings (environment.set, light.shadows).
    fn world_panel(&mut self, ui: &mut egui::Ui) {
        use shotstage_core::Sky;
        use shotstage_core::environment::SKIES;
        let scene = self.scene();
        let e = scene.environment.clone();
        let mut n = e.clone();
        let mut started = false;
        egui::Grid::new("world_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label(tr("Sky"));
            let name = |sky: Sky| {
                SKIES.iter().find(|(s, ..)| *s == sky).map_or(String::new(), |(_, key, _)| match sky {
                    Sky::Hdri => "HDRI".to_string(),
                    _ => {
                        let mut c = key.chars();
                        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
                    }
                })
            };
            egui::ComboBox::from_id_salt("sky_kind").selected_text(tr(name(n.sky))).width(150.0).show_ui(ui, |ui| {
                for (sky, _, desc) in SKIES {
                    if *sky == Sky::Hdri && n.hdri.is_none() {
                        continue;
                    }
                    ui.selectable_value(&mut n.sky, *sky, tr(name(*sky))).on_hover_text(tr(*desc));
                }
            });
            ui.end_row();
            ui.label(tr(""));
            ui.horizontal(|ui| {
                if ui
                    .button(tr("Import HDRI…"))
                    .on_hover_text(tr(
                        "An .hdr file or a 2:1 panorama picture (you can also drop an .hdr onto the view)",
                    ))
                    .clicked()
                {
                    self.sky_next = true;
                    crate::files::pick(&self.incoming, ui.ctx());
                }
                if e.lights_scene()
                    && ui
                        .button(tr("Add matching sun"))
                        .on_hover_text(tr(
                            "A sun light from the sky's brightest spot, so shadows match (light.fromEnvironment)",
                        ))
                        .clicked()
                {
                    match self.run("light.fromEnvironment", json!({})) {
                        Some(r) => {
                            let id = r["id"].as_str().map(str::to_string);
                            self.select(id);
                        }
                        None => self.toast("This sky has no clear sun"),
                    }
                }
            });
            ui.end_row();
            if n.sky != Sky::Studio {
                ui.label(tr(if n.sky == Sky::Color { "Colour" } else { "Tint" }));
                ui.horizontal(|ui| {
                    started |= ui.color_edit_button_rgb(&mut n.color).clicked();
                    let presets: &[(&str, [f32; 3])] = if n.sky == Sky::Color {
                        &[
                            ("White", [1.0; 3]),
                            ("Grey", [0.35, 0.35, 0.36]),
                            ("Black", [0.0; 3]),
                            ("Green", [0.05, 0.6, 0.15]),
                        ]
                    } else {
                        &[("None", [1.0; 3]), ("Warm", [1.0, 0.85, 0.7]), ("Cool", [0.8, 0.88, 1.0])]
                    };
                    for (label, c) in presets {
                        if ui.small_button(tr(*label)).clicked() {
                            n.color = *c;
                        }
                    }
                });
                ui.end_row();
                ui.label(tr("Sky light"));
                started |= ui
                    .add(egui::Slider::new(&mut n.intensity, 0.0..=3.0).max_decimals(2))
                    .on_hover_text(tr("How brightly the sky lights the scene (the background keeps its look)"))
                    .drag_started();
                ui.end_row();
                if n.sky != Sky::Color {
                    ui.label(tr("Turn"));
                    started |= ui
                        .add(egui::Slider::new(&mut n.rotation, 0.0..=360.0).suffix("°"))
                        .on_hover_text(tr("Turn the sky and its sun about the vertical axis"))
                        .drag_started();
                    ui.end_row();
                }
                ui.label(tr("Background"));
                ui.checkbox(&mut n.background, tr("Show the sky behind the scene"));
                ui.end_row();
            }
            ui.label(tr("Shadows"));
            ui.checkbox(&mut n.shadows.enabled, tr("On"));
            ui.end_row();
            if n.shadows.enabled {
                ui.label(tr("Darkness"));
                started |= ui.add(egui::Slider::new(&mut n.shadows.strength, 0.0..=1.0)).drag_started();
                ui.end_row();
                ui.label(tr("Softness"));
                started |= ui.add(egui::Slider::new(&mut n.shadows.softness, 0.0..=1.0)).drag_started();
                ui.end_row();
            }
        });
        if started {
            self.gesture += 1;
        }
        if n.shadows != e.shadows {
            let s = n.shadows;
            self.run_merged(
                "shadows",
                "light.shadows",
                json!({"enabled": s.enabled, "strength": s.strength, "softness": s.softness}),
            );
        }
        if n.sky != e.sky
            || n.color != e.color
            || n.intensity != e.intensity
            || n.rotation != e.rotation
            || n.background != e.background
        {
            self.run_merged(
                "sky",
                "environment.set",
                json!({"sky": n.sky, "color": n.color, "intensity": n.intensity, "rotation": n.rotation, "background": n.background}),
            );
        }
        ui.weak(tr("The sky lights everything softly from all round and adds to your lights; with no lights it lights the scene alone. Lighting presets set a matching sky."));
    }

    /// What a light should aim at: the subject last framed, else nothing.
    fn light_target(&self) -> Option<Value> {
        self.last_subject.as_ref().filter(|s| self.scene().contains(s)).map(|s| json!(s))
    }

    /// Several objects selected: group, align, distribute, delete.
    fn multi_section(&mut self, ui: &mut egui::Ui, scene: &Scene, primary: &str) {
        let ids: Vec<String> = std::iter::once(primary.to_string()).chain(self.also.iter().cloned()).collect();
        ui.heading(crate::lf!("{} objects", "{} 个物体", ids.len()));
        ui.weak(
            ids.iter().map(|i| scene.get(i).map(|e| e.name.clone()).unwrap_or_default()).collect::<Vec<_>>().join(", "),
        );
        ui.weak(tr("Shift+click adds or removes objects; click one alone to select just it."));
        ui.add_space(4.0);
        section(ui, &tr("Arrange"), true, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr("Group")).on_hover_text(tr("Ctrl/Cmd+G")).clicked() {
                    self.group_selection();
                }
                if ui.button(tr("Delete all")).clicked() {
                    let steps: Vec<Value> =
                        ids.iter().map(|i| json!({"command": "entity.delete", "params": {"id": i}})).collect();
                    let _ = hub::dispatch(&self.hub, json!({"batch": steps}));
                    self.also.clear();
                }
            });
            egui::Grid::new("align_grid").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
                for (axis, label) in [("x", "Align X"), ("z", "Align Z"), ("y", "Align Y")] {
                    ui.label(tr(label));
                    for (to, t) in [("min", "min"), ("center", "centre"), ("max", "max")] {
                        if ui.small_button(tr(t)).clicked() {
                            self.run("entity.align", json!({"ids": ids, "axis": axis, "to": to}));
                        }
                    }
                    ui.end_row();
                }
            });
            if ids.len() >= 3 {
                ui.horizontal(|ui| {
                    ui.label(tr("Distribute"));
                    for axis in ["x", "z"] {
                        if ui.small_button(tr(axis.to_uppercase())).clicked() {
                            self.run("entity.distribute", json!({"ids": ids, "axis": axis}));
                        }
                    }
                });
            }
        });
    }

    fn group_selection(&mut self) {
        let Some(primary) = self.selected() else { return };
        let ids: Vec<String> = std::iter::once(primary).chain(self.also.drain(..)).collect();
        match hub::dispatch(&self.hub, json!({"command": "entity.group", "params": {"ids": ids}})) {
            Ok(o) => {
                self.select(o.result["id"].as_str().map(str::to_string));
                self.toast(tr("Grouped: they now move together (Ctrl+Shift+G to ungroup)"));
            }
            Err(e) => self.toast(crate::lf!("Could not group: {}", "无法编组：{}", e.message)),
        }
    }

    fn person_section(&mut self, ui: &mut egui::Ui, scene: &Scene, root: &str, frame: f32) {
        let animated = self.auto_key || rig::JOINTS.iter().any(|j| is_animated(scene, &format!("{root}_{j}")));
        section(ui, "Pose", true, |ui| {
            ui.weak(tr("Click a body part to select its joint, then drag it (or the rings) to bend it."));
            let apply = |this: &mut Self, name: &str| {
                let mut req = json!({"id": root, "pose": name, "frame": frame});
                if animated {
                    req["keyframeAt"] = json!(frame.round());
                }
                this.run("pose.set", req);
            };
            ui.horizontal_wrapped(|ui| {
                for p in rig::POSES.iter().filter(|p| COMMON_POSES.contains(&p.name)) {
                    if ui.button(tr(humanize(p.name))).on_hover_text(tr(p.description)).clicked() {
                        apply(self, p.name);
                    }
                }
                ui.menu_button(tr("More poses ▾"), |ui| {
                    ui.set_max_height(420.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for p in rig::POSES.iter().filter(|p| !COMMON_POSES.contains(&p.name)) {
                            if ui.button(tr(humanize(p.name))).on_hover_text(tr(p.description)).clicked() {
                                apply(self, p.name);
                                ui.close();
                            }
                        }
                    });
                });
            });
        });
        if let Some(cam) = camera_id_at(scene, frame) {
            ui.horizontal(|ui| {
                if ui
                    .button(tr("Look at camera"))
                    .on_hover_text(tr("Turn the head and neck towards the shot camera"))
                    .clicked()
                {
                    let mut req = json!({"id": root, "target": cam, "frame": frame});
                    if animated {
                        req["keyframeAt"] = json!(frame.round());
                    }
                    self.run("person.lookAt", req);
                }
            });
        }
        if let Some((body, outfit)) = humans::human_of(scene, root) {
            self.human_sections(ui, scene, root, frame, body, outfit, animated);
            return;
        }
        section(ui, "Hair", true, |ui| {
            ui.horizontal_wrapped(|ui| {
                for style in rig::HAIR_STYLES {
                    if ui.button(tr(*style)).clicked() {
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
                    ui.label(tr("Colour"));
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

    /// Body, clothes, hair, face and hands of a realistic person.
    #[allow(clippy::too_many_arguments)]
    fn human_sections(
        &mut self,
        ui: &mut egui::Ui,
        scene: &Scene,
        root: &str,
        frame: f32,
        body: shotstage_core::human::HumanBody,
        outfit: shotstage_core::human::HumanOutfit,
        animated: bool,
    ) {
        let part_color = |part: &str| {
            scene.get(&format!("{root}_{part}")).ok().and_then(|e| match &e.kind {
                EntityKind::Mesh { color, .. } => Some(*color),
                _ => None,
            })
        };
        section(ui, "Hands", false, |ui| {
            ui.horizontal_wrapped(|ui| {
                for h in humans::HAND_SHAPES {
                    if ui.small_button(tr(humanize(h.name))).on_hover_text(tr(h.description)).clicked() {
                        let mut req = json!({"id": root, "shape": h.name, "frame": frame});
                        if animated {
                            req["keyframeAt"] = json!(frame.round());
                        }
                        self.run("pose.hand", req);
                    }
                }
            });
        });
        section(ui, &crate::lf!("Face", "面部"), true, |ui| {
            // with expression keys the face follows the timeline: show and key what is there
            let keyed = scene
                .timeline
                .track(root, TrackProperty::Expression)
                .and_then(|t| t.sample(frame))
                .and_then(|v| if let shotstage_core::KeyValue::Expression(m) = v { Some(m) } else { None });
            let keyed_now = keyed.map(|m| if m.a_amount >= m.b_amount { m.a } else { m.b });
            ui.horizontal_wrapped(|ui| {
                for (i, e) in shotstage_core::human::EXPRESSIONS.iter().enumerate() {
                    let on = match keyed_now {
                        Some(k) => k as usize == i,
                        None => {
                            (!body.face.is_empty()
                                && e.units.len() == body.face.len()
                                && e.units.iter().all(|(u, w)| body.face.get(*u).is_some_and(|v| (v - w).abs() < 1e-3)))
                                || (e.units.is_empty() && body.face.is_empty())
                        }
                    };
                    if ui.selectable_label(on, tr(humanize(e.name))).on_hover_text(tr(e.description)).clicked() {
                        let mut req = json!({"id": root, "expression": e.name});
                        if animated || keyed.is_some() {
                            req["keyframeAt"] = json!(frame.round());
                        }
                        self.run("person.setExpression", req);
                    }
                }
            });
            if keyed.is_some() {
                ui.weak(tr("Keyed: the face cross-fades between expression keys."));
            }
        });
        section(ui, "Body", true, |ui| {
            egui::Grid::new("human_body").num_columns(2).show(ui, |ui| {
                let mut b = body.clone();
                let mut started = false;
                let mut changed = false;
                let mut row = |ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, tip: &str| {
                    ui.label(tr(label)).on_hover_text(tr(tip));
                    let r = ui.add(egui::Slider::new(v, range).max_decimals(2));
                    started |= r.drag_started();
                    changed |= r.changed();
                    ui.end_row();
                };
                row(ui, "Female ↔ male", &mut b.gender, 0.0..=1.0, "0 = female, 1 = male");
                row(ui, "Age", &mut b.age, 11.0..=90.0, "Years");
                row(ui, "Weight", &mut b.weight, -1.0..=1.0, "Thin … heavy");
                row(ui, "Muscle", &mut b.muscle, -1.0..=1.0, "Slight … muscular");
                row(ui, "Height", &mut b.height, -1.0..=1.0, "Short … tall");
                if started {
                    self.gesture += 1;
                }
                if changed {
                    self.run_merged(
                        &format!("body:{root}"),
                        "person.setBody",
                        json!({"id": root, "gender": b.gender, "age": b.age, "weight": b.weight, "muscle": b.muscle, "height": b.height}),
                    );
                }
            });
            let h = shotstage_core::human::rest(&body).height();
            ui.weak(crate::lf!("{:.2} m tall", "身高 {:.2} 米", h));
        });
        section(ui, "Clothes", true, |ui| {
            let mut o = outfit.clone();
            let mut changed = false;
            let mut started = false;
            egui::Grid::new("human_clothes").num_columns(3).show(ui, |ui| {
                changed |= ui.checkbox(&mut o.top, tr("Top")).changed();
                let r = ui.add_enabled(
                    o.top,
                    egui::Slider::new(&mut o.sleeves, 0.0..=2.0).text(tr("sleeves")).show_value(false),
                );
                started |= r.drag_started();
                changed |= r.changed();
                self.part_color_button(ui, root, "top", part_color("top"));
                ui.end_row();
                changed |= ui.checkbox(&mut o.bottom, tr("Bottom")).changed();
                let r = ui.add_enabled(
                    o.bottom,
                    egui::Slider::new(&mut o.legs, 0.0..=2.0).text(tr("legs")).show_value(false),
                );
                started |= r.drag_started();
                changed |= r.changed();
                self.part_color_button(ui, root, "bottom", part_color("bottom"));
                ui.end_row();
                changed |= ui.checkbox(&mut o.shoes, tr("Shoes")).changed();
                ui.label(tr(""));
                self.part_color_button(ui, root, "shoes", part_color("shoes"));
                ui.end_row();
                changed |= ui.checkbox(&mut o.apron, tr("Apron")).changed();
                ui.label(tr(""));
                self.part_color_button(ui, root, "apron", part_color("apron"));
                ui.end_row();
                ui.label(tr("Skin"));
                ui.label(tr(""));
                self.part_color_button(ui, root, "skin", part_color("skin"));
                ui.end_row();
                ui.label(tr("Eyes"));
                ui.label(tr(""));
                self.part_color_button(ui, root, "eyes", part_color("irises"));
                ui.end_row();
            });
            if started {
                self.gesture += 1;
            }
            if changed {
                self.run_merged(
                    &format!("outfit:{root}"),
                    "person.setOutfit",
                    json!({"id": root, "top": o.top, "sleeves": o.sleeves, "bottom": o.bottom, "legs": o.legs, "shoes": o.shoes, "apron": o.apron}),
                );
            }
        });
        section(ui, "Hair", true, |ui| {
            ui.horizontal_wrapped(|ui| {
                for style in rig::HAIR_STYLES {
                    if ui.button(tr(humanize(style))).clicked() {
                        self.run("person.setHair", json!({"id": root, "style": style}));
                    }
                }
            });
            ui.horizontal(|ui| {
                let mut len = outfit.hair;
                ui.label(tr("Length"));
                let r = ui.add(egui::Slider::new(&mut len, 0.0..=1.6).show_value(false));
                if r.drag_started() {
                    self.gesture += 1;
                }
                if r.changed() {
                    self.run_merged(&format!("hairlen:{root}"), "person.setOutfit", json!({"id": root, "hair": len}));
                }
                self.part_color_button(ui, root, "hair", part_color("hair"));
            });
        });
    }

    /// A colour button for one part of a realistic person (`person.setOutfit` colours).
    fn part_color_button(&mut self, ui: &mut egui::Ui, root: &str, key: &str, current: Option<[f32; 3]>) {
        let Some(c) = current else {
            ui.label(tr(""));
            return;
        };
        let mut c2 = c;
        let r = ui.color_edit_button_rgb(&mut c2);
        if r.clicked() {
            self.gesture += 1;
        }
        if c2 != c {
            self.run_merged(
                &format!("color:{root}:{key}"),
                "person.setOutfit",
                json!({"id": root, "colors": { key: c2 }}),
            );
        }
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
        section(ui, &tr("Joint"), true, |ui| {
            ui.weak(tr("Drag a body part in the view to bend it, or turn the coloured rings."));
            let mut r = local_at(scene, id, frame).map(|t| t.euler_degrees()).unwrap_or_default();
            let mut changed = false;
            let mut started = false;
            egui::Grid::new("joint_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                for (i, label) in ["Bend  X", "Twist  Y", "Side  Z"].iter().enumerate() {
                    ui.label(tr(*label));
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
                if ui.button(tr("Reset")).clicked() {
                    set(self, json!({ joint: [0, 0, 0] }), None);
                }
                let other = joint
                    .strip_suffix("_l")
                    .map(|b| format!("{b}_r"))
                    .or_else(|| joint.strip_suffix("_r").map(|b| format!("{b}_l")));
                if let Some(o) = other
                    && ui.button(crate::lf!("Mirror to {o}", "镜像到 {o}")).clicked()
                {
                    set(self, json!({ o: [r[0], -r[1], -r[2]] }), None);
                }
                if ui.button(tr("Select person")).clicked() {
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
        transport_row(ui, |ui| {
            if ui.button(tr("⏮")).on_hover_text(tr("To the start (Home)")).clicked() {
                self.hub.borrow_mut().frame = t.start as f32;
            }
            if ui
                .add_sized([62.0, 22.0], egui::Button::new(tr(if self.playing { "⏸ Pause" } else { "▶ Play" })))
                .on_hover_text(tr("Space"))
                .clicked()
            {
                self.playing = !self.playing;
            }
            if ui.button(tr("⏭")).on_hover_text(tr("To the end (End)")).clicked() {
                self.hub.borrow_mut().frame = t.end as f32;
            }
            let has_cam = camera_id_at(&scene, t.start as f32).is_some();
            if ui
                .add_enabled(has_cam, egui::Button::new(tr("🎥 Preview shot")))
                .on_hover_text(tr("Play from the start, looking through the shot camera"))
                .clicked()
            {
                self.hub.borrow_mut().frame = t.start as f32;
                self.look_through = true;
                self.hub.borrow_mut().look_through = true;
                self.playing = true;
            }
            if !t.shots.is_empty() {
                let has_cuts = !cuts(&scene).is_empty();
                if ui
                    .add_enabled(has_cuts, egui::Button::new(tr("⇤")).small())
                    .on_hover_text(tr("Previous cut ( [ )"))
                    .clicked()
                {
                    self.jump_cut(-1);
                }
                let upcoming = next_cut(&scene, self.frame()).or_else(|| cuts(&scene).into_iter().next());
                if ui
                    .add_enabled(upcoming.is_some(), egui::Button::new(tr("✂ Preview cut")).small())
                    .on_hover_text(tr(
                        "Play through the next cut: a moment before it to a moment after, through the shot cameras",
                    ))
                    .clicked()
                    && let Some((f, _)) = upcoming
                {
                    self.preview_cut(f);
                }
                if ui
                    .add_enabled(has_cuts, egui::Button::new(tr("⇥")).small())
                    .on_hover_text(tr("Next cut ( ] )"))
                    .clicked()
                {
                    self.jump_cut(1);
                }
                ui.toggle_value(&mut self.loop_shot, tr("🔁 Loop shot"))
                    .on_hover_text(tr("Playback repeats the shot under the playhead"));
            }
            ui.separator();
            ui.monospace(format!("{:>4.0}", self.frame()));
            ui.weak(format!("/ {} · {:.2} s", t.end, (self.frame() - t.start as f32) / fps));
            ui.separator();
            let sel = self.selected();
            if ui
                .add_enabled(sel.is_some(), egui::Button::new(tr("◆ Key")))
                .on_hover_text(tr("Keyframe the selection at this frame (K)"))
                .clicked()
            {
                self.key_selection();
            }
            ui.toggle_value(&mut self.auto_key, tr("Auto-key"))
                .on_hover_text(tr("Every move, turn or pose change is keyed at the current frame"));
            ui.separator();
            let (mut fps2, mut start, mut end) = (t.fps, t.start, t.end);
            let a = ui.add(egui::DragValue::new(&mut fps2).range(1.0..=120.0).suffix(" fps"));
            let b = ui.add(egui::DragValue::new(&mut start).range(0..=end));
            ui.label(tr("–"));
            let c = ui.add(egui::DragValue::new(&mut end).range(start..=100_000));
            if a.changed() || b.changed() || c.changed() {
                self.run("timeline.set", json!({"fps": fps2, "start": start, "end": end}));
            }
            // the selected camera, else the active one
            let sel_cam = self
                .selected()
                .filter(|id| matches!(scene.get(id).map(|e| &e.kind), Ok(EntityKind::Camera(_))))
                .or(scene.active_camera.clone());
            if let Some(cam) = sel_cam {
                let name = scene.get(&cam).map(|e| e.name.clone()).unwrap_or(cam.clone());
                let tip = if t.shots.is_empty() {
                    crate::lf!("A shot through {name} over the whole timeline", "通过 {name} 拍摄整条时间轴的镜头")
                } else {
                    crate::lf!(
                        "Cut to {name} at the playhead (select another camera to cut to it)",
                        "在播放头处切到 {name}（选中别的相机可切到它）"
                    )
                };
                if ui.small_button(tr("+ Shot")).on_hover_text(tr(tip)).clicked() {
                    if t.shots.is_empty() {
                        self.run("shot.create", json!({"camera": cam, "start": t.start, "end": t.end}));
                    } else if let Some(r) = self.run(
                        "shot.cut",
                        json!({"camera": cam, "frame": self.frame().round().max(0.0) as u32, "name": name}),
                    ) {
                        self.toast(crate::lf!("Cut to {name} at frame {}", "在第 {} 帧切到 {name}", r["start"]));
                    }
                }
            }
            if self.tl_view.is_some()
                && ui
                    .small_button(tr("Fit"))
                    .on_hover_text(tr(
                        "Show the whole timeline (or double-click the ruler; pinch or Cmd/Ctrl+scroll on it to zoom)",
                    ))
                    .clicked()
            {
                self.tl_view = None;
            }
            ui.toggle_value(&mut self.show_curves, tr("Curves")).on_hover_text(tr(
                "The selection's animation as curves: drag keys to change values, click a stretch to shape its easing",
            ));
            ui.toggle_value(&mut self.show_board, tr("Storyboard"))
                .on_hover_text(tr("Every shot as a picture through its camera; click one to go there"));
            ui.separator();
            ui.toggle_value(&mut self.show_console, tr("Console"))
                .on_hover_text(tr("Type commands: the same API as window.shotstage, the CLI and MCP"));
        });
        self.track_strip(ui, &scene);
        if self.show_console {
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong(tr("Command console"));
                ui.weak(tr("JSON request: {\"command\", \"params\"} or {\"batch\": [...]}"));
                if ui.small_button(tr("List commands")).clicked() {
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
                if cols[0].button(if crate::i18n::zh() { "运行" } else { "Run" }).clicked() {
                    self.console_out = match serde_json::from_str::<Value>(&self.console) {
                        Ok(req) => match hub::dispatch(&self.hub, req) {
                            Ok(o) => serde_json::to_string_pretty(&o.result).unwrap_or_default(),
                            Err(e) => format!("error {}: {}", e.code, e.message),
                        },
                        Err(e) => crate::lf!("not JSON: {e}", "不是 JSON：{e}"),
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
        let (full0, full1) = (t.start as f32, t.end.max(t.start + 1) as f32);
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
        // the visible frames: zoom with a pinch or Cmd/Ctrl+scroll over the timeline, pan with a
        // sideways scroll; it follows the playhead while playing
        let clamp_view = |(a, b): (f32, f32)| {
            let span = (b - a).clamp(12.0_f32.min(full1 - full0), full1 - full0);
            let a = a.clamp(full0, full1 - span);
            (a, a + span)
        };
        let (mut t0, mut t1) = self.tl_view.map(clamp_view).unwrap_or((full0, full1));
        if let Some(h) =
            ui.input(|i| i.pointer.hover_pos()).filter(|h| h.x >= area.left() && ui.max_rect().contains(*h))
        {
            let (zoom, dx) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.x));
            let at = t0 + (h.x - area.left()) / area.width() * (t1 - t0);
            if zoom != 1.0 && zoom.is_finite() && zoom > 0.0 {
                (t0, t1) = clamp_view((at - (at - t0) / zoom, at + (t1 - at) / zoom));
            }
            if dx != 0.0 && (t1 - t0) < full1 - full0 {
                let df = -dx / area.width() * (t1 - t0);
                (t0, t1) = clamp_view((t0 + df, t1 + df));
            }
        }
        let now = self.frame();
        if self.playing && (now > t1 || now < t0) {
            (t0, t1) = clamp_view((now, now + (t1 - t0)));
        }
        self.tl_view = ((t1 - t0) < full1 - full0 - 0.5).then_some((t0, t1));
        let x_of = |f: f32| area.left() + (f - t0) / (t1 - t0) * area.width();
        let f_of = |x: f32| (t0 + (x - area.left()) / area.width() * (t1 - t0)).clamp(t0, t1).round();
        let p = ui.painter_at(ruler);
        p.rect_filled(ruler, 2.0, Color32::from_gray(30));
        let fps = t.fps.max(1.0);
        let secs = (t1 - t0) / fps;
        let every = [0.25f32, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
            .into_iter()
            .find(|s| secs / s <= area.width() / 60.0)
            .unwrap_or(120.0);
        // seconds from the timeline start; ticks from the first one in view
        let mut s = (((t0 - full0) / fps) / every).ceil() * every;
        while s <= (t1 - full0) / fps + 1e-3 {
            let x = x_of(full0 + s * fps);
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
            tr("Timeline"),
            egui::FontId::proportional(11.0),
            Color32::from_gray(170),
        );
        if ruler_resp.double_clicked() {
            self.tl_view = None;
        }
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
                ui.weak(tr("No shots yet: + Shot makes one through the selected or active camera"));
            }
            for s in &shots {
                ui.menu_button(format!("{} · {}–{} · {}", s.name, s.start, s.end, s.camera), |ui| {
                    if ui.button(tr("Look through its camera")).clicked() {
                        self.run("camera.setActive", json!({"id": s.camera}));
                        self.select(Some(s.camera.clone()));
                        self.hub.borrow_mut().frame = s.start as f32;
                        self.look_through = true;
                        self.hub.borrow_mut().look_through = true;
                        ui.close();
                    }
                    if ui.button(tr("Delete shot")).clicked() {
                        self.run("shot.delete", json!({"id": s.id}));
                        ui.close();
                    }
                });
            }
        });
        // shots: one bar each; drag to move, drag an edge to trim, double-click to look through
        let (srow, sresp) = ui.allocate_exact_size(Vec2::new(full, 18.0), Sense::click_and_drag());
        {
            let p = ui.painter_at(srow);
            p.rect_filled(srow, 0.0, Color32::from_gray(26));
            p.text(
                egui::pos2(srow.left() + 6.0, srow.center().y),
                egui::Align2::LEFT_CENTER,
                tr("Shots"),
                egui::FontId::proportional(11.5),
                Color32::from_gray(170),
            );
            let colors = [
                Color32::from_rgb(70, 130, 220),
                Color32::from_rgb(90, 170, 110),
                Color32::from_rgb(210, 150, 60),
                Color32::from_rgb(170, 100, 200),
            ];
            if t.shots.is_empty() {
                p.text(
                    egui::pos2(area.left() + 6.0, srow.center().y),
                    egui::Align2::LEFT_CENTER,
                    tr(if scene.entities.iter().any(|e| matches!(e.kind, EntityKind::Camera(_))) {
                        "No shots: + Shot (above) or right-click here"
                    } else {
                        "No camera yet: + Camera in the view toolbar puts one where you look from"
                    }),
                    egui::FontId::proportional(11.0),
                    Color32::from_gray(110),
                );
            }
            for (i, sh) in t.shots.iter().enumerate() {
                let r = Rect::from_min_max(
                    egui::pos2(x_of(sh.start as f32).max(area.left()), srow.top() + 2.0),
                    egui::pos2(x_of(sh.end as f32 + 1.0).min(area.right()), srow.bottom() - 2.0),
                );
                if r.width() <= 0.0 {
                    continue; // scrolled out of view
                }
                let active = self.shot_drag.as_ref().is_some_and(|d| d.id == sh.id);
                p.rect_filled(r, 3.0, if active { colors[i % 4].gamma_multiply(1.3) } else { colors[i % 4] });
                let cam = scene.get(&sh.camera).map(|e| e.name.as_str()).unwrap_or(&sh.camera);
                p.text(
                    r.left_center() + Vec2::new(5.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{} · {cam}", sh.name),
                    egui::FontId::proportional(10.5),
                    Color32::WHITE,
                );
            }
        }
        let shot_at = |x: f32| -> Option<(String, u8)> {
            t.shots.iter().find_map(|sh| {
                let (a, b) = (x_of(sh.start as f32), x_of(sh.end as f32 + 1.0));
                if (x - a).abs() < 5.0 {
                    Some((sh.id.clone(), 1))
                } else if (x - b).abs() < 5.0 {
                    Some((sh.id.clone(), 2))
                } else if x > a && x < b {
                    Some((sh.id.clone(), 0))
                } else {
                    None
                }
            })
        };
        if let Some(h) = sresp.hover_pos() {
            match shot_at(h.x) {
                Some((_, 1 | 2)) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                Some((id, _)) => {
                    ui.ctx().set_cursor_icon(CursorIcon::Grab);
                    // what the shot shows, from the storyboard pictures
                    self.board_wanted = true;
                    if self.shot_drag.is_none()
                        && let Some(i) = t.shots.iter().position(|s| s.id == id)
                    {
                        let sh = &t.shots[i];
                        let pic = self.board.as_ref().and_then(|b| b.get(i));
                        let cam = scene.get(&sh.camera).map(|e| e.name.clone()).unwrap_or(sh.camera.clone());
                        let secs = (sh.end + 1 - sh.start) as f32 / t.fps.max(1.0);
                        sresp.clone().on_hover_ui_at_pointer(|ui| {
                            if let Some((tex, uv, aspect)) = pic {
                                let h = 90.0;
                                ui.add(
                                    egui::Image::new(egui::load::SizedTexture::new(tex, Vec2::new(h * aspect, h)))
                                        .uv(uv),
                                );
                            }
                            ui.label(crate::lf!(
                                "{} · {cam} · frames {}–{} · {secs:.1}s",
                                "{} · {cam} · 第 {}–{} 帧 · {secs:.1} 秒",
                                sh.name,
                                sh.start,
                                sh.end
                            ));
                            ui.weak(tr("Drag to move, drag an edge to trim, double-click to look through"));
                        });
                    }
                }
                None => {}
            }
        }
        if sresp.drag_started()
            && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(sresp.interact_pointer_pos())
            && let Some((id, part)) = shot_at(pos.x)
            && let Some(sh) = t.shots.iter().find(|s| s.id == id)
        {
            let key = self.new_key("shot");
            self.shot_drag = Some(ShotDrag { id, part, grab: f_of(pos.x), start: sh.start, end: sh.end, key });
        }
        // also on release, so a quick flick still lands where the pointer ended
        if (sresp.dragged() || sresp.drag_stopped())
            && let (Some(d), Some(pos)) = (self.shot_drag.clone(), ui.input(|i| i.pointer.latest_pos()))
        {
            let delta = (f_of(pos.x) - d.grab) as i64;
            let (mut a, mut b) = (d.start as i64, d.end as i64);
            match d.part {
                1 => a = (a + delta).clamp(0, b),
                2 => b = (b + delta).max(a),
                _ => {
                    let shift = delta.max(-a);
                    a += shift;
                    b += shift;
                }
            }
            self.run_key(&d.key, "shot.set", json!({"id": d.id, "start": a, "end": b}));
        }
        if sresp.drag_stopped() {
            self.shot_drag = None;
        }
        if sresp.double_clicked()
            && let Some(pos) = sresp.interact_pointer_pos()
            && let Some((id, _)) = shot_at(pos.x)
            && let Some(sh) = t.shots.iter().find(|s| s.id == id)
        {
            self.hub.borrow_mut().frame = sh.start as f32;
            self.look_through = true;
            self.hub.borrow_mut().look_through = true;
        }
        if sresp.secondary_clicked() {
            self.key_menu = sresp.interact_pointer_pos().map(|p| ("__shots__".to_string(), f_of(p.x)));
        }
        let cams: Vec<(String, String)> = scene
            .entities
            .iter()
            .filter(|e| matches!(e.kind, EntityKind::Camera(_)))
            .map(|e| (e.id.clone(), e.name.clone()))
            .collect();
        let menu_frame = self.key_menu.clone().filter(|(r, _)| r == "__shots__").map(|(_, f)| f);
        sresp.context_menu(|ui| {
            let Some(fr) = menu_frame else { return };
            let under = t.shots.iter().find(|s| fr >= s.start as f32 && fr <= s.end as f32).cloned();
            match under {
                Some(sh) => {
                    ui.weak(crate::lf!("{} · frames {}–{}", "{} · 第 {}–{} 帧", sh.name, sh.start, sh.end));
                    ui.menu_button(tr("Camera"), |ui| {
                        for (cid, cname) in &cams {
                            if ui.selectable_label(*cid == sh.camera, tr(cname)).clicked() {
                                self.run("shot.set", json!({"id": sh.id, "camera": cid}));
                                ui.close();
                            }
                        }
                    });
                    if ui.button(tr("Split at the playhead")).clicked() {
                        let f = self.frame().round() as u32;
                        if f > sh.start && f <= sh.end {
                            let _ = hub::dispatch(&self.hub, json!({"batch": [
                                {"command": "shot.set", "params": {"id": sh.id, "end": f - 1}},
                                {"command": "shot.create", "params": {"camera": sh.camera, "start": f, "end": sh.end, "name": format!("{} b", sh.name)}},
                            ]}));
                        }
                        ui.close();
                    }
                    if ui.button(tr("Delete shot")).clicked() {
                        self.run("shot.delete", json!({"id": sh.id}));
                        ui.close();
                    }
                }
                None => {
                    ui.weak(crate::lf!("frame {fr:.0}", "第 {fr:.0} 帧"));
                    for (cid, cname) in &cams {
                        if ui.button(crate::lf!("New shot here through {cname}", "在这里用 {cname} 新建镜头")).clicked() {
                            let a = fr.round().max(0.0) as u32;
                            let next = t.shots.iter().map(|s| s.start).filter(|s| *s > a).min().unwrap_or(t.end + 1);
                            self.run("shot.create", json!({"camera": cid, "start": a, "end": (a + (t.fps * 3.0) as u32).min(next.saturating_sub(1)).max(a)}));
                            ui.close();
                        }
                    }
                    if cams.is_empty() {
                        ui.weak(tr("Add a camera first"));
                    }
                }
            }
        });
        let mut playhead_bottom = srow.bottom();
        egui::ScrollArea::vertical().id_salt("tracks").max_height(row_h * 5.0 + 2.0).auto_shrink([false, true]).show(ui, |ui| {
            if rows.is_empty() {
                ui.weak(tr("No animation yet: select something and press ◆ Key, or use a preset under Animate on the right."));
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
                    let (a, b) = (x_of(w[0]).max(area.left()), x_of(w[1]).min(area.right()));
                    if a < b {
                        p.line_segment([egui::pos2(a, y), egui::pos2(b, y)], (2.0, Color32::from_gray(60)));
                    }
                }
                let near = resp.hover_pos().and_then(|h| row.frames.iter().copied().find(|f| (x_of(*f) - h.x).abs() < 6.0));
                for f in row.frames.iter().filter(|f| (t0 - 0.5..=t1 + 0.5).contains(*f)) {
                    let c = egui::pos2(x_of(*f), rect.center().y);
                    let hot = near == Some(*f) || self.key_drag.as_ref().is_some_and(|d| d.row == row.id && (d.frame - f).abs() < 0.5);
                    let r = if hot { 6.0 } else { 4.5 };
                    let pts = vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)];
                    p.add(egui::Shape::convex_polygon(pts, if hot { Color32::WHITE } else { ACCENT }, (1.0, Color32::from_gray(20))));
                }
                // the bar between the first and last key: drag it to slide the whole animation
                let span = row.frames.first().zip(row.frames.last()).filter(|(a, b)| b > a).map(|(a, b)| (*a, *b));
                let on_bar = |x: f32| span.is_some_and(|(a, b)| x > x_of(a) + 6.0 && x < x_of(b) - 6.0);
                if resp.hovered() && near.is_some() {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                } else if resp.hover_pos().is_some_and(|h| on_bar(h.x)) {
                    ui.ctx().set_cursor_icon(CursorIcon::Grab);
                }
                let ids: Vec<String> = {
                    let mut v: Vec<String> = row.tracks.iter().map(|(id, _)| id.clone()).collect();
                    v.dedup();
                    v
                };
                if resp.drag_started() {
                    let grab = ui.input(|i| i.pointer.press_origin());
                    if let Some(f) = near {
                        let key = self.new_key("retime");
                        self.key_drag = Some(KeyDrag { row: row.id.clone(), frame: f, key, all: false });
                    } else if let Some(g) = grab.filter(|g| on_bar(g.x)) {
                        let key = self.new_key("slide");
                        self.key_drag = Some(KeyDrag { row: row.id.clone(), frame: f_of(g.x), key, all: true });
                    }
                }
                if (resp.dragged() || resp.drag_stopped())
                    && let (Some(d), Some(pos)) = (self.key_drag.clone(), ui.input(|i| i.pointer.latest_pos()))
                    && d.row == row.id
                    && d.all
                {
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                    let first = row.frames.first().copied().unwrap_or(0.0);
                    let delta = (f_of(pos.x) - d.frame).max(-first);
                    if delta.abs() >= 1.0 {
                        self.run_key(&d.key, "keyframe.retime", json!({"ids": ids, "offset": delta}));
                        self.key_drag = Some(KeyDrag { frame: d.frame + delta, ..d });
                    }
                } else if (resp.dragged() || resp.drag_stopped())
                    && let (Some(d), Some(pos)) = (self.key_drag.clone(), ui.input(|i| i.pointer.latest_pos()))
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
                        // the whole row
                        let Some((first, last)) = row.frames.first().zip(row.frames.last()).map(|(a, b)| (*a, *b)) else { return };
                        ui.weak(crate::lf!("{}: frames {first:.0}–{last:.0}", "{}：第 {first:.0}–{last:.0} 帧", row.label));
                        let now = self.frame().round();
                        let go = |ui: &mut egui::Ui, label: &str, tip: &str, params: Value| {
                            if ui.button(tr(label)).on_hover_text(tr(tip)).clicked() {
                                let mut params = params;
                                params["ids"] = json!(ids);
                                let _ = hub::dispatch(&self.hub, json!({"command": "keyframe.retime", "params": params}));
                                ui.close();
                            }
                        };
                        go(ui, "Start at the playhead", "Slide every key so the animation begins here", json!({"offset": now - first}));
                        go(ui, "Faster", "Play it in two thirds of the time", json!({"scale": 2.0 / 3.0}));
                        go(ui, "Slower", "Take half as long again", json!({"scale": 1.5}));
                        ui.weak(tr("Tip: drag the bar between the keys to slide it all."));
                        return;
                    };
                    ui.weak(crate::lf!("Key at frame {f:.0}", "第 {f:.0} 帧的关键帧"));
                    ui.label(tr("Ease into the next key"));
                    for (label, interp) in [("Smooth (ease)", "ease"), ("Ease in", "easeIn"), ("Ease out", "easeOut"), ("Linear", "linear"), ("Overshoot", "overshoot"), ("Hold (step)", "step")] {
                        if ui.button(tr(label)).clicked() {
                            let steps: Vec<Value> = row.tracks.iter().filter(|(id, prop)| scene.timeline.track(id, *prop).is_some_and(|tr| tr.keys.iter().any(|k| (k.frame - f).abs() < 1e-3)))
                                .map(|(id, prop)| json!({"command": "keyframe.setInterp", "params": {"id": id, "property": prop, "frame": f, "interp": interp}})).collect();
                            let _ = hub::dispatch(&self.hub, json!({"batch": steps}));
                            ui.close();
                        }
                    }
                    let curve = row.tracks.iter().filter(|(id, prop)| {
                        *prop != TrackProperty::Expression
                            && scene.timeline.track(id, *prop).is_some_and(|tr| {
                                tr.keys.iter().position(|k| (k.frame - f).abs() < 1e-3).is_some_and(|i| i + 1 < tr.keys.len())
                            })
                    }).min_by_key(|(id, _)| *id != row.id);
                    if let Some((id, prop)) = curve
                        && ui
                            .button(tr("Custom curve…"))
                            .on_hover_text(tr("Shape this stretch's easing with two handles in the curve editor"))
                            .clicked()
                    {
                        self.select(Some(id.clone()));
                        self.show_curves = true;
                        self.curves.focus(id, *prop, f);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(tr("Delete key")).clicked() {
                        let steps: Vec<Value> = row.tracks.iter().filter(|(id, prop)| scene.timeline.track(id, *prop).is_some_and(|tr| tr.keys.iter().any(|k| (k.frame - f).abs() < 1e-3)))
                            .map(|(id, prop)| json!({"command": "keyframe.delete", "params": {"id": id, "property": prop, "frame": f}})).collect();
                        let _ = hub::dispatch(&self.hub, json!({"batch": steps}));
                        ui.close();
                    }
                });
            }
        });
        // the cuts, as faint lines down the rows
        for (f, _) in cuts(scene) {
            let cx = x_of(f);
            if cx > area.left() && cx < area.right() {
                ui.painter().line_segment(
                    [egui::pos2(cx, srow.top()), egui::pos2(cx, playhead_bottom)],
                    (1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
                );
            }
        }
        // playhead across the ruler and the rows
        let x = x_of(self.frame());
        ui.painter().line_segment(
            [egui::pos2(x, ruler.top()), egui::pos2(x, playhead_bottom)],
            (1.5, Color32::from_rgb(255, 120, 90)),
        );
        if self.show_curves {
            self.curve_panel(ui, scene, label_w, (t0, t1));
        }
    }

    /// The curve editor for the selection (or its group), under the keyframe rows.
    fn curve_panel(&mut self, ui: &mut egui::Ui, scene: &Scene, label_w: f32, view: (f32, f32)) {
        let Some(sel) = self.selected() else {
            ui.weak(tr("Curves: select an animated object or camera to see its curves."));
            return;
        };
        let root = root_of(scene, &sel);
        let Some(id) = crate::curves::subject(scene, &sel, &root) else {
            ui.weak(tr("Curves: this has no keys yet. Press ◆ Key at two frames, or use a preset under Animate."));
            return;
        };
        let now = self.frame();
        let actions = self.curves.show(ui, scene, &id, label_w, view, now, 150.0);
        for a in actions {
            match a {
                crate::curves::CurveAction::Run { merge: Some(m), command, params } => {
                    self.run_key(&m, command, params);
                }
                crate::curves::CurveAction::Run { merge: None, command, params } => {
                    self.run(command, params);
                }
                crate::curves::CurveAction::SetFrame(f) => {
                    self.playing = false;
                    self.hub.borrow_mut().frame = f;
                }
            }
        }
    }

    /// Bring the storyboard pictures up to date (a couple per frame).
    fn update_board(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let scene = self.scene();
        if let Some(rs) = frame.wgpu_render_state() {
            let board = self.board.get_or_insert_with(|| crate::storyboard::Storyboard::new(&rs.device));
            let look = crate::storyboard::Look {
                revision: self.hub.borrow().session.revision(),
                pass: self.pass,
                studio_light: self.studio_light,
                shadows: self.shadows,
            };
            // hold the pictures while something is being dragged (the scene changes every frame
            // then); they catch up when the drag ends
            let dragging = !matches!(self.drag, Gesture::Idle | Gesture::Cancelled) || self.palette.is_some();
            if !dragging && board.update(rs, &scene, &look, 2) {
                ctx.request_repaint();
            }
        }
    }

    /// One card per shot: its picture at the middle frame, number, name, length and camera.
    /// Clicking a card puts the playhead at the shot's start and looks through its camera.
    fn storyboard_strip(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let scene = self.scene();
        let shots = scene.timeline.shots.clone();
        self.update_board(ui.ctx(), frame);
        ui.add_space(4.0);
        if shots.is_empty() {
            ui.weak(tr("No shots yet: add one with + Shot, or right-click the Shots row on the timeline."));
            ui.add_space(4.0);
            return;
        }
        let now = self.frame();
        let fps = scene.timeline.fps.max(1.0);
        let card = Vec2::new(160.0, 90.0);
        ui.horizontal(|ui| {
            ui.weak(crate::lf!("{} shots", "{} 个镜头", shots.len()));
            ui.menu_button(tr("Export…"), |ui| self.sheet_menu(ui))
                .response
                .on_hover_text(tr("Save a video of the animation, or the shots for the crew: a shot sheet (PDF or picture) or one HTML page"));
        });
        egui::ScrollArea::horizontal().id_salt("storyboard").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, sh) in shots.iter().enumerate() {
                    let current = (sh.start as f32..=sh.end as f32 + 0.999).contains(&now);
                    let (rect, resp) = ui.allocate_exact_size(card + Vec2::new(0.0, 18.0), Sense::click());
                    let pic = Rect::from_min_size(rect.min, card);
                    let p = ui.painter_at(rect);
                    p.rect_filled(pic, 3.0, Color32::from_gray(16));
                    if let Some((tex, uv, aspect)) = self.board.as_ref().and_then(|b| b.get(i)) {
                        // keep the camera's aspect inside the card
                        let mut size = card;
                        if size.x / size.y > aspect {
                            size.x = size.y * aspect;
                        } else {
                            size.y = size.x / aspect;
                        }
                        p.image(tex, Rect::from_center_size(pic.center(), size), uv, Color32::WHITE);
                    }
                    let stroke = if current {
                        (2.0, ACCENT)
                    } else if resp.hovered() {
                        (1.0, Color32::from_gray(160))
                    } else {
                        (1.0, Color32::from_gray(60))
                    };
                    p.rect_stroke(pic, 3.0, stroke, egui::StrokeKind::Inside);
                    let secs = (sh.end + 1 - sh.start) as f32 / fps;
                    p.text(
                        pic.left_bottom() + Vec2::new(2.0, 2.0),
                        egui::Align2::LEFT_TOP,
                        crate::lf!("{} · {} · {secs:.1}s", "{} · {} · {secs:.1} 秒", i + 1, sh.name),
                        egui::FontId::proportional(11.0),
                        if current { Color32::WHITE } else { Color32::from_gray(190) },
                    );
                    let cam = scene.get(&sh.camera).map(|e| e.name.clone()).unwrap_or(sh.camera.clone());
                    resp.context_menu(|ui| {
                        ui.weak(format!("{} · {cam}", sh.name));
                        if ui
                            .button(crate::lf!("Select {cam}", "选中 {cam}"))
                            .on_hover_text(tr("To move it, change its lens or animate it"))
                            .clicked()
                        {
                            self.also.clear();
                            self.select(Some(sh.camera.clone()));
                            ui.close();
                        }
                        if ui.button(tr("Go to its middle frame")).on_hover_text(tr("The frame this picture shows")).clicked() {
                            self.playing = false;
                            self.look_through = true;
                            let mut h = self.hub.borrow_mut();
                            h.frame = (sh.start + sh.end) as f32 / 2.0;
                            h.look_through = true;
                            ui.close();
                        }
                        ui.separator();
                        ui.label(tr("Notes for the crew"))
                            .on_hover_text(tr("Action, dialogue, how the camera moves: printed on the shot sheet"));
                        let mut notes = sh.notes.clone();
                        let edit = egui::TextEdit::multiline(&mut notes)
                            .desired_rows(3)
                            .desired_width(240.0)
                            .hint_text(tr("e.g. She picks up the melon and smiles"));
                        if ui.add(edit).changed() {
                            self.run_key(&format!("notes:{}", sh.id), "shot.set", json!({"id": sh.id, "notes": notes}));
                        }
                        ui.separator();
                        if ui.button(tr("Delete shot")).clicked() {
                            self.run("shot.delete", json!({"id": sh.id}));
                            ui.close();
                        }
                    });
                    let resp = resp.on_hover_text(crate::lf!(
                        "{}: frames {}–{} through {cam}. Click to go there, double-click to play it; right-click for more.",
                        "{}：第 {}–{} 帧，通过 {cam}。单击跳转，双击播放，右键查看更多。",
                        sh.name, sh.start, sh.end
                    ));
                    if resp.clicked() || resp.double_clicked() {
                        self.hub.borrow_mut().frame = sh.start as f32;
                        self.playing = resp.double_clicked();
                        self.look_through = true;
                        self.hub.borrow_mut().look_through = true;
                    }
                }
            });
        });
        ui.add_space(2.0);
    }

    /// The video exports, the shot sheets and the HTML storyboard, for the Export menus.
    fn sheet_menu(&mut self, ui: &mut egui::Ui) {
        let scene = self.scene();
        if scene.timeline.shots.is_empty() && scene.active_camera.is_none() {
            ui.weak(tr("Videos and shot sheets: add a camera or a shot first"));
            return;
        }
        ui.weak(tr("Video of the whole timeline, through the shot cameras"));
        let busy = self.video.borrow().is_some();
        for (label, tip, pass, width) in [
            ("Video 720p (MP4)", "The animation as a reference video, cut between the shots", "clay", 1280),
            ("Video 1080p (MP4)", "The animation as a reference video, cut between the shots", "clay", 1920),
            (
                "Depth video (MP4)",
                "Near is white, far is black, one range for the whole video: the depth control of video models",
                "depth",
                1280,
            ),
        ] {
            if ui.add_enabled(!busy, egui::Button::new(tr(label))).on_hover_text(tr(tip)).clicked() {
                self.export_video(pass, width);
                ui.close();
            }
        }
        ui.separator();
        ui.weak(tr("Shots, for the crew"));
        for (label, tip, format, layout) in [
            (
                "Shot sheet (PDF)",
                "A table of every shot: picture, shot size and angle, focal length, camera move, duration and notes",
                "pdf",
                "table",
            ),
            ("Storyboard panels (PDF)", "Every shot's picture with its caption, three to a row", "pdf", "grid"),
            ("Shot sheet as a picture (PNG)", "The same table as one long image, to paste into a chat", "png", "table"),
        ] {
            if ui.button(tr(label)).on_hover_text(tr(tip)).clicked() {
                self.export_sheet(format, layout);
                ui.close();
            }
        }
        if !scene.timeline.shots.is_empty()
            && ui
                .button(tr("Storyboard web page (HTML)"))
                .on_hover_text(tr("Save every shot's picture as one HTML page (open it, print it or send it)"))
                .clicked()
        {
            self.export_board();
            ui.close();
        }
    }

    /// Render the timeline through the shot cameras as an MP4 (`pass`: clay or depth).
    fn export_video(&mut self, pass: &str, width: u32) {
        let t = self.scene().timeline.clone();
        let name = format!("shotstage_{pass}_{}p.mp4", width * 9 / 16);
        self.toast(tr("Rendering the video…"));
        crate::files::save_video(
            &self.hub,
            json!({ "passes": [pass], "width": width, "start": t.start, "end": t.end }),
            name,
            &self.video,
            &self.saved,
        );
    }

    /// Progress of a video export, with a button to stop it.
    fn video_progress(&mut self, ctx: &egui::Context) {
        let Some(job) = self.video.borrow().clone() else { return };
        egui::Window::new(tr("Exporting video"))
            .id(egui::Id::unique("video_export"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -96.0])
            .show(ctx, |ui| {
                let f = if job.total > 0 { job.done as f32 / job.total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(f).desired_width(260.0).text(crate::lf!(
                    "Frame {} of {}",
                    "第 {} / {} 帧",
                    job.done,
                    job.total
                )));
                if job.cancel {
                    ui.weak(tr("Stopping…"));
                } else if ui.button(tr("Cancel")).clicked()
                    && let Some(j) = self.video.borrow_mut().as_mut()
                {
                    j.cancel = true;
                }
            });
        ctx.request_repaint();
    }

    /// Render and save the shot sheet, labels in the interface language.
    fn export_sheet(&mut self, format: &str, layout: &str) {
        let lang = if crate::i18n::zh() { "zh" } else { "en" };
        self.toast(tr("Rendering the shot sheet…"));
        crate::files::save_shot_sheet(
            &self.hub,
            json!({ "format": format, "layout": layout, "lang": lang }),
            &self.saved,
        );
    }

    /// Every shot through its camera at its middle frame, saved as one HTML page.
    fn export_board(&mut self) {
        let scene = self.scene();
        let fps = scene.timeline.fps.max(1.0);
        let panels = scene
            .timeline
            .shots
            .iter()
            .enumerate()
            .map(|(i, sh)| {
                let cam = scene.get(&sh.camera).map(|e| e.name.clone()).unwrap_or(sh.camera.clone());
                crate::files::Panel {
                    heading: format!("{} · {}", i + 1, sh.name),
                    note: crate::lf!(
                        "{cam} · frames {}–{} · {:.1}s",
                        "{cam} · 第 {}–{} 帧 · {:.1} 秒",
                        sh.start,
                        sh.end,
                        (sh.end + 1 - sh.start) as f32 / fps
                    ),
                    camera: sh.camera.clone(),
                    frame: (sh.start + sh.end) as f32 / 2.0,
                }
            })
            .collect();
        crate::files::save_storyboard(&self.hub, panels, 960, &self.saved);
    }

    /// Keyframe the selection at the playhead (a whole person: every joint).
    fn key_selection(&mut self) {
        let Some(id) = self.selected() else { return };
        let scene = self.scene();
        let f = self.frame().round();
        let mut steps = vec![];
        let ids: Vec<String> = match rig::person_root(&scene, &id) {
            Ok(root) if root == id => {
                // realistic people key every joint (fingers too) and their expression
                let joints: Vec<String> = humans::human_parts(&scene, &root)
                    .first()
                    .and_then(|(part, ..)| scene.get(part).ok().and_then(rig::mesh_joints).cloned())
                    .unwrap_or_else(|| rig::JOINTS.iter().map(|j| format!("{root}_{j}")).collect());
                if scene.timeline.track(&root, TrackProperty::Expression).is_some() {
                    steps.push(json!({"command": "keyframe.set", "params": {"id": root, "property": "expression", "frame": f}}));
                }
                std::iter::once(root.clone()).chain(joints).collect()
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
            Ok(_) => self.toast(crate::lf!(
                "Keyed {} at frame {f:.0}",
                "已在第 {f:.0} 帧为 {} 打关键帧",
                scene.get(&id).map(|e| e.name.clone()).unwrap_or(id)
            )),
            Err(e) => self.toast(crate::lf!("Could not key: {}", "无法打关键帧：{}", e.message)),
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
                (rect, View { view: world.inverse(), proj: lens.projection(a), depth: self.hub.borrow().depth_range })
            }
            None => {
                let aspect = (full.width() / full.height().max(1.0)).max(0.01);
                let proj = Mat4::perspective_rh(45f32.to_radians(), aspect, 0.05, 1000.0);
                (full, View { view: self.orbit.view(), proj, depth: self.hub.borrow().depth_range })
            }
        };

        self.last_view = Some((rect, view));
        // mesh edit mode: the cage of the edited mesh as seen now
        let editing = if cam.is_some() { None } else { self.edit_state() };
        let cage = editing.as_ref().and_then(|st| {
            if !self.edit_refresh(&scene, &st.id) {
                return None;
            }
            self.edit_cage(&scene, &st.id, f, rect, &view, self.edit_xray)
        });
        let (pointer, modifiers, scroll, esc, released) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.modifiers,
                i.smooth_scroll_delta.y,
                i.key_pressed(egui::Key::Escape),
                i.pointer.any_released(),
            )
        });
        // (the camera preview in the corner sits on top: no hovering through it)
        let pip_rect = self.pip.as_ref().and_then(|p| p.rect);
        let inside = pointer.filter(|p| rect.contains(*p) && !pip_rect.is_some_and(|r| r.contains(*p)));
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
        let selected = self.selected().filter(|s| scene.contains(s) && cage.is_none());
        // Objects get move arrows, a turn ring and a scale box; a person's joint gets three
        // rotation rings.
        let gizmo = selected.as_ref().filter(|s| !clean && !scene.get(s).is_ok_and(|e| e.locked)).and_then(|id| {
            let o = world_at(&scene, id, f).ok()?.w_axis.truncate();
            let joint = rig::is_joint(&scene, id);
            let len =
                (eye - o).length().max(0.1) * if joint { 0.09 } else { 0.14 } * if self.touch { 1.3 } else { 1.0 };
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
        // a selected light's aim point: the surface it shines at, draggable
        let aim: Option<(String, Pos2)> = selected.as_ref().filter(|_| !clean).and_then(|s| {
            let EntityKind::Light(l) = &scene.get(s).ok()?.kind else { return None };
            if l.kind == shotstage_core::LightKind::Point {
                return None;
            }
            let w = world_at(&scene, s, f).ok()?;
            let (o, d) = (w.w_axis.truncate(), w.transform_vector3(Vec3::NEG_Z).normalize_or_zero());
            let opts = RayOptions { ground: true, ..Default::default() };
            let hit = raycast(&scene, f, o, d, &opts).map(|h| Vec3::from_array(h.point)).unwrap_or(o + d * 3.0);
            Some((s.clone(), project(rect, &view, hit)?))
        });
        // fingers are less exact than a mouse pointer
        let tol = if self.touch { 2.2 } else { 1.0 };
        let path_key_at = |p: Pos2| -> Option<(String, f32, Vec3)> {
            let (id, _, keys) = path.as_ref()?;
            keys.iter()
                .filter(|(_, sp, _)| (*sp - p).length() < 8.0 * tol)
                .min_by(|a, b| (a.1 - p).length().total_cmp(&(b.1 - p).length()))
                .map(|(f, _, w)| (id.clone(), *f, *w))
        };
        let handle_at = |p: Pos2| -> Option<Handle> {
            let (_, _, _, c, axes, rings, joint) = gizmo.as_ref()?;
            if !joint && (p - (*c + Vec2::new(30.0, -30.0))).length() < 8.0 * tol {
                return Some(Handle::Scale);
            }
            for (i, a) in axes.iter().enumerate() {
                if let Some(a) = a
                    && seg_dist(p, *c, *a) < 7.0 * tol
                    && (p - *c).length() > 6.0
                {
                    return Some(Handle::Axis(i));
                }
            }
            rings
                .iter()
                .map(|(ax, pts)| (*ax, pts.windows(2).map(|w| seg_dist(p, w[0], w[1])).fold(f32::MAX, f32::min)))
                .filter(|(_, d)| *d < 5.0 * tol)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(ax, _)| Handle::Ring(ax))
        };
        if matches!(self.drag, Gesture::Idle) {
            self.hot_handle = inside.and_then(handle_at);
        }

        // Edit mode: the element under the pointer, the arrow under it, and the running tool.
        if let Some(cg) = &cage {
            self.hovered = None;
            let st = self.edit_state();
            if matches!(self.drag, Gesture::Idle) && self.edit_modal.is_none() {
                self.edit_hot_axis = inside.and_then(|p| Self::edit_axis_at(cg, p));
                self.edit_hover = match (inside, &st) {
                    (Some(p), Some(st)) if self.edit_hot_axis.is_none() => {
                        self.edit_pick(cg, st.mode, p, ray_at(rect, &view, p), self.edit_xray)
                    }
                    _ => None,
                };
            } else {
                self.edit_hover = None;
                self.edit_hot_axis = None;
            }
            if self.edit_modal.is_some() {
                let ptr = pointer.unwrap_or(rect.center());
                self.edit_modal_update(cg, rect, &view, ptr, modifiers, if inside.is_some() { scroll } else { 0.0 });
                let (press, cancel) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.secondary_pressed()));
                let by_release = self.edit_modal.as_ref().is_some_and(|m| m.release_confirms);
                if cancel {
                    self.edit_cancel();
                    self.drag = Gesture::Cancelled;
                } else if press && !by_release && inside.is_some() {
                    self.edit_confirm();
                    self.edit_swallow = true;
                    self.drag = Gesture::Cancelled;
                }
                ui.ctx().request_repaint();
            }
            if self.edit_hot_axis.is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
        }

        // 3. Hover highlight (whole group unless a part of it is selected).
        if self.palette.is_none() && matches!(self.drag, Gesture::Idle) && cage.is_none() {
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

        // Two fingers on the view pan, pinch to zoom and twist to turn around (below). They end
        // what the first finger was doing; if it only just began, it is taken back.
        let multi = ui
            .input(|i| i.multi_touch())
            .filter(|m| m.num_touches >= 2)
            .filter(|_| self.two_finger || resp.hovered() || resp.dragged());
        if multi.is_some() && !self.two_finger {
            let now = ui.input(|i| i.time);
            match &self.drag {
                Gesture::Idle | Gesture::Navigate | Gesture::Cancelled | Gesture::Marquee { .. } => {}
                Gesture::EditTool => self.edit_cancel(),
                _ => {
                    if let Some((base, t0)) = self.drag_base
                        && now - t0 < 0.6
                    {
                        while self.undo_depth() > base && self.run("history.discard", json!({})).is_some() {}
                    }
                }
            }
            if self.edit_modal.is_some() {
                self.edit_cancel();
            }
            self.drag = Gesture::Cancelled;
        }
        self.two_finger = multi.is_some();
        if let Some(m) = multi
            && cam.is_none()
            && self.palette.is_none()
        {
            let inv = self.orbit.view().inverse();
            let k = self.orbit.distance * 0.0015;
            let d = m.translation_delta;
            self.orbit.target += (-inv.x_axis.xyz() * d.x + inv.y_axis.xyz() * d.y) * k;
            // (the scene turns with the fingers)
            self.orbit.yaw += m.rotation_delta;
            ui.ctx().request_repaint();
        }

        // 4. Drags in the viewport.
        if self.palette.is_none() {
            let primary = resp.drag_started_by(egui::PointerButton::Primary);
            if primary
                && !matches!(self.drag, Gesture::Cancelled)
                && let Some(start) = ui.input(|i| i.pointer.press_origin()).filter(|p| rect.contains(*p))
            {
                self.drag_base = self.touch.then(|| (self.undo_depth(), ui.input(|i| i.time)));
                let (o, d) = ray_at(rect, &view, start);
                if let Some(cg) = cage.as_ref() {
                    self.drag = self.edit_drag_start(cg, start, (o, d), modifiers, cam.is_none());
                } else {
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
                    self.drag = if let Some(id) =
                        aim.as_ref().filter(|(_, sp)| (*sp - start).length() < 9.0 * tol).map(|(id, _)| id.clone())
                    {
                        let key = self.new_key("aim");
                        Gesture::LightAim { id, key }
                    } else if let Some((id, frame, origin)) = path_key_at(start) {
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
                                Gesture::Ring {
                                    id: id.clone(),
                                    axis,
                                    last: (-(start.y - c.y)).atan2(start.x - c.x),
                                    key,
                                }
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
                        let centre =
                            spatial::bounds(&scene, &target, f).ok().flatten().map(|b| b.center()).unwrap_or(grab);
                        let offset = Vec3::new(centre.x - grab.x, 0.0, centre.z - grab.z);
                        Gesture::Move { id: target, grab, offset, key, lift: None }
                    } else if cam.is_none() && modifiers.command {
                        Gesture::Marquee { start }
                    } else if cam.is_none() {
                        Gesture::Navigate
                    } else {
                        Gesture::Idle
                    };
                }
            }
            if esc && matches!(self.drag, Gesture::Marquee { .. }) {
                self.drag = Gesture::Cancelled;
            }
            if matches!(self.drag, Gesture::EditTool) && self.edit_modal.is_none() {
                self.drag = Gesture::Cancelled;
            }
            if esc && !matches!(self.drag, Gesture::Idle | Gesture::Navigate | Gesture::Cancelled | Gesture::EditTool) {
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
                        // scrolling turns it 15° per ~40 points of scroll (smoothed wheel notches
                        // and trackpad swipes arrive spread over many frames)
                        self.turn_acc += scroll;
                        let steps = (self.turn_acc / 40.0).trunc();
                        if steps != 0.0 {
                            self.turn_acc -= steps * 40.0;
                            self.run_key(&key, "entity.rotate", json!({"id": id, "degrees": steps * 15.0, "frame": f}));
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
                Gesture::LightAim { id, key } if resp.dragged() => {
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                    let opts = RayOptions { ground: true, eye: Some(o), ..Default::default() };
                    if let Some(h) = raycast(&scene, f, o, d, &opts) {
                        self.run_key(&key, "camera.lookAt", json!({"id": id, "target": h.point}));
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
            // the selection rectangle: drawn while dragging, applied on release
            self.marquee = None;
            if let Gesture::Marquee { start } = self.drag
                && let Some(now) = ui.input(|i| i.pointer.latest_pos())
            {
                let r = Rect::from_two_pos(start, now).intersect(rect);
                if resp.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
                    match &cage {
                        Some(cg) => self.edit_select_rect(cg, r, modifiers.shift),
                        None => self.select_in_rect(r, rect, &view, f),
                    }
                } else {
                    self.marquee = Some(r);
                }
            }
            if resp.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
                if matches!(self.drag, Gesture::EditTool) {
                    self.edit_confirm();
                }
                // a drag never ends in a click: nothing left to swallow
                if resp.drag_stopped() {
                    self.edit_swallow = false;
                }
                self.drag = Gesture::Idle;
                self.turn_acc = 0.0;
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
            // two fingers on a trackpad orbit (Shift: pan) and pinching zooms; a mouse wheel zooms
            let (trackpad, delta) = ui.input(|i| (wheel_from_trackpad(&i.events), i.smooth_scroll_delta));
            if let Some(t) = trackpad {
                self.wheel_trackpad = t;
            }
            let orbit_scroll = match self.scroll_mode {
                ScrollMode::Auto => self.wheel_trackpad,
                ScrollMode::Zoom => false,
                ScrollMode::Orbit => true,
            };
            let tool_scroll = self.edit_modal.as_ref().is_some_and(|m| {
                matches!(m.tool, editmode::Tool::Bevel | editmode::Tool::LoopCut)
                    || (self.edit_prop
                        && matches!(m.tool, editmode::Tool::Grab | editmode::Tool::Rotate | editmode::Tool::Scale))
            });
            if resp.hovered() && !matches!(self.drag, Gesture::Move { .. }) && delta != Vec2::ZERO && !tool_scroll {
                if !orbit_scroll {
                    self.orbit.distance = (self.orbit.distance * (1.0 - delta.y * 0.0015)).clamp(0.3, 500.0);
                } else if modifiers.shift {
                    let inv = self.orbit.view().inverse();
                    let k = self.orbit.distance * 0.0015;
                    self.orbit.target += (-inv.x_axis.xyz() * delta.x + inv.y_axis.xyz() * delta.y) * k;
                } else {
                    self.orbit.yaw -= delta.x * 0.006;
                    self.orbit.pitch = (self.orbit.pitch + delta.y * 0.006).clamp(-1.5, 1.5);
                }
            }
            // trackpad pinch (and Ctrl+wheel) arrives as a zoom factor, not as scrolling
            let pinch = ui.input(|i| i.zoom_delta());
            if (resp.hovered() || self.two_finger) && pinch != 1.0 && pinch.is_finite() && pinch > 0.0 {
                self.orbit.distance = (self.orbit.distance / pinch).clamp(0.3, 500.0);
            }
            if resp.double_clicked()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let (o, d) = ray_at(rect, &view, pos);
                if let Some(cg) = &cage {
                    if self.edit_modal.is_none() {
                        self.edit_select_linked_at(cg, pos, (o, d), modifiers.shift);
                    }
                } else if let Some(hit) = pick(&scene, f, o, d)
                    && let Ok(Some(b)) = spatial::bounds(&scene, &root_of(&scene, &hit), f)
                {
                    self.orbit.target = b.center();
                    self.orbit.distance = (b.size().max_element() * 2.2).clamp(0.4, 200.0);
                }
            }
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos().filter(|p| rect.contains(*p))
            && let Some(cg) = &cage
        {
            if std::mem::take(&mut self.edit_swallow) || self.edit_modal.is_some() {
                // the click that applied a tool
            } else {
                // modifiers as they were when the button went up (a quick Alt+click may let go
                // of Alt before this frame)
                let m = ui
                    .input(|i| {
                        i.events.iter().rev().find_map(|e| match e {
                            egui::Event::PointerButton { pressed: false, modifiers, .. } => Some(*modifiers),
                            _ => None,
                        })
                    })
                    .unwrap_or(modifiers);
                self.edit_click(cg, pos, ray_at(rect, &view, pos), m);
            }
        } else if resp.clicked()
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
                            (dist < 14.0 * tol).then(|| (dist, e.id.clone()))
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
            if let Some(s) = sel.clone().filter(|_| modifiers.shift && self.selected().is_some()) {
                // Shift+click: add to (or take out of) the selection
                if self.selected().as_ref() != Some(&s) {
                    match self.also.iter().position(|a| *a == s) {
                        Some(i) => {
                            self.also.remove(i);
                        }
                        None => self.also.push(s),
                    }
                }
            } else {
                self.also.clear();
                self.select(sel);
            }
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
            selected: if clean || cage.is_some() { None } else { self.selected() },
            grid: !clean,
            cameras: !clean,
            looking_through: cam.clone(),
            hovered: if clean { None } else { self.hovered.clone() },
            footprint: dragged_id,
            studio_light: self.studio_light,
            eye: None,
            also_selected: if clean { vec![] } else { self.also.clone() },
            no_shadows: !self.shadows,
        };
        let pass = self.pass;
        let (Some(rs), Some(renderer)) = (frame.wgpu_render_state(), self.renderer.as_mut()) else {
            ui.painter().text(
                full.center(),
                egui::Align2::CENTER_CENTER,
                tr("GPU not available"),
                egui::FontId::proportional(16.0),
                Color32::GRAY,
            );
            return;
        };
        if px.0 < 2 || px.1 < 2 {
            return;
        }
        renderer.render(&rs.device, &rs.queue, &scene, f, view, pass, &overlay, px);
        self.depth_seen = renderer.last_depth;
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
        // the shot camera's view in the corner (the programme), and beside it the next shot
        // (what the cut will switch to), each re-rendered only when what it shows changes
        let pip_cam = (!clean && self.cam_preview && self.palette.is_none()).then(|| camera_id_at(&scene, f)).flatten();
        let mut pip_show = None;
        let mut next_show = None;
        for p in [self.pip.as_mut(), self.pip_next.as_mut()].into_iter().flatten() {
            p.rect = None;
        }
        let revision = self.hub.borrow().session.revision();
        let now = ui.input(|i| i.time);
        let look = (pass, self.studio_light, self.shadows, revision, now);
        let next_cut = pip_cam.as_ref().and_then(|_| next_cut(&scene, f));
        if let Some(c) = pip_cam
            && let Ok(lens) = camera_data_at(&scene, &c, f)
        {
            let a = lens.aspect.clamp(0.3, 4.0);
            let w = (full.width() * 0.24).clamp(if self.compact { 110.0 } else { 150.0 }, 300.0);
            let size = Vec2::new(w, w / a);
            let prect = Rect::from_min_size(full.right_bottom() - size - Vec2::splat(12.0), size);
            let pip = self.pip.get_or_insert_with(|| Pip::new(&rs.device));
            if let Some(tex) = pip.update(rs, &scene, &c, f, (size * ppp).round(), look) {
                pip.rect = Some(prect);
                let cam = scene.get(&c).map(|e| e.name.clone()).unwrap_or(c.clone());
                let name = match scene.timeline.shot_at(f) {
                    Some(sh) => format!("{} · {cam}", sh.name),
                    None => cam,
                };
                pip_show = Some((tex, prect, name));
            } else if pip.key.is_some() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            }
            // the next shot, smaller, to the left
            if let Some((nf, shot)) = next_cut.clone()
                && let Ok(nl) = camera_data_at(&scene, &shot.camera, nf)
            {
                let na = nl.aspect.clamp(0.3, 4.0);
                let nw = w * 0.62;
                let nsize = Vec2::new(nw, nw / na);
                let nrect =
                    Rect::from_min_size(egui::pos2(prect.left() - nsize.x - 10.0, prect.bottom() - nsize.y), nsize);
                if nrect.left() > full.left() + 8.0 {
                    let np = self.pip_next.get_or_insert_with(|| Pip::new(&rs.device));
                    if let Some(tex) = np.update(rs, &scene, &shot.camera, nf, (nsize * ppp).round(), look) {
                        np.rect = Some(nrect);
                        let secs = (nf - f) / scene.timeline.fps.max(1.0);
                        let label = crate::lf!("Next in {secs:.1}s · {}", "{secs:.1} 秒后切到 · {}", shot.name);
                        next_show = Some((tex, nrect, label, nf));
                    }
                }
            }
        }
        if let Some((tex, prect, name)) = pip_show {
            let r = monitor(ui.ctx(), "cam_preview", tex, prect, &name, ACCENT)
                .on_hover_text(tr("Shot camera at the playhead: click to look through it"));
            if r.clicked() {
                self.look_through = true;
                self.hub.borrow_mut().look_through = true;
            }
        }
        if let Some((tex, nrect, label, nf)) = next_show {
            let r = monitor(ui.ctx(), "next_preview", tex, nrect, &label, Color32::from_rgb(90, 200, 120))
                .on_hover_text(tr(
                    "The shot after the next cut: click to play through the cut, double-click to jump to it",
                ));
            if r.double_clicked() {
                self.playing = false;
                self.hub.borrow_mut().frame = nf;
            } else if r.clicked() {
                self.preview_cut(nf);
            }
        }
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, Color32::from_gray(10));
        if let Some((id, _)) = self.texture {
            painter.image(id, rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        // (a tool may have changed the mesh since the cage was projected: project it again)
        let cage = cage.and_then(|_| {
            let st = self.edit_state()?;
            let scene = self.scene();
            self.edit_refresh(&scene, &st.id).then_some(())?;
            self.edit_cage(&scene, &st.id, f, rect, &view, self.edit_xray)
        });
        if let Some(cg) = &cage {
            self.edit_draw(&painter, cg, rect, &view, self.edit_hover);
        }
        if let Some(m) = self.marquee {
            painter.rect(m, 0.0, ACCENT.gamma_multiply(0.12), (1.0, ACCENT), egui::StrokeKind::Inside);
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
                .map(|d| {
                    if crate::i18n::zh() {
                        format!(" · {}，{:.0}mm {}", tr(&d.angle), d.focal_length_mm, tr(format!("{} lens", d.lens)))
                    } else {
                        format!(" · {}", d.summary)
                    }
                })
                .unwrap_or_default();
            painter.text(
                rect.left_top() + Vec2::new(8.0, 6.0),
                egui::Align2::LEFT_TOP,
                crate::lf!(
                    "{} · frame {f:.0}{hud}",
                    "{} · 第 {f:.0} 帧{hud}",
                    scene.get(c).map(|e| e.name.as_str()).unwrap_or(c)
                ),
                egui::FontId::monospace(12.0),
                Color32::from_white_alpha(180),
            );
        }
        if let Some((id, sp)) = &aim
            && let Some(lp) = world_at(&scene, id, f).ok().and_then(|w| project(rect, &view, w.w_axis.truncate()))
        {
            let lit = matches!(&self.drag, Gesture::LightAim { .. })
                || ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| (p - *sp).length() < 9.0);
            let col = Color32::from_rgb(255, 210, 90);
            painter.add(egui::Shape::dashed_line(&[lp, *sp], (1.0, col.gamma_multiply(0.7)), 6.0, 4.0));
            painter.circle_stroke(*sp, if lit { 9.0 } else { 7.0 }, (2.0, col));
            painter.circle_filled(*sp, 2.5, col);
            if lit {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
                painter.text(
                    *sp + Vec2::new(12.0, -12.0),
                    egui::Align2::LEFT_BOTTOM,
                    tr("drag to aim"),
                    egui::FontId::proportional(11.0),
                    col,
                );
            }
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
        // (an open drawer covers the view: its floating bits would sit on top of it)
        let covered = self.drawer.is_some();
        if !covered {
            self.toolbar(ui, full, cam.is_some() || camera_id_at(&scene, f).is_some());
        }
        if cage.is_some() && !covered {
            self.edit_bar(ui, full);
        }
        if !clean && !covered {
            self.view_cube(ui, full);
            // name of what the pointer would grab
            if let (Some(h), Some(p)) = (&self.hovered, inside)
                && matches!(self.drag, Gesture::Idle)
                && self.palette.is_none()
                && let Ok(e) = scene.get(h)
            {
                let font = egui::FontId::proportional(11.5);
                let g = painter.layout_no_wrap(e.name.clone(), font, Color32::WHITE);
                let r = Rect::from_min_size(p + Vec2::new(14.0, 14.0), g.size() + Vec2::new(10.0, 6.0));
                painter.rect_filled(r, 4.0, Color32::from_rgba_unmultiplied(20, 22, 28, 200));
                painter.galley(r.min + Vec2::new(5.0, 3.0), g, Color32::WHITE);
            }
        }
        // nothing to offer when the scene already is the autosave (a page that builds its scene)
        if self.restore_offer.as_ref().is_some_and(|(saved, _)| **saved == *scene) {
            self.restore_offer = None;
        }
        if let Some(n) = self.restore_offer.as_ref().map(|o| o.1) {
            egui::Area::new(egui::Id::unique("restore_offer"))
                .order(egui::Order::Foreground)
                .fixed_pos(full.center_top() + Vec2::new(-170.0, 104.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.strong(tr("Pick up where you left off?"));
                        ui.weak(crate::lf!(
                            "Your last scene ({n} objects) was saved automatically.",
                            "上次的场景（{n} 个物体）已自动保存。"
                        ));
                        ui.horizontal(|ui| {
                            if ui.button(tr("Restore it")).clicked()
                                && let Some((saved, _)) = self.restore_offer.take()
                                && let Ok(scene) = serde_json::to_value(&*saved)
                            {
                                let _ = hub::dispatch(
                                    &self.hub,
                                    json!({"command": "scene.load", "params": {"scene": scene}}),
                                );
                                self.toast(tr("Restored your last scene (Undo goes back)"));
                            }
                            if ui.button(tr("Start fresh")).clicked() {
                                self.restore_offer = None;
                            }
                        });
                    });
                });
        } else if scene.entities.is_empty() && self.palette.is_none() && !covered {
            self.welcome(ui, full);
        }
        // files dragged over the window
        if ui.input(|i| !i.raw.hovered_files.is_empty()) {
            painter.rect_filled(full.shrink(6.0), 8.0, Color32::from_rgba_unmultiplied(40, 98, 170, 60));
            painter.text(
                full.center(),
                egui::Align2::CENTER_CENTER,
                tr("Drop to import (glb, gltf, obj, stl, fbx, blend)"),
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

    /// Axis gizmo at the top-right: shows which way the view faces; click an axis (or a button)
    /// to look from that side.
    fn view_cube(&mut self, ui: &egui::Ui, full: Rect) {
        let size = Vec2::new(92.0, 124.0);
        let view = self.orbit.view();
        let (yaw, pitch) = (self.orbit.yaw, self.orbit.pitch);
        let mut pick: Option<(f32, f32)> = None;
        egui::Area::new(egui::Id::unique("view_cube"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(full.right() - size.x - 6.0, full.top() + 8.0))
            .show(ui.ctx(), |ui| {
                let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                let painter = ui.painter_at(rect);
                let c = egui::pos2(rect.center().x, rect.top() + 46.0);
                painter.circle_filled(c, 42.0, Color32::from_rgba_unmultiplied(20, 22, 28, 150));
                let axes = [
                    (Vec3::X, "X", Color32::from_rgb(235, 80, 80), (std::f32::consts::FRAC_PI_2, 0.0)),
                    (Vec3::Y, "Y", Color32::from_rgb(110, 210, 90), (yaw, 1.45)),
                    (Vec3::Z, "Z", Color32::from_rgb(80, 140, 255), (0.0, 0.0)),
                    (Vec3::NEG_X, "", Color32::from_rgb(160, 70, 70), (-std::f32::consts::FRAC_PI_2, 0.0)),
                    (Vec3::NEG_Z, "", Color32::from_rgb(70, 100, 170), (std::f32::consts::PI, 0.0)),
                ];
                // far axes first so the near ones draw on top
                let mut items: Vec<CubeAxis> = axes
                    .iter()
                    .map(|(a, l, col, target)| {
                        let v = view.transform_vector3(*a);
                        (v.z, c + Vec2::new(v.x, -v.y) * 30.0, *l, *col, *target)
                    })
                    .collect();
                items.sort_by(|a, b| a.0.total_cmp(&b.0));
                let pointer = resp.hover_pos();
                for (_, p, label, col, target) in &items {
                    painter.line_segment([c, *p], (2.0, *col));
                    let hot = pointer.is_some_and(|q| (q - *p).length() < 9.0);
                    painter.circle_filled(*p, if hot { 9.0 } else { 7.0 }, if hot { Color32::WHITE } else { *col });
                    if !label.is_empty() {
                        painter.text(
                            *p,
                            egui::Align2::CENTER_CENTER,
                            *label,
                            egui::FontId::proportional(10.0),
                            Color32::from_gray(20),
                        );
                    }
                    if hot && resp.clicked() {
                        pick = Some(*target);
                    }
                }
                // a reset to a pleasant three-quarter view under the gizmo
                let r = Rect::from_center_size(egui::pos2(c.x, rect.bottom() - 12.0), Vec2::new(46.0, 18.0));
                let hot = pointer.is_some_and(|q| r.contains(q));
                painter.rect_filled(
                    r,
                    4.0,
                    if hot { Color32::from_rgb(60, 66, 80) } else { Color32::from_rgba_unmultiplied(20, 22, 28, 170) },
                );
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    "3/4",
                    egui::FontId::proportional(11.0),
                    Color32::WHITE,
                );
                if hot && resp.clicked() {
                    pick = Some((0.6, 0.35));
                }
                let _ = pitch;
                resp.on_hover_text(tr("Click an axis to look along it (Y: from above) · 3/4: the default view"));
            });
        if let Some((y, p)) = pick {
            (self.orbit.yaw, self.orbit.pitch) = (y, p);
        }
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
                    ui.set_max_width(372.0);
                    ui.heading(tr("Start a scene"));
                    ui.weak(tr("Pick a ready set (everything stays editable), or drag blocks in from the left."));
                    ui.add_space(6.0);
                    use shotstage_commands::templates::{PRODUCT_LOOKS, TEMPLATES};
                    for (heading, products) in [("Scenes", false), ("Product close-ups", true)] {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(tr(heading)).strong());
                        egui::Grid::new(("welcome_grid", products)).num_columns(3).spacing([6.0, 6.0]).show(ui, |ui| {
                            let list = TEMPLATES.iter().filter(|t| PRODUCT_LOOKS.contains(&t.0) == products);
                            for (i, (name, label, desc)) in list.enumerate() {
                                if ui.add_sized([116.0, 30.0], egui::Button::new(tr(*label))).on_hover_text(tr(*desc)).clicked() {
                                    self.apply_template(name, label);
                                }
                                if i % 3 == 2 {
                                    ui.end_row();
                                }
                            }
                        });
                    }
                    ui.add_space(4.0);
                    ui.weak(tr("Tip: drop a .glb / .fbx model or a picture on the view to bring it in. Cmd/Ctrl+K searches every block, move and action."));
                });
            });
    }

    /// The depth pass range: fitted to the subjects in frame, or fixed near / far distances.
    fn depth_menu(&mut self, ui: &mut egui::Ui) {
        let cur = self.hub.borrow().depth_range;
        let label = if cur.is_some() { tr("Range: manual") } else { tr("Range: auto") };
        ui.menu_button(format!("{label} ▾"), |ui| {
            let mut manual = cur.is_some();
            ui.radio_value(&mut manual, false, tr("Auto (subjects stand out)")).on_hover_text(tr(
                "Fit the greys to what is in frame; the subjects (not the floor, walls or a table top running out of frame) get most of them",
            ));
            ui.radio_value(&mut manual, true, tr("Manual near / far"));
            let mut next = match (manual, cur) {
                (false, _) => None,
                (true, Some(d)) => Some(d),
                // start from what the auto fit shows now, without its focus
                (true, None) => Some(shotstage_render::DepthMap::linear(self.depth_seen.near, self.depth_seen.far)),
            };
            if let Some(d) = next.as_mut() {
                let (mut n, mut f) = (d.near, d.far);
                egui::Grid::new("depth_range").num_columns(2).show(ui, |ui| {
                    ui.label(tr("Near (white)"));
                    ui.add(egui::DragValue::new(&mut n).range(0.0..=f - 0.01).speed(0.01).max_decimals(2).suffix(" m"));
                    ui.end_row();
                    ui.label(tr("Far (black)"));
                    ui.add(egui::DragValue::new(&mut f).range(n + 0.01..=10_000.0).speed(0.02).max_decimals(2).suffix(" m"));
                    ui.end_row();
                });
                if (n, f) != (d.near, d.far) {
                    *d = shotstage_render::DepthMap::linear(n, f);
                }
            } else {
                let d = self.depth_seen;
                let text = match d.focus {
                    Some([a, b]) => format!("{:.2}–{:.2} m ({} {:.2}–{:.2} m)", d.near, d.far, tr("subjects"), a, b),
                    None => format!("{:.2}–{:.2} m", d.near, d.far),
                };
                ui.weak(text);
            }
            if next != cur {
                self.hub.borrow_mut().depth_range = next;
            }
        });
    }

    /// Floating view options at the top-left of the viewport (folded into one menu on a small
    /// screen, next to the camera view switch).
    fn toolbar(&mut self, ui: &egui::Ui, full: Rect, has_cam: bool) {
        let compact = self.compact;
        egui::Area::new(egui::Id::unique("viewport_toolbar"))
            .order(egui::Order::Foreground)
            .fixed_pos(full.left_top() + Vec2::new(8.0, 8.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                    // wrap rather than spill over the side panels or under the view cube
                    ui.set_max_width((full.width() - 28.0 - 100.0).max(160.0));
                    ui.horizontal_wrapped(|ui| {
                        if compact {
                            ui.menu_button(tr("View options ▾"), |ui| self.view_options(ui, has_cam, true));
                            self.camera_view_button(ui, has_cam);
                        } else {
                            self.view_options(ui, has_cam, false);
                        }
                    });
                });
            });
    }

    /// Look through the shot camera (the clean reference frame).
    fn camera_view_button(&mut self, ui: &mut egui::Ui, has_cam: bool) {
        // (a plain button: an enabled_ui wrapper would wrap its text letter by letter)
        if ui
            .add_enabled(has_cam, egui::Button::selectable(self.look_through, tr("🎥 Camera view")))
            .on_hover_text(tr("Look through the shot camera (the clean reference frame)"))
            .clicked()
        {
            self.look_through = !self.look_through;
        }
        if !has_cam {
            self.look_through = false;
        }
    }

    fn view_options(&mut self, ui: &mut egui::Ui, has_cam: bool, compact: bool) {
        ui.selectable_value(&mut self.pass, Pass::Clay, tr("Clay")).on_hover_text(tr("Clay shading"));
        ui.selectable_value(&mut self.pass, Pass::Depth, tr("Depth")).on_hover_text(tr("Depth pass (near = white)"));
        ui.selectable_value(&mut self.pass, Pass::Normal, tr("Normal")).on_hover_text(tr("Normal pass"));
        if self.pass == Pass::Depth {
            self.depth_menu(ui);
        }
        ui.add_space(6.0);
        ui.toggle_value(&mut self.snap, tr("Snap"))
            .on_hover_text(tr("Snap dragged blocks to a grid (hold Ctrl to place freely)"));
        ui.add_enabled(self.snap, egui::DragValue::new(&mut self.snap_step).range(0.01..=5.0).speed(0.01).suffix(" m"));
        ui.add_space(6.0);
        if !compact {
            self.camera_view_button(ui, has_cam);
        }
        if ui
            .add_enabled(has_cam, egui::Button::new(tr("📷 Save frame")))
            .on_hover_text(tr("Save the shot camera's view at this frame as a PNG (1920 px wide)"))
            .clicked()
        {
            crate::files::save_frame(&self.hub, 1920, &self.saved);
        }
        if !self.look_through
            && ui.button(tr("+ Camera")).on_hover_text(tr("A new camera right where you are looking from")).clicked()
        {
            self.camera_from_view();
        }
        ui.menu_button(tr("Sky ▾"), |ui| {
            ui.set_min_width(330.0);
            self.world_panel(ui);
        })
        .response
        .on_hover_text(tr("Sky (built-in skies or an HDRI), sky light and shadows"));
        ui.menu_button(tr("View ▾"), |ui| {
            ui.checkbox(&mut self.shadows, tr("Shadows"))
                .on_hover_text(tr("Show shadows in the viewport (snapshots follow the scene's shadow settings)"));
            ui.checkbox(&mut self.studio_light, tr("Studio light")).on_hover_text(tr(
                "Show the flat studio clay light instead of the scene's lights (scenes without lights always use it)",
            ));
            ui.checkbox(&mut self.safe_areas, tr("Safe areas in camera view"))
                .on_hover_text(tr("Action / title safe frames and a centre cross when looking through a camera"));
            ui.checkbox(&mut self.cam_preview, tr("Camera preview in the corner"))
                .on_hover_text(tr("A small live view of the shot camera while you work in the free view"));
            ui.separator();
            ui.label(tr("Two-finger scroll / mouse wheel"));
            ui.horizontal(|ui| {
                for (m, label, tip) in [
                    (ScrollMode::Auto, "Auto", "Trackpad orbits, mouse wheel zooms"),
                    (ScrollMode::Orbit, "Orbit", "Scrolling orbits (Shift: pan); pinch or Ctrl+scroll zooms"),
                    (ScrollMode::Zoom, "Zoom", "Scrolling always zooms"),
                ] {
                    ui.selectable_value(&mut self.scroll_mode, m, tr(label)).on_hover_text(tr(tip));
                }
            });
        })
        .response
        .on_hover_text(tr("Shadows, lighting, camera preview and navigation"));
        if self.touch {
            // no hovering with fingers: tap for the help
            ui.menu_button(egui::RichText::new(tr(" ? ")).strong(), |ui| {
                ui.set_max_width(340.0);
                ui.label(crate::i18n::touch_help());
                ui.separator();
                ui.weak(crate::i18n::help(HELP));
            });
        } else {
            ui.label(egui::RichText::new(tr(" ? ")).strong()).on_hover_text(crate::i18n::help(HELP));
        }
    }
}

#[cfg(test)]
mod quick_tests {
    use super::*;

    fn item(label: &str, kind: &'static str, hint: &str) -> QuickItem {
        QuickItem { label: label.into(), kind, hint: hint.into(), run: QuickRun::Action(QuickAction::Undo) }
    }

    #[test]
    fn humanizes_names() {
        assert_eq!(humanize("holdBasket"), "Hold basket");
        assert_eq!(humanize("tPose"), "T pose");
        assert_eq!(humanize("stand"), "Stand");
    }

    #[test]
    fn ranks_prefix_then_word_then_hint() {
        let items = [
            item("Orbit left", "Camera move", "circle the subject"),
            item("Push in", "Camera move", "move toward the subject"),
            item("Slow push", "Camera move", ""),
            item("Apples", "Add", "a crate of apples"),
        ];
        assert_eq!(rank_quick(&items, "push"), vec![1, 2]);
        assert_eq!(rank_quick(&items, "subject"), vec![0, 1]);
        assert_eq!(rank_quick(&items, "crate apples"), vec![3]);
        assert_eq!(rank_quick(&items, "zzz"), Vec::<usize>::new());
        assert_eq!(rank_quick(&items, "").len(), 4);
    }
}

#[cfg(test)]
mod wheel_tests {
    use super::*;

    fn wheel(unit: egui::MouseWheelUnit, delta: Vec2, source: egui::MouseWheelSource) -> egui::Event {
        egui::Event::MouseWheel { unit, delta, phase: egui::TouchPhase::Move, source, modifiers: Default::default() }
    }

    #[test]
    fn tells_trackpad_from_wheel() {
        use egui::{MouseWheelSource as S, MouseWheelUnit as U};
        assert_eq!(wheel_from_trackpad(&[]), None);
        assert_eq!(wheel_from_trackpad(&[wheel(U::Point, Vec2::new(0.0, 3.0), S::Trackpad)]), Some(true));
        assert_eq!(wheel_from_trackpad(&[wheel(U::Line, Vec2::new(0.0, 1.0), S::Wheel)]), Some(false));
        // the web does not say: guess from the movement
        assert_eq!(wheel_from_trackpad(&[wheel(U::Point, Vec2::new(1.5, 0.0), S::Unknown)]), Some(true));
        assert_eq!(wheel_from_trackpad(&[wheel(U::Point, Vec2::new(0.0, 2.25), S::Unknown)]), Some(true));
        assert_eq!(wheel_from_trackpad(&[wheel(U::Point, Vec2::new(0.0, -100.0), S::Unknown)]), Some(false));
        assert_eq!(wheel_from_trackpad(&[wheel(U::Point, Vec2::new(0.0, 4.0), S::Unknown)]), None);
        assert_eq!(wheel_from_trackpad(&[wheel(U::Line, Vec2::new(0.0, 1.0), S::Unknown)]), Some(false));
    }
}

#[cfg(test)]
mod i18n_tests {
    use super::*;

    /// Wording that needs no translation: shortcuts, abbreviations, identifiers, names.
    fn needs_words(s: &str) -> bool {
        let t = s.trim();
        let word = !t.contains(' ');
        t.chars().filter(|c| c.is_ascii_alphabetic()).count() >= 2
            && !t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            && !(word && (t.contains('+') || t.contains('.') || t.contains('_')))
            && !(word && t.chars().all(|c| !c.is_ascii_lowercase()))
            && !(word && t.starts_with(|c: char| c.is_ascii_lowercase()) && t.chars().any(|c| c.is_ascii_uppercase()))
            && !["Tab", "Home", "shotstage", "drag", "Cmd/Ctrl+Shift+Z"].contains(&t)
    }

    /// Every text the interface shows in Chinese: the data tables it lists and every
    /// `tr("…")` in the sources.
    #[test]
    fn interface_is_translated() {
        let mut texts: Vec<String> = Vec::new();
        for (title, _, blocks) in palette() {
            texts.push(title.into());
            for b in blocks {
                texts.push(b.label().into());
                texts.push(b.description().into());
            }
        }
        for p in prefabs::PREFABS {
            texts.extend([p.label.to_string(), p.description.to_string()]);
        }
        for p in rig::POSES {
            texts.extend([humanize(p.name), p.description.to_string()]);
        }
        for h in humans::HAND_SHAPES {
            texts.extend([humanize(h.name), h.description.to_string()]);
        }
        for e in shotstage_core::human::EXPRESSIONS {
            texts.extend([humanize(e.name), e.description.to_string()]);
        }
        for s in rig::HAIR_STYLES {
            texts.push(humanize(s));
        }
        for p in shotstage_commands::motion::PRESETS {
            texts.extend([p.label.to_string(), p.description.to_string()]);
        }
        for c in shotstage_core::Channel::TRANSFORM.into_iter().chain([shotstage_core::Channel::Focal]) {
            texts.push(crate::curves::channel_name(c).to_string());
        }
        for (label, _) in crate::curves::PRESETS {
            texts.push(label.to_string());
        }
        for (_, key, desc) in shotstage_core::environment::SKIES {
            let mut c = key.chars();
            texts.push(c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default());
            texts.push(desc.to_string());
        }
        for (_, label, desc) in shotstage_commands::templates::TEMPLATES {
            texts.extend([label.to_string(), desc.to_string()]);
        }
        for src in [
            include_str!("ui.rs"),
            include_str!("editmode.rs"),
            include_str!("material.rs"),
            include_str!("uveditor.rs"),
            include_str!("storyboard.rs"),
            include_str!("curves.rs"),
        ] {
            for part in src.split("tr(\"").skip(1) {
                let Some(end) = part.find("\")") else { continue };
                let lit = &part[..end];
                if !lit.contains('"') {
                    texts.push(lit.replace("\\n", "\n").replace("\\\"", "\""));
                }
            }
        }
        let mut missing: Vec<String> = texts.into_iter().filter(|t| needs_words(t) && !crate::i18n::has(t)).collect();
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "{} texts without Chinese:\n{}", missing.len(), missing.join("\n"));
    }
}
