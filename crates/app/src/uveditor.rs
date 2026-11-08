//! The UV editor, Blender style: a panel beside the viewport that shows a mesh's UV map over
//! its picture. Pick UV vertices, edges, faces or islands (1 / 2 / 3 / 4), drag them, or grab
//! (G), rotate (R) and scale (S) them with the pointer (X / Y lock an axis, Ctrl snaps), box
//! select by dragging empty space, L / double-click picks a whole island.
//!
//! Every change is an ordinary command (`mesh.uvSet` while dragging, merged into one undo
//! step; `mesh.uvAlign`, `mesh.uvWeld`, `mesh.uvPack`, `mesh.unwrap`...), and the panel and its
//! selection are editor view state (`editor.uvEditor`, `editor.uvSelect`), so an agent sees
//! and can drive the same thing.

use std::collections::{BTreeSet, HashMap};

use shotstage_core::Mesh;
use shotstage_core::uv::{Mapping, Material};
use shotstage_core::uvedit::{self as ue, Corner, UvTopology};

use super::*;
use crate::hub::{UvSelectMode, UvState};

const SEL: Color32 = Color32::from_rgb(255, 152, 40);
const HOVER: Color32 = Color32::from_rgb(255, 236, 170);
/// Above this many faces, edges are drawn without their dark outline.
const OUTLINE_LIMIT: usize = 12_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UvTool {
    Grab,
    Rotate,
    Scale,
}

/// A grab / rotate / scale following the pointer.
pub(super) struct UvModal {
    tool: UvTool,
    /// The mesh before the tool.
    base: Mesh,
    /// The corners that move (sticky already applied).
    corners: Vec<Corner>,
    start: Pos2,
    pivot: Vec2,
    /// 0 = u only, 1 = v only.
    axis: Option<usize>,
    key: String,
    changed: bool,
    /// Started by dragging: letting go applies.
    release_confirms: bool,
    /// What the last try sent, to skip repeats.
    last: String,
    /// Shown in the canvas.
    info: String,
}

#[derive(Default)]
pub(super) enum UvGesture {
    #[default]
    Idle,
    /// Box select from here (Shift: add).
    Box(Pos2, bool),
    Modal(Box<UvModal>),
}

/// The mesh shown, rebuilt when the scene changes.
struct UvCache {
    rev: u64,
    id: String,
    mesh: Mesh,
    topo: UvTopology,
    material: Material,
    /// Texture asset key, size.
    picture: Option<(String, [u32; 2])>,
}

pub(super) struct UvEditor {
    /// The UV point at the canvas centre.
    center: Vec2,
    /// Pixels per UV unit.
    zoom: f32,
    /// Mesh the view was last framed on.
    framed: Option<String>,
    cache: Option<UvCache>,
    pub gesture: UvGesture,
    /// The pointer is over the panel (its keys go to the UV editor).
    pub hovered: bool,
    /// Last canvas rect.
    canvas: Option<Rect>,
    textures: HashMap<String, (usize, egui::TextureHandle)>,
    checker: Option<egui::TextureHandle>,
    /// In mesh edit mode with faces picked, show only those faces (like Blender).
    only_picked: bool,
    snap: bool,
}

impl Default for UvEditor {
    fn default() -> Self {
        Self {
            center: Vec2::new(0.5, 0.5),
            zoom: 300.0,
            framed: None,
            cache: None,
            gesture: UvGesture::Idle,
            hovered: false,
            canvas: None,
            textures: HashMap::new(),
            checker: None,
            only_picked: true,
            snap: false,
        }
    }
}

impl UvEditor {
    fn to_screen(&self, rect: Rect, uv: [f32; 2]) -> Pos2 {
        rect.center() + (Vec2::new(uv[0], uv[1]) - self.center) * self.zoom
    }
    fn to_uv(&self, rect: Rect, p: Pos2) -> Vec2 {
        self.center + (p - rect.center()) / self.zoom
    }
    /// Show the box `lo..hi` (UV units) in the canvas.
    fn frame(&mut self, rect: Rect, lo: Vec2, hi: Vec2) {
        let size = (hi - lo).max(Vec2::splat(0.05));
        self.center = (lo + hi) / 2.0;
        self.zoom = ((rect.width() - 40.0) / size.x).min((rect.height() - 40.0) / size.y).clamp(20.0, 50_000.0);
    }
}

fn point_in_poly(p: Vec2, poly: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a[1] > p.y) != (b[1] > p.y) && p.x < (b[0] - a[0]) * (p.y - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
    }
    inside
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-9)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

impl EditorApp {
    /// The UV editor's state if open on a mesh that can still be edited (closes it otherwise).
    /// It follows the selection to another editable mesh.
    fn uv_state(&mut self) -> Option<UvState> {
        let scene = self.scene();
        let mut h = self.hub.borrow_mut();
        let mut st = h.uv.clone()?;
        if let Some(sel) = h.selected.clone()
            && sel != st.id
            && crate::hub::editable(&scene, &sel).is_ok()
        {
            st = UvState { id: sel, selection: Vec::new(), ..st };
            h.uv = Some(st.clone());
        }
        if crate::hub::editable(&scene, &st.id).is_err() {
            h.uv = None;
            return None;
        }
        Some(st)
    }

    fn uv_set_sel(&self, sel: impl IntoIterator<Item = Corner>) {
        let set: BTreeSet<Corner> = sel.into_iter().collect();
        if let Some(u) = self.hub.borrow_mut().uv.as_mut() {
            u.selection = set.into_iter().collect();
        }
    }

    fn uv_set_mode(&self, mode: UvSelectMode) {
        let Some(c) = &self.uv.cache else { return };
        let mut h = self.hub.borrow_mut();
        let Some(u) = h.uv.as_mut() else { return };
        u.mode = mode;
        // keep what makes sense in the new mode: whole faces / islands, or the corners
        let sel: BTreeSet<Corner> = u.selection.iter().copied().collect();
        let faces: Vec<u32> = (0..c.mesh.faces.len() as u32)
            .filter(|&f| (0..c.mesh.faces[f as usize].len() as u32).all(|k| sel.contains(&[f, k])))
            .collect();
        u.selection = match mode {
            UvSelectMode::Face => ue::face_corners(&c.mesh, &faces),
            UvSelectMode::Island => ue::face_corners(&c.mesh, &c.topo.island_faces(&faces)),
            _ => u.selection.clone(),
        };
        if faces.is_empty() && matches!(mode, UvSelectMode::Face | UvSelectMode::Island) {
            u.selection.clear();
        }
    }

    /// Open the UV editor on `id`, or close it if it already shows `id`.
    pub(super) fn uv_toggle(&mut self, id: &str) {
        let open = self.hub.borrow().uv.as_ref().is_some_and(|u| u.id == id);
        let p = if open { json!({"id": null}) } else { json!({"id": id}) };
        if let Err(e) = hub::dispatch(&self.hub, json!({"command": "editor.uvEditor", "params": p})) {
            self.toast(crate::lf!("UV editor: {}", "UV 编辑器：{}", e.message));
        }
    }

    pub(super) fn uv_is_open(&self, id: &str) -> bool {
        self.hub.borrow().uv.as_ref().is_some_and(|u| u.id == id)
    }

    fn uv_refresh(&mut self, scene: &Scene, id: &str) -> bool {
        let rev = self.hub.borrow().session.revision();
        if self.uv.cache.as_ref().is_some_and(|c| c.rev == rev && c.id == id) {
            return true;
        }
        let Ok(EntityKind::Mesh { source, material, .. }) = scene.get(id).map(|e| &e.kind) else {
            self.uv.cache = None;
            return false;
        };
        let mesh = match source {
            shotstage_core::MeshSource::Primitive { primitive } => primitive.build().ok(),
            shotstage_core::MeshSource::Data { mesh } => Some(mesh.clone()),
            _ => None,
        };
        let Some(mesh) = mesh else {
            self.uv.cache = None;
            return false;
        };
        let material = material.clone().unwrap_or_default();
        let picture = material
            .texture
            .as_ref()
            .and_then(|k| scene.assets.get(k).map(|a| (k.clone(), [a.width.max(1), a.height.max(1)])));
        let topo = UvTopology::new(&mesh);
        // drop selected corners that no longer exist
        if let Some(u) = self.hub.borrow_mut().uv.as_mut() {
            u.selection.retain(|c| mesh.faces.get(c[0] as usize).is_some_and(|f| (c[1] as usize) < f.len()));
        }
        self.uv.cache = Some(UvCache { rev, id: id.to_string(), mesh, topo, material, picture });
        true
    }

    /// The faces drawn: the faces picked in edit mode (if any and "only picked" is on), else all.
    fn uv_visible(&mut self, id: &str) -> Vec<u32> {
        let n = self.uv.cache.as_ref().map(|c| c.mesh.faces.len()).unwrap_or(0) as u32;
        if self.uv.only_picked
            && let Some(f) = self.edit_faces(id).filter(|f| !f.is_empty())
        {
            return f;
        }
        (0..n).collect()
    }

    /// Picture texture (repeating) of an asset.
    fn uv_texture(&mut self, ctx: &egui::Context, scene: &Scene, key: &str) -> Option<egui::TextureHandle> {
        use base64::Engine as _;
        let asset = scene.assets.get(key)?;
        let identity = asset.data.as_ptr() as usize ^ asset.data.len();
        if let Some((id, t)) = self.uv.textures.get(key)
            && *id == identity
        {
            return Some(t.clone());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(asset.data.as_bytes()).ok()?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
        let tex = ctx.load_texture(format!("uv:{key}"), color, egui::TextureOptions::LINEAR_REPEAT);
        self.uv.textures.insert(key.to_string(), (identity, tex.clone()));
        Some(tex)
    }

    fn uv_checker(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        self.uv
            .checker
            .get_or_insert_with(|| {
                let n = 64;
                let mut px = Vec::with_capacity(n * n);
                for y in 0..n {
                    for x in 0..n {
                        let on = ((x / 8) + (y / 8)) % 2 == 0;
                        px.push(if on { Color32::from_gray(74) } else { Color32::from_gray(58) });
                    }
                }
                let img = egui::ColorImage { size: [n, n], source_size: egui::vec2(n as f32, n as f32), pixels: px };
                ctx.load_texture("uv:checker", img, egui::TextureOptions::NEAREST_REPEAT)
            })
            .clone()
    }

    /// The panel. Called while the UV editor is open.
    pub(super) fn uv_panel(&mut self, ui: &mut egui::Ui) {
        let Some(st) = self.uv_state() else { return };
        let scene = self.scene();
        if !self.uv_refresh(&scene, &st.id) {
            ui.label(tr("This mesh has no editable UV map (rigged people are not unwrapped here)."));
            return;
        }
        self.uv.hovered = ui.rect_contains_pointer(ui.max_rect());
        let has_uvs = self.uv.cache.as_ref().is_some_and(|c| c.mesh.has_uvs());
        // header: what is shown, select mode, close
        ui.horizontal(|ui| {
            ui.strong(tr("UV editor"));
            ui.weak(&st.id);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(tr("✕")).on_hover_text(tr("Close the UV editor")).clicked() {
                    self.uv_toggle(&st.id);
                }
            });
        });
        if !has_uvs {
            ui.add_space(6.0);
            ui.label(tr("This mesh has no UV map yet. Unwrap it to lay its faces flat on the picture:"));
            self.uv_unwrap_buttons(ui, &st, Vec::new());
            ui.add_space(4.0);
            ui.weak(tr("Smart cuts it into flat pieces and packs them; Box / Cylinder / Sphere bake that projection."));
            return;
        }
        ui.horizontal_wrapped(|ui| {
            let mut mode = st.mode;
            for (m, label, key) in [
                (UvSelectMode::Vertex, "Vertex", "1"),
                (UvSelectMode::Edge, "Edge", "2"),
                (UvSelectMode::Face, "Face", "3"),
                (UvSelectMode::Island, "Island", "4"),
            ] {
                ui.selectable_value(&mut mode, m, tr(label)).on_hover_text(tr(key));
            }
            if mode != st.mode {
                self.uv_set_mode(mode);
            }
            let mut sticky = st.sticky;
            if ui
                .checkbox(&mut sticky, tr("Sticky"))
                .on_hover_text(tr("Picking a UV vertex or edge also picks the neighbouring faces' corners at the same spot, so the map stays joined"))
                .changed()
                && let Some(u) = self.hub.borrow_mut().uv.as_mut()
            {
                u.sticky = sticky;
            }
            ui.checkbox(&mut self.uv.snap, tr("Snap")).on_hover_text(tr("Snap moves to the picture's pixels (or hold Ctrl)"));
            if self.edit_state().is_some_and(|e| e.id == st.id) {
                ui.checkbox(&mut self.uv.only_picked, tr("Only picked"))
                    .on_hover_text(tr("Show only the faces picked in the 3D view (when some are)"));
            }
        });
        self.uv_toolbar(ui, &st);
        if let Some(c) = &self.uv.cache
            && c.picture.is_some()
            && c.material.mapping != Mapping::Uv
        {
            let mut use_uv = false;
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    Color32::from_rgb(240, 200, 90),
                    tr("The picture is projected, not laid on by this UV map."),
                );
                use_uv = ui.small_button(tr("Use UV map")).clicked();
            });
            if use_uv {
                self.run("mesh.setMaterial", json!({"id": st.id, "mapping": "uv"}));
            }
        }
        // the canvas takes the rest, leaving a line for the status
        let size = ui.available_size() - egui::vec2(0.0, 20.0);
        let (rect, resp) = ui.allocate_exact_size(size.max(egui::vec2(120.0, 120.0)), Sense::click_and_drag());
        self.uv.canvas = Some(rect);
        if self.uv.framed.as_deref() != Some(st.id.as_str()) {
            self.uv.framed = Some(st.id.clone());
            self.uv_frame_all(rect);
        }
        self.uv_canvas(ui, rect, &resp, &st);
        let (faces, islands) =
            self.uv.cache.as_ref().map(|c| (c.mesh.faces.len(), c.topo.islands.len())).unwrap_or((0, 0));
        let sel = self.hub.borrow().uv.as_ref().map(|u| u.selection.len()).unwrap_or(0);
        ui.weak(crate::lf!(
            "{faces} faces · {islands} islands · {sel} corners picked · G R S · drag · A all · L island · Home fit",
            "{faces} 个面 · {islands} 个孤岛 · 选中 {sel} 个角 · G R S · 拖动 · A 全选 · L 孤岛 · Home 适配"
        ));
    }

    fn uv_unwrap_buttons(&mut self, ui: &mut egui::Ui, st: &UvState, faces: Vec<u32>) {
        ui.horizontal_wrapped(|ui| {
            for (method, label, tip) in [
                ("smart", "Smart", "Cut into flat pieces and pack them without overlap"),
                ("box", "Box", "Bake a box projection"),
                ("planar", "Planar", "Bake a projection from the front"),
                ("cylinder", "Cylinder", "Bake a cylinder projection (can, bottle)"),
                ("sphere", "Sphere", "Bake a sphere projection"),
            ] {
                let label =
                    if method == "smart" { tr(label) } else { super::material::mapping_label(method).to_string() };
                if ui.button(label).on_hover_text(tr(tip)).clicked() {
                    let mut p = json!({"id": st.id, "method": method});
                    if !faces.is_empty() {
                        p["faces"] = json!(faces);
                    }
                    self.run("mesh.unwrap", p);
                }
            }
        });
    }

    /// Faces fully picked in the UV editor, else those picked in edit mode, else none (= all).
    fn uv_picked_faces(&mut self, st: &UvState) -> Vec<u32> {
        let Some(c) = &self.uv.cache else { return Vec::new() };
        let sel: BTreeSet<Corner> = st.selection.iter().copied().collect();
        let faces: Vec<u32> = (0..c.mesh.faces.len() as u32)
            .filter(|&f| (0..c.mesh.faces[f as usize].len() as u32).all(|k| sel.contains(&[f, k])))
            .collect();
        if !faces.is_empty() {
            return faces;
        }
        self.edit_faces(&st.id).unwrap_or_default()
    }

    fn uv_toolbar(&mut self, ui: &mut egui::Ui, st: &UvState) {
        let sel = st.selection.clone();
        let has_sel = !sel.is_empty();
        let mut run: Option<(&str, Value)> = None;
        ui.horizontal_wrapped(|ui| {
            let faces = self.uv_picked_faces(st);
            let scope = tr(if faces.is_empty() { "the whole mesh" } else { "the picked faces" });
            ui.menu_button(tr("Unwrap ▾"), |ui| {
                ui.weak(crate::lf!("Unwrap {scope}", "展开 UV（{scope}）"));
                self.uv_unwrap_buttons(ui, st, faces.clone());
            });
            if ui
                .button(tr("Pack"))
                .on_hover_text(crate::lf!(
                    "Lay out {} again inside the picture without overlap",
                    "把{}在图片内重新排好，互不重叠",
                    tr(if faces.is_empty() { "all islands" } else { "the picked faces' islands" })
                ))
                .clicked()
            {
                run = Some(("mesh.uvPack", json!({"faces": faces, "margin": 0.01})));
            }
            ui.add_enabled_ui(has_sel, |ui| {
                ui.menu_button(tr("Align ▾"), |ui| {
                    for (label, axis, to) in [
                        ("Left", "u", "min"),
                        ("Centre (column)", "u", "center"),
                        ("Right", "u", "max"),
                        ("Top edge", "v", "min"),
                        ("Middle (row)", "v", "center"),
                        ("Bottom edge", "v", "max"),
                    ] {
                        if ui.button(tr(label)).clicked() {
                            run = Some(("mesh.uvAlign", json!({"corners": sel, "axis": axis, "to": to})));
                            ui.close();
                        }
                    }
                });
                if ui.button(tr("Weld")).on_hover_text(tr("Move the picked corners to one point")).clicked() {
                    run = Some(("mesh.uvWeld", json!({"corners": sel})));
                }
                if ui
                    .button(tr("Stitch"))
                    .on_hover_text(tr(
                        "Join each vertex's picked corners at their middle: sews pieces back together along a seam",
                    ))
                    .clicked()
                {
                    run = Some(("mesh.uvWeld", json!({"corners": sel, "mode": "stitch"})));
                }
                for (label, tip, p) in [
                    ("⟲", "Turn 90° counter-clockwise", json!({"rotate": 90})),
                    ("⟳", "Turn 90° clockwise", json!({"rotate": -90})),
                    ("⇋", "Mirror left-right", json!({"scale": [-1, 1]})),
                    ("⇵", "Mirror top-bottom", json!({"scale": [1, -1]})),
                ] {
                    if ui.button(tr(label)).on_hover_text(tr(tip)).clicked() {
                        let mut p = p;
                        p["corners"] = json!(sel);
                        if let Some(c) = &self.uv.cache {
                            p["pivot"] = json!(ue::bounds_center(&c.mesh, &sel).to_array());
                        }
                        run = Some(("mesh.uvTransform", p));
                    }
                }
            });
            if ui.button(tr("Fit")).on_hover_text(tr("Frame everything (Home)")).clicked()
                && let Some(r) = self.uv.canvas
            {
                self.uv_frame_all(r);
            }
        });
        if let Some((cmd, mut p)) = run {
            p["id"] = json!(st.id);
            if let Err(e) = hub::dispatch(&self.hub, json!({"command": cmd, "params": p})) {
                self.toast(e.message);
            }
        }
    }

    fn uv_frame_all(&mut self, rect: Rect) {
        let mut lo = Vec2::ZERO;
        let mut hi = Vec2::splat(1.0);
        if let Some(c) = &self.uv.cache
            && c.mesh.has_uvs()
        {
            for u in c.mesh.uvs.iter().flatten() {
                lo = lo.min(Vec2::new(u[0], u[1]));
                hi = hi.max(Vec2::new(u[0], u[1]));
            }
        }
        self.uv.frame(rect, lo, hi);
    }

    fn uv_frame_selection(&mut self, rect: Rect, sel: &[Corner]) {
        let Some(c) = &self.uv.cache else { return };
        if sel.is_empty() || !c.mesh.has_uvs() {
            return self.uv_frame_all(rect);
        }
        let (lo, hi) = ue::bounds(&c.mesh, sel);
        self.uv.frame(rect, Vec2::new(lo.x, lo.y), Vec2::new(hi.x, hi.y));
    }

    /// The corners a click at `p` picks in `mode` (sticky applied), if any.
    fn uv_element_at(&self, rect: Rect, p: Pos2, st: &UvState, visible: &[u32]) -> Option<Vec<Corner>> {
        let c = self.uv.cache.as_ref()?;
        let m = &c.mesh;
        let scr = |f: u32, k: usize| self.uv.to_screen(rect, m.uvs[f as usize][k]);
        let face_at = || {
            let q = self.uv.to_uv(rect, p);
            visible.iter().rev().copied().find(|&f| point_in_poly(q, &m.uvs[f as usize]))
        };
        let sticky = |v: Vec<Corner>| if st.sticky { c.topo.sticky(&v) } else { v };
        match st.mode {
            UvSelectMode::Vertex => {
                // nearest corner; among corners at one spot, the one whose face is under the pointer
                let under = face_at();
                let mut best: Option<(f32, Corner)> = None;
                for &f in visible {
                    for k in 0..m.faces[f as usize].len() {
                        let d = (scr(f, k) - p).length() + if Some(f) == under { 0.0 } else { 0.01 };
                        if d < 10.0 && best.is_none_or(|b| d < b.0) {
                            best = Some((d, [f, k as u32]));
                        }
                    }
                }
                best.map(|(_, c)| sticky(vec![c]))
            }
            UvSelectMode::Edge => {
                let under = face_at();
                let mut best: Option<(f32, [Corner; 2])> = None;
                for &f in visible {
                    let n = m.faces[f as usize].len();
                    for k in 0..n {
                        let d = seg_dist(p, scr(f, k), scr(f, (k + 1) % n)) + if Some(f) == under { 0.0 } else { 0.01 };
                        if d < 7.0 && best.is_none_or(|b| d < b.0) {
                            best = Some((d, [[f, k as u32], [f, ((k + 1) % n) as u32]]));
                        }
                    }
                }
                best.map(|(_, e)| sticky(e.to_vec()))
            }
            UvSelectMode::Face => face_at().map(|f| ue::face_corners(m, &[f])),
            UvSelectMode::Island => face_at().map(|f| ue::face_corners(m, &c.topo.island_faces(&[f]))),
        }
    }

    fn uv_box_select(&self, rect: Rect, r: Rect, st: &UvState, visible: &[u32], add: bool) {
        let Some(c) = &self.uv.cache else { return };
        let m = &c.mesh;
        let mut sel: BTreeSet<Corner> = if add { st.selection.iter().copied().collect() } else { BTreeSet::new() };
        match st.mode {
            UvSelectMode::Vertex | UvSelectMode::Edge => {
                let mut picked = Vec::new();
                for &f in visible {
                    for k in 0..m.faces[f as usize].len() {
                        if r.contains(self.uv.to_screen(rect, m.uvs[f as usize][k])) {
                            picked.push([f, k as u32]);
                        }
                    }
                }
                if st.sticky {
                    picked = c.topo.sticky(&picked);
                }
                sel.extend(picked);
            }
            UvSelectMode::Face | UvSelectMode::Island => {
                let faces: Vec<u32> = visible
                    .iter()
                    .copied()
                    .filter(|&f| {
                        let uv = &m.uvs[f as usize];
                        let cen = uv.iter().fold([0.0, 0.0], |a, b| [a[0] + b[0], a[1] + b[1]]);
                        let n = uv.len().max(1) as f32;
                        r.contains(self.uv.to_screen(rect, [cen[0] / n, cen[1] / n]))
                    })
                    .collect();
                let faces = if st.mode == UvSelectMode::Island { c.topo.island_faces(&faces) } else { faces };
                sel.extend(ue::face_corners(m, &faces));
            }
        }
        self.uv_set_sel(sel);
    }

    /// Start grab / rotate / scale on the selection.
    pub(super) fn uv_start(&mut self, tool: UvTool, pointer: Pos2, release_confirms: bool) {
        self.uv_cancel();
        let Some(st) = self.hub.borrow().uv.clone() else { return };
        let Some(c) = &self.uv.cache else { return };
        if st.selection.is_empty() || !c.mesh.has_uvs() {
            self.toast(crate::lf!("UV: pick something first", "UV：请先选中元素"));
            return;
        }
        let corners = if st.sticky && matches!(st.mode, UvSelectMode::Vertex | UvSelectMode::Edge) {
            c.topo.sticky(&st.selection)
        } else {
            st.selection.clone()
        };
        let pivot = ue::bounds_center(&c.mesh, &corners);
        let base = c.mesh.clone();
        let key = self.new_key("uv");
        self.uv.gesture = UvGesture::Modal(Box::new(UvModal {
            tool,
            base,
            corners,
            start: pointer,
            pivot: Vec2::new(pivot.x, pivot.y),
            axis: None,
            key,
            changed: false,
            release_confirms,
            last: String::new(),
            info: String::new(),
        }));
    }

    pub(super) fn uv_cancel(&mut self) {
        if let UvGesture::Modal(m) = std::mem::take(&mut self.uv.gesture)
            && m.changed
        {
            self.run("history.discard", json!({}));
        }
    }

    fn uv_confirm(&mut self) {
        self.uv.gesture = UvGesture::Idle;
    }

    /// Follow the pointer: work out the move / turn / scale and send it when it changes.
    fn uv_modal_update(&mut self, rect: Rect, pointer: Pos2, snap: bool, st: &UvState) {
        let picture = self.uv.cache.as_ref().and_then(|c| c.picture.as_ref().map(|p| p.1));
        let zoom = self.uv.zoom;
        let pivot_scr = |m: &UvModal| self.uv.to_screen(rect, [m.pivot.x, m.pivot.y]);
        let UvGesture::Modal(m) = &self.uv.gesture else { return };
        let ps = pivot_scr(m);
        let (mut t, mut r, mut s) = ([0.0f32; 2], 0.0f32, [1.0f32; 2]);
        let lock = |v: &mut [f32; 2], keep: f32| {
            if let Some(a) = m.axis {
                v[1 - a] = keep;
            }
        };
        match m.tool {
            UvTool::Grab => {
                let d = (pointer - m.start) / zoom;
                t = [d.x, d.y];
                if snap {
                    let step = picture.map(|p| [1.0 / p[0] as f32, 1.0 / p[1] as f32]).unwrap_or([1.0 / 64.0; 2]);
                    // snap where the pivot lands, so the selection sits on the pixel grid
                    for i in 0..2 {
                        let at = m.pivot[i] + t[i];
                        t[i] = (at / step[i]).round() * step[i] - m.pivot[i];
                    }
                }
                lock(&mut t, 0.0);
            }
            UvTool::Rotate => {
                let a0 = (m.start - ps).angle();
                let a1 = (pointer - ps).angle();
                r = -(a1 - a0).to_degrees();
                r = (r + 540.0).rem_euclid(360.0) - 180.0;
                if snap {
                    r = (r / 15.0).round() * 15.0;
                }
            }
            UvTool::Scale => {
                let d0 = (m.start - ps).length().max(4.0);
                let k = (pointer - ps).length() / d0;
                let k = if snap { (k * 10.0).round() / 10.0 } else { k };
                s = [k, k];
                lock(&mut s, 1.0);
            }
        }
        let info = match m.tool {
            UvTool::Grab => crate::lf!("Move  Δu {:.3}  Δv {:.3}", "移动  Δu {:.3}  Δv {:.3}", t[0], t[1]),
            UvTool::Rotate => crate::lf!("Rotate  {r:.1}°", "旋转  {r:.1}°"),
            UvTool::Scale => crate::lf!("Scale  {:.3} × {:.3}", "缩放  {:.3} × {:.3}", s[0], s[1]),
        };
        let lock_txt = match m.axis {
            Some(0) => tr(" · U only"),
            Some(_) => tr(" · V only"),
            None => String::new(),
        };
        let sig = format!("{t:?}{r}{s:?}");
        let send = sig != m.last;
        let out =
            if send { ue::transform_corners(&m.base, &m.corners, t, r, s, Some(m.pivot.into())).ok() } else { None };
        let (corners, key) = (m.corners.clone(), m.key.clone());
        if let UvGesture::Modal(m) = &mut self.uv.gesture {
            m.info = format!("{info}{lock_txt}");
            m.last = sig;
        }
        if let Some(out) = out {
            let uvs: Vec<[f32; 2]> = corners.iter().map(|c| out.uvs[c[0] as usize][c[1] as usize]).collect();
            self.run_key(&key, "mesh.uvSet", json!({"id": st.id, "corners": corners, "uvs": uvs}));
            if let UvGesture::Modal(m) = &mut self.uv.gesture {
                m.changed = true;
            }
        }
    }

    fn uv_canvas(&mut self, ui: &mut egui::Ui, rect: Rect, resp: &egui::Response, st: &UvState) {
        let scene = self.scene();
        let visible = self.uv_visible(&st.id);
        let (modifiers, pointer) = ui.input(|i| (i.modifiers, i.pointer.hover_pos()));
        let snap = self.uv.snap != modifiers.command;
        // ---- navigation: wheel zooms about the pointer, two fingers pan, pinch zooms
        if resp.hovered() {
            let (trackpad, delta, pinch) =
                ui.input(|i| (wheel_from_trackpad(&i.events), i.smooth_scroll_delta, i.zoom_delta()));
            let at = pointer.unwrap_or(rect.center());
            let zoom_about = |ed: &mut UvEditor, k: f32| {
                let before = ed.to_uv(rect, at);
                ed.zoom = (ed.zoom * k).clamp(20.0, 50_000.0);
                let after = ed.to_uv(rect, at);
                ed.center += before - after;
            };
            if trackpad == Some(true) || (trackpad.is_none() && self.wheel_trackpad) {
                self.uv.center -= delta / self.uv.zoom;
            } else if delta.y != 0.0 {
                zoom_about(&mut self.uv, (1.0 + delta.y * 0.0015).clamp(0.5, 2.0));
            }
            if pinch != 1.0 && pinch.is_finite() && pinch > 0.0 {
                zoom_about(&mut self.uv, pinch);
            }
        }
        if resp.dragged_by(egui::PointerButton::Middle)
            || (resp.dragged_by(egui::PointerButton::Secondary) && !matches!(self.uv.gesture, UvGesture::Modal(_)))
        {
            self.uv.center -= resp.drag_delta() / self.uv.zoom;
        }
        // ---- tools
        if let UvGesture::Modal(m) = &self.uv.gesture {
            let release = m.release_confirms;
            if let Some(p) = pointer.or(resp.interact_pointer_pos()) {
                self.uv_modal_update(rect, p, snap, st);
            }
            let (enter, esc, released, clicked, right) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                    i.pointer.primary_released(),
                    i.pointer.primary_pressed(),
                    i.pointer.secondary_pressed(),
                )
            });
            if esc || right {
                self.uv_cancel();
            } else if enter || (release && released) || (!release && clicked) {
                self.uv_confirm();
            }
            ui.ctx().request_repaint();
        } else if let UvGesture::Box(start, add) = self.uv.gesture {
            if resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                if let Some(end) = pointer.or(resp.interact_pointer_pos()) {
                    self.uv_box_select(rect, Rect::from_two_pos(start, end), st, &visible, add);
                }
                self.uv.gesture = UvGesture::Idle;
            }
        } else {
            if resp.drag_started_by(egui::PointerButton::Primary)
                && let Some(p) = resp.interact_pointer_pos()
            {
                let start = p - resp.drag_delta();
                match self.uv_element_at(rect, start, st, &visible) {
                    Some(el) => {
                        let sel: BTreeSet<Corner> = st.selection.iter().copied().collect();
                        if !el.iter().all(|c| sel.contains(c)) {
                            if modifiers.shift {
                                self.uv_set_sel(sel.into_iter().chain(el));
                            } else {
                                self.uv_set_sel(el);
                            }
                        }
                        self.uv_start(UvTool::Grab, start, true);
                    }
                    None => self.uv.gesture = UvGesture::Box(start, modifiers.shift),
                }
            } else if resp.double_clicked()
                && let Some(p) = resp.interact_pointer_pos()
            {
                self.uv_select_island_at(rect, p, st, &visible, true);
            } else if resp.clicked()
                && let Some(p) = resp.interact_pointer_pos()
            {
                let el = self.uv_element_at(rect, p, st, &visible);
                let sel: BTreeSet<Corner> = st.selection.iter().copied().collect();
                match el {
                    Some(el) if modifiers.shift => {
                        if el.iter().all(|c| sel.contains(c)) {
                            let drop: BTreeSet<Corner> = el.into_iter().collect();
                            self.uv_set_sel(sel.difference(&drop).copied());
                        } else {
                            self.uv_set_sel(sel.into_iter().chain(el));
                        }
                    }
                    Some(el) => self.uv_set_sel(el),
                    None if !modifiers.shift => self.uv_set_sel([]),
                    None => {}
                }
            }
        }
        // ---- draw
        let st = self.hub.borrow().uv.clone().unwrap_or_else(|| st.clone());
        self.uv_draw(ui, rect, &scene, &st, &visible, pointer.filter(|p| rect.contains(*p)));
    }

    fn uv_select_island_at(&mut self, rect: Rect, p: Pos2, st: &UvState, visible: &[u32], add: bool) {
        let Some(c) = &self.uv.cache else { return };
        let q = self.uv.to_uv(rect, p);
        let Some(f) = visible.iter().rev().copied().find(|&f| point_in_poly(q, &c.mesh.uvs[f as usize])) else {
            return;
        };
        let island = ue::face_corners(&c.mesh, &c.topo.island_faces(&[f]));
        if add {
            self.uv_set_sel(st.selection.iter().copied().chain(island));
        } else {
            self.uv_set_sel(island);
        }
    }

    fn uv_draw(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        scene: &Scene,
        st: &UvState,
        visible: &[u32],
        hover: Option<Pos2>,
    ) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(34));
        // picture (repeating, laid on exactly as the surface shows it) or a checker
        let picture = self.uv.cache.as_ref().and_then(|c| c.picture.clone());
        let material = self.uv.cache.as_ref().map(|c| c.material.clone()).unwrap_or_default();
        let tex = match &picture {
            Some((k, _)) => self.uv_texture(ui.ctx(), scene, k),
            None => None,
        };
        let (tex_id, tex_of): (egui::TextureId, Box<dyn Fn(Vec2) -> Pos2>) = match &tex {
            Some(t) => {
                let m = material.clone();
                (
                    t.id(),
                    Box::new(move |uv: Vec2| {
                        let a = m.adjust([uv.x, uv.y]);
                        egui::pos2(a[0], a[1])
                    }),
                )
            }
            None => (self.uv_checker(ui.ctx()).id(), Box::new(|uv: Vec2| egui::pos2(uv.x, uv.y))),
        };
        let quad = |r: Rect, tint: Color32| {
            let mut mesh = egui::Mesh::with_texture(tex_id);
            for p in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
                mesh.vertices.push(egui::epaint::Vertex { pos: p, uv: tex_of(self.uv.to_uv(rect, p)), color: tint });
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3]);
            mesh
        };
        let unit = Rect::from_two_pos(self.uv.to_screen(rect, [0.0, 0.0]), self.uv.to_screen(rect, [1.0, 1.0]));
        painter.add(quad(rect, Color32::from_gray(if tex.is_some() { 80 } else { 150 })));
        painter.add(quad(unit.intersect(rect), Color32::WHITE));
        painter.rect_stroke(unit, 0.0, egui::Stroke::new(1.0, Color32::from_gray(200)), egui::StrokeKind::Outside);
        // quarter lines
        for i in 1..4 {
            let t = i as f32 / 4.0;
            let s = egui::Stroke::new(1.0, Color32::from_white_alpha(28));
            painter.line_segment([self.uv.to_screen(rect, [t, 0.0]), self.uv.to_screen(rect, [t, 1.0])], s);
            painter.line_segment([self.uv.to_screen(rect, [0.0, t]), self.uv.to_screen(rect, [1.0, t])], s);
        }
        let Some(c) = &self.uv.cache else { return };
        let m = &c.mesh;
        if !m.has_uvs() {
            return;
        }
        let sel: BTreeSet<Corner> = st.selection.iter().copied().collect();
        let scr = |f: u32, k: usize| self.uv.to_screen(rect, m.uvs[f as usize][k]);
        let outline = visible.len() <= OUTLINE_LIMIT;
        // filled selected faces
        let face_sel = |f: u32| (0..m.faces[f as usize].len() as u32).all(|k| sel.contains(&[f, k]));
        for &f in visible {
            if face_sel(f) {
                let pts: Vec<Pos2> = (0..m.faces[f as usize].len()).map(|k| scr(f, k)).collect();
                fill_poly(&painter, &pts, Color32::from_rgba_unmultiplied(255, 152, 40, 85));
            }
        }
        // hover
        let hovered = match (&self.uv.gesture, hover) {
            (UvGesture::Idle, Some(p)) => self.uv_element_at(rect, p, st, visible),
            _ => None,
        };
        if let (Some(h), UvSelectMode::Face | UvSelectMode::Island) = (&hovered, st.mode) {
            let faces: BTreeSet<u32> = h.iter().map(|c| c[0]).collect();
            for f in faces {
                let pts: Vec<Pos2> = (0..m.faces[f as usize].len()).map(|k| scr(f, k)).collect();
                fill_poly(&painter, &pts, Color32::from_white_alpha(30));
            }
        }
        // edges
        let dark = egui::Stroke::new(3.0, Color32::from_black_alpha(150));
        let light = egui::Stroke::new(1.0, Color32::from_gray(225));
        let hot = egui::Stroke::new(2.0, SEL);
        let mut selected_edges = Vec::new();
        for &f in visible {
            let n = m.faces[f as usize].len();
            for k in 0..n {
                let (a, b) = (scr(f, k), scr(f, (k + 1) % n));
                if !rect.expand(4.0).intersects(Rect::from_two_pos(a, b)) {
                    continue;
                }
                if outline {
                    painter.line_segment([a, b], dark);
                }
                if sel.contains(&[f, k as u32]) && sel.contains(&[f, ((k + 1) % n) as u32]) {
                    selected_edges.push([a, b]);
                } else {
                    painter.line_segment([a, b], light);
                }
            }
        }
        for e in selected_edges {
            painter.line_segment(e, hot);
        }
        if let (Some(h), UvSelectMode::Edge) = (&hovered, st.mode) {
            let set: BTreeSet<Corner> = h.iter().copied().collect();
            for &f in visible {
                let n = m.faces[f as usize].len();
                for k in 0..n {
                    if set.contains(&[f, k as u32]) && set.contains(&[f, ((k + 1) % n) as u32]) {
                        painter.line_segment([scr(f, k), scr(f, (k + 1) % n)], egui::Stroke::new(2.0, HOVER));
                    }
                }
            }
        }
        // vertices (one dot per UV vertex)
        if st.mode == UvSelectMode::Vertex && visible.len() <= 40_000 {
            let mut seen: HashMap<u32, bool> = HashMap::new();
            for &f in visible {
                for k in 0..m.faces[f as usize].len() {
                    let g = c.topo.group[f as usize][k];
                    let on = sel.contains(&[f, k as u32]);
                    let e = seen.entry(g).or_insert(false);
                    *e |= on;
                }
            }
            for (g, on) in seen {
                let [f, k] = c.topo.groups[g as usize][0];
                let p = scr(f, k as usize);
                if !rect.contains(p) {
                    continue;
                }
                if on {
                    painter.circle_filled(p, 3.5, SEL);
                } else {
                    painter.circle_filled(p, 2.5, Color32::from_gray(20));
                    painter.circle_filled(p, 1.6, Color32::from_gray(230));
                }
            }
            if let Some(h) = &hovered {
                for c in h {
                    painter.circle_stroke(scr(c[0], c[1] as usize), 5.0, egui::Stroke::new(1.5, HOVER));
                }
            }
        } else if st.mode == UvSelectMode::Face {
            for &f in visible {
                let n = m.faces[f as usize].len();
                let cen = (0..n).fold(Vec2::ZERO, |a, k| a + scr(f, k).to_vec2()) / n.max(1) as f32;
                painter.circle_filled(cen.to_pos2(), 1.8, if face_sel(f) { SEL } else { Color32::from_gray(200) });
            }
        }
        // the gesture
        match &self.uv.gesture {
            UvGesture::Box(start, _) => {
                if let Some(p) = hover.or(ui.input(|i| i.pointer.hover_pos())) {
                    let r = Rect::from_two_pos(*start, p);
                    painter.rect_filled(r, 0.0, Color32::from_white_alpha(14));
                    painter.rect_stroke(
                        r,
                        0.0,
                        egui::Stroke::new(1.0, Color32::from_white_alpha(160)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            UvGesture::Modal(md) => {
                let ps = self.uv.to_screen(rect, [md.pivot.x, md.pivot.y]);
                if md.tool != UvTool::Grab
                    && let Some(p) = hover
                {
                    painter.line_segment([ps, p], egui::Stroke::new(1.0, Color32::from_white_alpha(120)));
                }
                painter.circle_stroke(ps, 4.0, egui::Stroke::new(1.5, Color32::WHITE));
                if let Some(a) = md.axis {
                    let col = if a == 0 { Color32::from_rgb(230, 80, 80) } else { Color32::from_rgb(90, 200, 90) };
                    let (p0, p1) = if a == 0 {
                        (egui::pos2(rect.left(), ps.y), egui::pos2(rect.right(), ps.y))
                    } else {
                        (egui::pos2(ps.x, rect.top()), egui::pos2(ps.x, rect.bottom()))
                    };
                    painter.line_segment([p0, p1], egui::Stroke::new(1.0, col));
                }
                let text = crate::lf!(
                    "{}  ·  X / Y lock · Ctrl snap · Enter applies · Esc cancels",
                    "{}  ·  X / Y 锁定轴 · Ctrl 吸附 · Enter 应用 · Esc 取消",
                    md.info
                );
                let g = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), Color32::WHITE);
                let at = rect.left_top() + egui::vec2(8.0, 8.0);
                painter.rect_filled(Rect::from_min_size(at, g.size()).expand(4.0), 4.0, Color32::from_black_alpha(170));
                painter.galley(at, g, Color32::WHITE);
            }
            UvGesture::Idle => {}
        }
        // a pointer readout in the corner
        if let Some(p) = hover {
            let q = self.uv.to_uv(rect, p);
            let text = match &picture {
                Some((_, [w, h])) => {
                    format!("u {:.3}  v {:.3}   ({:.0}, {:.0} px)", q.x, q.y, q.x * *w as f32, q.y * *h as f32)
                }
                None => format!("u {:.3}  v {:.3}", q.x, q.y),
            };
            painter.text(
                rect.right_bottom() - egui::vec2(6.0, 4.0),
                egui::Align2::RIGHT_BOTTOM,
                text,
                egui::FontId::monospace(11.0),
                Color32::from_gray(200),
            );
        }
    }

    /// Keys while the pointer is over the UV editor. True when the key was the UV editor's.
    pub(super) fn uv_shortcuts(&mut self, ui: &egui::Ui) -> bool {
        let Some(st) = self.hub.borrow().uv.clone() else { return false };
        let Some(rect) = self.uv.canvas else { return false };
        let pointer = ui.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center());
        let pressed = |k: egui::Key| ui.input(|i| i.key_pressed(k) && !i.modifiers.command);
        let (alt, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));
        let mut used = false;
        for (k, mode) in [
            (egui::Key::Num1, UvSelectMode::Vertex),
            (egui::Key::Num2, UvSelectMode::Edge),
            (egui::Key::Num3, UvSelectMode::Face),
            (egui::Key::Num4, UvSelectMode::Island),
        ] {
            if pressed(k) && !matches!(self.uv.gesture, UvGesture::Modal(_)) {
                self.uv_set_mode(mode);
                used = true;
            }
        }
        let modal = matches!(self.uv.gesture, UvGesture::Modal(_));
        for (k, tool) in [(egui::Key::G, UvTool::Grab), (egui::Key::R, UvTool::Rotate), (egui::Key::S, UvTool::Scale)] {
            if !modal && pressed(k) {
                self.uv_start(tool, pointer, false);
                used = true;
            }
        }
        if let UvGesture::Modal(_) = &self.uv.gesture {
            // (a lock key pressed in the same frame as G / R / S counts too)
            for (k, axis) in [(egui::Key::X, 0usize), (egui::Key::Y, 1)] {
                if pressed(k)
                    && let UvGesture::Modal(m) = &mut self.uv.gesture
                {
                    m.axis = if m.axis == Some(axis) { None } else { Some(axis) };
                    m.last.clear();
                }
            }
            return true;
        }
        if pressed(egui::Key::A) {
            let all = !alt;
            let visible = self.uv_visible(&st.id);
            if let Some(c) = &self.uv.cache {
                self.uv_set_sel(if all { ue::face_corners(&c.mesh, &visible) } else { Vec::new() });
            }
            used = true;
        }
        if pressed(egui::Key::L) {
            let visible = self.uv_visible(&st.id);
            self.uv_select_island_at(rect, pointer, &st, &visible, true);
            used = true;
        }
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::L)) {
            // everything joined to the selection
            if let Some(c) = &self.uv.cache {
                let faces: Vec<u32> = st.selection.iter().map(|c| c[0]).collect();
                let more = ue::face_corners(&c.mesh, &c.topo.island_faces(&faces));
                self.uv_set_sel(st.selection.iter().copied().chain(more));
            }
            used = true;
        }
        if pressed(egui::Key::Home) || pressed(egui::Key::F) {
            if shift || st.selection.is_empty() || pressed(egui::Key::Home) {
                self.uv_frame_all(rect);
            } else {
                self.uv_frame_selection(rect, &st.selection);
            }
            used = true;
        }
        if pressed(egui::Key::Escape) {
            used = true;
        }
        // other plain keys must not reach the 3D view (X would delete the object)
        used || ui.input(|i| {
            !i.modifiers.command && i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
        })
    }
}

/// Fill a (possibly concave) polygon by fanning: fine for the convex faces most meshes have.
fn fill_poly(painter: &egui::Painter, pts: &[Pos2], color: Color32) {
    if pts.len() < 3 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    for p in pts {
        mesh.colored_vertex(*p, color);
    }
    for i in 1..pts.len() as u32 - 1 {
        mesh.add_triangle(0, i, i + 1);
    }
    painter.add(mesh);
}
