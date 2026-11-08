//! Mesh edit mode in the viewport, Blender style: Tab on a mesh, then pick vertices, edges or
//! faces (1 / 2 / 3), and grab (G), rotate (R), scale (S), extrude (E), inset (I), bevel
//! (Ctrl+B), loop cut (Ctrl+R or C), edge slide (G G), knife (K), fill (F), connect (J), merge
//! (M), delete (X)...
//!
//! Interactive tools follow the pointer and re-run the real `mesh.*` command each time their
//! amount changes (undoing the previous try first), so the history shows "mesh.bevel" and an
//! agent watching the session sees ordinary commands. Click or Enter applies, Esc or a right
//! click cancels.

use crate::i18n::tr;
use std::collections::HashSet;

use shotstage_core::meshedit::{self as me, Elements};
use shotstage_core::{Mesh, MeshSource};

use super::*;
use crate::hub::{EditState, SelectMode};

const SEL: Color32 = Color32::from_rgb(255, 152, 40);
const WIRE: Color32 = Color32::from_rgba_premultiplied(10, 12, 16, 170);
/// Above this many edges only the selection is drawn (big imported meshes).
const DRAW_LIMIT: usize = 60_000;

/// Geometry of the mesh being edited, rebuilt when the scene changes.
pub(super) struct EditCache {
    rev: u64,
    id: String,
    pub mesh: Mesh,
    pub edges: Vec<[u32; 2]>,
    /// Faces of each edge (same order as `edges`).
    edge_faces: Vec<[Option<u32>; 2]>,
    vert_faces: Vec<Vec<u32>>,
    normals: Vec<Vec3>,
    centers: Vec<Vec3>,
    topo: me::Topology,
}

impl EditCache {
    fn build(rev: u64, id: &str, mesh: Mesh) -> Self {
        let topo = me::Topology::new(&mesh);
        let edges: Vec<[u32; 2]> = topo.edges.iter().map(|&(a, b)| [a, b]).collect();
        let edge_faces = topo
            .edges
            .iter()
            .map(|e| {
                let f = topo.faces_of(*e);
                [f.first().map(|&x| x as u32), f.get(1).map(|&x| x as u32)]
            })
            .collect();
        let vert_faces = topo.vert_faces.iter().map(|v| v.iter().map(|&f| f as u32).collect()).collect();
        let normals = mesh.faces.iter().map(|f| me::face_normal(&mesh, f)).collect();
        let centers = mesh.faces.iter().map(|f| me::face_center(&mesh, f)).collect();
        Self { rev, id: id.to_string(), mesh, edges, edge_faces, vert_faces, normals, centers, topo }
    }
}

/// The cage of the edited mesh as seen this frame.
pub(super) struct Cage {
    /// Revision of the cache it was projected from.
    rev: u64,
    pub world: Mat4,
    pts: Vec<Option<Pos2>>,
    face_vis: Vec<bool>,
    vert_vis: Vec<bool>,
    edge_vis: Vec<bool>,
    /// The selection's centre (world) and its gizmo arrows on screen.
    pub center: Option<Vec3>,
    pub gizmo: Option<(Pos2, [Option<Pos2>; 3])>,
}

/// One element under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Elem {
    Vert(u32),
    Edge([u32; 2]),
    Face(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    Grab,
    Rotate,
    Scale,
    Extrude,
    Inset,
    Bevel,
    LoopCut,
    /// Edge slide (G twice).
    Slide,
    /// Draw a line to cut along (K).
    Knife,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Grab => "Move",
            Tool::Rotate => "Rotate",
            Tool::Scale => "Scale",
            Tool::Extrude => "Extrude",
            Tool::Inset => "Inset",
            Tool::Bevel => "Bevel",
            Tool::LoopCut => "Loop cut",
            Tool::Slide => "Edge slide",
            Tool::Knife => "Knife",
        }
    }
}

/// An interactive tool in progress.
#[derive(Clone)]
pub(super) struct Modal {
    pub tool: Tool,
    /// The mesh before the tool.
    base: Mesh,
    /// Selection before the tool.
    sel: Elements,
    start: Pos2,
    /// Where the start pointer met the plane through the selection facing the view (world).
    start_hit: Vec3,
    center: Vec3,
    pub axis: Option<usize>,
    pub segments: u32,
    /// The command last run as a preview (kept on confirm), whether it changed the scene (so
    /// cancelling undoes it) and why it failed.
    tried: Option<(String, Value)>,
    changed: bool,
    error: Option<String>,
    /// Apply on release (a drag) instead of on the next click.
    pub release_confirms: bool,
    /// Text typed for an exact amount (Blender-style numeric input).
    pub typed: String,
    /// Loop cut: the edge under the pointer and the preview lines (world).
    hover_edge: Option<[u32; 2]>,
    preview: Vec<(Vec3, Vec3)>,
    /// Amount shown in the hint.
    shown: String,
    /// The pointer as last seen.
    pointer: Pos2,
    /// Knife: the points clicked so far (world, on the surface where possible).
    knife: Vec<Vec3>,
    /// Edge slide: one vertex and where it goes on side A and side B (world), to read the
    /// pointer against.
    rail: Option<(Vec3, Vec3, Option<Vec3>)>,
    /// Grab with Cmd / Ctrl: the vertex it snapped to (screen).
    snap_to: Option<Pos2>,
}

/// Amounts used by the panel's tool buttons.
pub(super) struct EditAmounts {
    pub extrude: f32,
    pub inset: f32,
    pub depth: f32,
    pub bevel: f32,
    pub segments: u32,
    pub cuts: u32,
    /// Proportional editing reach (world metres) and fall-off.
    pub prop_radius: f32,
    pub falloff: me::Falloff,
    /// Spin: degrees, steps and axis (0 = X, 1 = Y, 2 = Z).
    pub spin_angle: f32,
    pub spin_steps: u32,
    pub spin_axis: usize,
}

impl Default for EditAmounts {
    fn default() -> Self {
        Self {
            extrude: 0.2,
            inset: 0.05,
            depth: 0.0,
            bevel: 0.05,
            segments: 1,
            cuts: 1,
            prop_radius: 1.0,
            falloff: me::Falloff::Smooth,
            spin_angle: 360.0,
            spin_steps: 12,
            spin_axis: 1,
        }
    }
}

fn plane_hit(o: Vec3, d: Vec3, p: Vec3, n: Vec3) -> Option<Vec3> {
    let denom = d.dot(n);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (p - o).dot(n) / denom;
    (t > 0.0).then(|| o + d * t)
}

fn ray_tri(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/// The selection as it should look in `mode` (Blender converts when you switch).
fn convert(mesh: &Mesh, s: &Elements, mode: SelectMode) -> Elements {
    let Ok(f) = me::flush(mesh, s) else { return Elements::default() };
    match mode {
        SelectMode::Vertex => Elements::vertices(f.verts),
        SelectMode::Edge => Elements::edges(f.edges.into_iter().map(|(a, b)| [a, b])),
        SelectMode::Face => Elements::faces(f.faces),
    }
}

/// Keep only elements that exist in `mesh`.
fn sanitize(mesh: &Mesh, edges: &[[u32; 2]], s: &Elements) -> Elements {
    let nv = mesh.positions.len() as u32;
    let nf = mesh.faces.len() as u32;
    let all: HashSet<[u32; 2]> = edges.iter().copied().collect();
    Elements {
        vertices: s.vertices.iter().copied().filter(|&v| v < nv).collect(),
        edges: s.edges.iter().map(|e| [e[0].min(e[1]), e[0].max(e[1])]).filter(|e| all.contains(e)).collect(),
        faces: s.faces.iter().copied().filter(|&f| f < nf).collect(),
    }
}

impl EditorApp {
    /// The edit state if edit mode is on and still valid (leaves it otherwise).
    pub(super) fn edit_state(&mut self) -> Option<EditState> {
        let scene = self.scene();
        let mut h = self.hub.borrow_mut();
        let e = h.edit.clone()?;
        let ok = crate::hub::editable(&scene, &e.id).is_ok() && h.selected.as_deref() == Some(e.id.as_str());
        if !ok {
            h.edit = None;
            drop(h);
            self.edit_modal = None;
            return None;
        }
        Some(e)
    }

    /// The faces picked in edit mode on `id` (whatever the select mode), or `None` when not
    /// editing it.
    pub(super) fn edit_faces(&mut self, id: &str) -> Option<Vec<u32>> {
        let st = self.edit_state().filter(|s| s.id == id)?;
        let c = self.edit_cache.as_ref()?;
        Some(convert(&c.mesh, &st.selection, SelectMode::Face).faces)
    }

    fn set_edit_sel(&self, sel: Elements) {
        if let Some(e) = self.hub.borrow_mut().edit.as_mut() {
            e.selection = sel.normalized();
        }
    }

    fn set_edit_mode(&mut self, mode: SelectMode) {
        let Some(st) = self.edit_state() else { return };
        let sel = match &self.edit_cache {
            Some(c) => convert(&c.mesh, &st.selection, mode),
            None => Elements::default(),
        };
        if let Some(e) = self.hub.borrow_mut().edit.as_mut() {
            e.mode = mode;
            e.selection = sel;
        }
    }

    pub(super) fn enter_edit(&mut self) {
        let Some(id) = self.selected() else {
            self.toast(tr("Select a mesh, then press Tab to edit its vertices, edges and faces"));
            return;
        };
        let scene = self.scene();
        // a group with a single mesh inside: edit that
        let target = if crate::hub::editable(&scene, &id).is_ok() {
            id
        } else {
            let meshes: Vec<String> =
                spatial::subtree(&scene, &id).into_iter().filter(|c| crate::hub::editable(&scene, c).is_ok()).collect();
            match meshes.as_slice() {
                [one] => one.clone(),
                [] => {
                    let why = crate::hub::editable(&scene, &id).err().map(|e| e.message).unwrap_or_default();
                    self.toast(crate::lf!("Only meshes can be edited: {why}", "只有网格可以编辑：{why}"));
                    return;
                }
                _ => {
                    self.toast(tr("This is a group: click again to pick one part, then press Tab"));
                    return;
                }
            }
        };
        self.also.clear();
        match hub::dispatch(&self.hub, json!({"command": "editor.editMesh", "params": {"id": target}})) {
            Ok(_) => self.toast(tr("Edit mode · 1 vertex / 2 edge / 3 face · G move · E extrude · Tab to finish")),
            Err(e) => self.toast(crate::lf!("Cannot edit: {}", "无法编辑：{}", e.message)),
        }
    }

    pub(super) fn leave_edit(&mut self) {
        self.edit_cancel();
        self.hub.borrow_mut().edit = None;
    }

    /// Rebuild the cache if the scene changed; clean the selection of stale indices.
    pub(super) fn edit_refresh(&mut self, scene: &Scene, id: &str) -> bool {
        let rev = self.hub.borrow().session.revision();
        let stale = self.edit_cache.as_ref().is_none_or(|c| c.rev != rev || c.id != id);
        if stale {
            let mesh = match &scene.get(id).map(|e| &e.kind) {
                Ok(EntityKind::Mesh { source, .. }) => match source {
                    MeshSource::Primitive { primitive } => primitive.build().ok(),
                    MeshSource::Data { mesh } => Some(mesh.clone()),
                    MeshSource::Skinned { .. } | MeshSource::Human { .. } => None,
                },
                _ => None,
            };
            let Some(mesh) = mesh else {
                self.edit_cache = None;
                return false;
            };
            let c = EditCache::build(rev, id, mesh);
            if let Some(e) = self.hub.borrow_mut().edit.as_mut() {
                e.selection = sanitize(&c.mesh, &c.edges, &e.selection);
            }
            self.edit_cache = Some(c);
        }
        true
    }

    /// Project the cage for this frame.
    pub(super) fn edit_cage(
        &self,
        scene: &Scene,
        id: &str,
        f: f32,
        rect: Rect,
        view: &View,
        xray: bool,
    ) -> Option<Cage> {
        let c = self.edit_cache.as_ref()?;
        let world = world_at(scene, id, f).ok()?;
        let eye = view.view.inverse().w_axis.truncate();
        let flip = world.determinant() < 0.0;
        let pts: Vec<Option<Pos2>> = c
            .mesh
            .positions
            .iter()
            .map(|p| project(rect, view, world.transform_point3(Vec3::from_array(*p))))
            .collect();
        let face_vis: Vec<bool> = (0..c.mesh.faces.len())
            .map(|i| {
                if xray {
                    return true;
                }
                let n = world.transform_vector3(c.normals[i]) * if flip { -1.0 } else { 1.0 };
                let p = world.transform_point3(c.centers[i]);
                n == Vec3::ZERO || n.dot(eye - p) > 0.0
            })
            .collect();
        let vert_vis: Vec<bool> = (0..c.mesh.positions.len())
            .map(|v| xray || c.vert_faces[v].is_empty() || c.vert_faces[v].iter().any(|&f| face_vis[f as usize]))
            .collect();
        let edge_vis: Vec<bool> = c
            .edge_faces
            .iter()
            .map(|fs| xray || fs.iter().flatten().any(|&f| face_vis[f as usize]) || fs[1].is_none())
            .collect();
        let st = self.hub.borrow().edit.clone()?;
        let center = me::flush_with(&c.mesh, &c.topo, &st.selection)
            .ok()
            .filter(|fl| !fl.verts.is_empty())
            .map(|fl| world.transform_point3(me::selection_center(&c.mesh, &fl.verts)));
        let gizmo = center.and_then(|o| {
            let len = (eye - o).length().max(0.1) * 0.12;
            let cp = project(rect, view, o)?;
            let arrows = [Vec3::X, Vec3::Y, Vec3::Z].map(|a| project(rect, view, o + a * len));
            Some((cp, arrows))
        });
        Some(Cage { rev: c.rev, world, pts, face_vis, vert_vis, edge_vis, center, gizmo })
    }

    /// The face hit by a ray (nearest), and how far along the ray.
    /// The cache, if `cage` was projected from it (a tool may have changed the mesh since).
    fn cache_for(&self, cage: &Cage) -> Option<&EditCache> {
        self.edit_cache.as_ref().filter(|c| c.rev == cage.rev)
    }

    fn edit_ray_face(&self, cage: &Cage, o: Vec3, d: Vec3, xray: bool) -> Option<(u32, f32)> {
        let c = self.cache_for(cage)?;
        let mut best: Option<(u32, f32)> = None;
        for (fi, f) in c.mesh.faces.iter().enumerate() {
            if !xray && !cage.face_vis[fi] {
                continue;
            }
            let w = |v: u32| cage.world.transform_point3(Vec3::from_array(c.mesh.positions[v as usize]));
            let a = w(f[0]);
            for i in 1..f.len() - 1 {
                if let Some(t) = ray_tri(o, d, a, w(f[i]), w(f[i + 1]))
                    && best.is_none_or(|b| t < b.1)
                {
                    best = Some((fi as u32, t));
                }
            }
        }
        best
    }

    /// The element under the pointer in the current select mode.
    pub(super) fn edit_pick(
        &self,
        cage: &Cage,
        mode: SelectMode,
        pos: Pos2,
        ray: (Vec3, Vec3),
        xray: bool,
    ) -> Option<Elem> {
        let c = self.cache_for(cage)?;
        let (o, d) = ray;
        let hit = self.edit_ray_face(cage, o, d, xray);
        // vertices and edges behind the surface under the pointer are hidden
        let depth_ok = |p: Vec3| xray || hit.is_none_or(|(_, t)| (p - o).dot(d) <= t * 1.02 + 1e-3);
        let w = |v: u32| cage.world.transform_point3(Vec3::from_array(c.mesh.positions[v as usize]));
        match mode {
            SelectMode::Vertex => {
                let mut best: Option<(u32, f32)> = None;
                for (v, p) in cage.pts.iter().enumerate() {
                    let Some(p) = p else { continue };
                    let dist = (*p - pos).length();
                    if dist < 14.0 && cage.vert_vis[v] && best.is_none_or(|b| dist < b.1) && depth_ok(w(v as u32)) {
                        best = Some((v as u32, dist));
                    }
                }
                best.map(|b| Elem::Vert(b.0))
            }
            SelectMode::Edge => self.edit_pick_edge(cage, pos, &depth_ok).map(Elem::Edge),
            SelectMode::Face => hit.map(|h| Elem::Face(h.0)),
        }
    }

    fn edit_pick_edge(&self, cage: &Cage, pos: Pos2, depth_ok: &dyn Fn(Vec3) -> bool) -> Option<[u32; 2]> {
        let c = self.cache_for(cage)?;
        let w = |v: u32| cage.world.transform_point3(Vec3::from_array(c.mesh.positions[v as usize]));
        let mut best: Option<([u32; 2], f32)> = None;
        for (i, e) in c.edges.iter().enumerate() {
            let (Some(a), Some(b)) = (cage.pts[e[0] as usize], cage.pts[e[1] as usize]) else { continue };
            if !cage.edge_vis[i] {
                continue;
            }
            let dist = seg_dist(pos, a, b);
            if dist < 10.0 && best.is_none_or(|x| dist < x.1) && depth_ok((w(e[0]) + w(e[1])) / 2.0) {
                best = Some((*e, dist));
            }
        }
        best.map(|b| b.0)
    }

    fn edit_contains(sel: &Elements, e: Elem) -> bool {
        match e {
            Elem::Vert(v) => sel.vertices.contains(&v),
            Elem::Edge(x) => sel.edges.contains(&[x[0].min(x[1]), x[0].max(x[1])]),
            Elem::Face(f) => sel.faces.contains(&f),
        }
    }

    fn edit_toggle(sel: &mut Elements, e: Elem, add_only: bool) {
        match e {
            Elem::Vert(v) => match sel.vertices.iter().position(|x| *x == v) {
                Some(i) if !add_only => {
                    sel.vertices.remove(i);
                }
                Some(_) => {}
                None => sel.vertices.push(v),
            },
            Elem::Edge(x) => {
                let x = [x[0].min(x[1]), x[0].max(x[1])];
                match sel.edges.iter().position(|y| *y == x) {
                    Some(i) if !add_only => {
                        sel.edges.remove(i);
                    }
                    Some(_) => {}
                    None => sel.edges.push(x),
                }
            }
            Elem::Face(f) => match sel.faces.iter().position(|x| *x == f) {
                Some(i) if !add_only => {
                    sel.faces.remove(i);
                }
                Some(_) => {}
                None => sel.faces.push(f),
            },
        }
    }

    fn single(e: Elem) -> Elements {
        let mut s = Elements::default();
        Self::edit_toggle(&mut s, e, true);
        s
    }

    /// Click in edit mode: select (Shift toggles, Alt selects a loop).
    pub(super) fn edit_click(&mut self, cage: &Cage, pos: Pos2, ray: (Vec3, Vec3), modifiers: egui::Modifiers) {
        let Some(st) = self.edit_state() else { return };
        let xray = self.edit_xray;
        if modifiers.alt {
            self.edit_loop_select(cage, pos, ray, modifiers.shift);
            return;
        }
        let hit = self.edit_pick(cage, st.mode, pos, ray, xray);
        let mut sel = if modifiers.shift { st.selection.clone() } else { Elements::default() };
        if let Some(e) = hit {
            Self::edit_toggle(&mut sel, e, !modifiers.shift);
        }
        self.set_edit_sel(sel);
    }

    /// Alt+click: the edge loop (vertex / edge mode) or face loop (face mode) nearest the pointer.
    fn edit_loop_select(&mut self, cage: &Cage, pos: Pos2, ray: (Vec3, Vec3), add: bool) {
        let Some(st) = self.edit_state() else { return };
        let (o, d) = ray;
        let hit = self.edit_ray_face(cage, o, d, self.edit_xray);
        let xray = self.edit_xray;
        let depth_ok = |p: Vec3| xray || hit.is_none_or(|(_, t)| (p - o).dot(d) <= t * 1.02 + 1e-3);
        // the edge near the pointer, else the nearest edge of the face under it
        let near = self.edit_pick_edge(cage, pos, &depth_ok).or_else(|| {
            let c = self.cache_for(cage)?;
            let f = &c.mesh.faces[hit?.0 as usize];
            (0..f.len())
                .filter_map(|i| {
                    let (a, b) = (f[i], f[(i + 1) % f.len()]);
                    let d = seg_dist(pos, cage.pts[a as usize]?, cage.pts[b as usize]?);
                    Some(([a.min(b), a.max(b)], d))
                })
                .min_by(|x, y| x.1.total_cmp(&y.1))
                .map(|x| x.0)
        });
        let Some(edge) = near else { return };
        let how = if st.mode == SelectMode::Face { "faceLoop" } else { "loop" };
        let Some(r) = self.run("mesh.select", json!({"id": st.id, "how": how, "edge": edge})) else { return };
        let Ok(found) = serde_json::from_value::<Elements>(r["selection"].clone()) else { return };
        let Some(c) = self.edit_cache.as_ref() else { return };
        let found = convert(&c.mesh, &found, st.mode);
        let mut sel = if add { st.selection.clone() } else { Elements::default() };
        sel.vertices.extend(found.vertices);
        sel.edges.extend(found.edges);
        sel.faces.extend(found.faces);
        self.set_edit_sel(sel);
    }

    /// Double-click / L: everything connected to the element under the pointer.
    pub(super) fn edit_select_linked_at(&mut self, cage: &Cage, pos: Pos2, ray: (Vec3, Vec3), add: bool) {
        let Some(st) = self.edit_state() else { return };
        let Some(e) = self.edit_pick(cage, st.mode, pos, ray, self.edit_xray) else { return };
        self.edit_select_how("linked", Some(Self::single(e)), add);
    }

    /// Run `mesh.select` with `how` on the selection (or `from`) and take the result.
    pub(super) fn edit_select_how(&mut self, how: &str, from: Option<Elements>, add: bool) {
        let Some(st) = self.edit_state() else { return };
        let src = from.unwrap_or_else(|| st.selection.clone());
        let mut p = serde_json::to_value(&src).unwrap_or(json!({}));
        p["id"] = json!(st.id);
        p["how"] = json!(how);
        let Some(r) = self.run("mesh.select", p) else { return };
        let Ok(found) = serde_json::from_value::<Elements>(r["selection"].clone()) else { return };
        let Some(c) = self.edit_cache.as_ref() else { return };
        let found = convert(&c.mesh, &found, st.mode);
        let mut sel = if add { st.selection.clone() } else { Elements::default() };
        sel.vertices.extend(found.vertices);
        sel.edges.extend(found.edges);
        sel.faces.extend(found.faces);
        self.set_edit_sel(sel);
    }

    /// Box select: visible elements inside `r` (Shift adds).
    pub(super) fn edit_select_rect(&mut self, cage: &Cage, r: Rect, add: bool) {
        let Some(st) = self.edit_state() else { return };
        let Some(c) = self.cache_for(cage) else { return };
        let inside = |v: u32| cage.pts[v as usize].is_some_and(|p| r.contains(p));
        let mut sel = if add { st.selection.clone() } else { Elements::default() };
        match st.mode {
            SelectMode::Vertex => {
                sel.vertices
                    .extend((0..c.mesh.positions.len() as u32).filter(|&v| cage.vert_vis[v as usize] && inside(v)));
            }
            SelectMode::Edge => {
                for (i, e) in c.edges.iter().enumerate() {
                    if cage.edge_vis[i] && inside(e[0]) && inside(e[1]) {
                        sel.edges.push(*e);
                    }
                }
            }
            SelectMode::Face => {
                for (fi, f) in c.mesh.faces.iter().enumerate() {
                    if cage.face_vis[fi] && f.iter().all(|&v| inside(v)) {
                        sel.faces.push(fi as u32);
                    }
                }
            }
        }
        self.set_edit_sel(sel);
    }

    pub(super) fn edit_select_all(&mut self, all: bool) {
        let Some(st) = self.edit_state() else { return };
        let Some(c) = self.edit_cache.as_ref() else { return };
        let sel = if !all {
            Elements::default()
        } else {
            match st.mode {
                SelectMode::Vertex => Elements::vertices(0..c.mesh.positions.len() as u32),
                SelectMode::Edge => Elements::edges(c.edges.clone()),
                SelectMode::Face => Elements::faces(0..c.mesh.faces.len() as u32),
            }
        };
        self.set_edit_sel(sel);
    }

    /// Run an edit command on the selection; the result's selection becomes the selection.
    pub(super) fn edit_apply(&mut self, command: &str, extra: Value) -> bool {
        let Some(st) = self.edit_state() else { return false };
        // (a loop cut takes an edge and the knife its own faces, not the selection)
        let mut p = if matches!(command, "mesh.loopCut" | "mesh.knife") {
            json!({})
        } else {
            serde_json::to_value(&st.selection).unwrap_or(json!({}))
        };
        p["id"] = json!(st.id);
        if let (Some(o), Some(x)) = (p.as_object_mut(), extra.as_object()) {
            for (k, v) in x {
                o.insert(k.clone(), v.clone());
            }
        }
        match hub::dispatch(&self.hub, json!({"command": command, "params": p})) {
            Ok(o) => {
                if let Ok(s) = serde_json::from_value::<Elements>(o.result["selection"].clone()) {
                    self.edit_adopt(s);
                }
                true
            }
            Err(e) => {
                self.toast(tr(e.message));
                false
            }
        }
    }

    /// Take a tool's resulting selection, in the current select mode.
    fn edit_adopt(&mut self, s: Elements) {
        let Some(st) = self.edit_state() else { return };
        let scene = self.scene();
        self.edit_refresh(&scene, &st.id);
        let Some(c) = self.edit_cache.as_ref() else { return };
        // a loop cut makes edges: show them in edge mode (vertex mode keeps its vertices)
        let mode = if !s.edges.is_empty() && s.faces.is_empty() && st.mode == SelectMode::Face {
            SelectMode::Edge
        } else {
            st.mode
        };
        let s = convert(&c.mesh, &s, mode);
        if let Some(e) = self.hub.borrow_mut().edit.as_mut() {
            e.mode = mode;
            e.selection = s;
        }
    }

    /// Start an interactive tool at the pointer.
    pub(super) fn edit_start(&mut self, tool: Tool, pointer: Pos2, axis: Option<usize>, release_confirms: bool) {
        self.edit_cancel();
        let Some(st) = self.edit_state() else { return };
        let Some((rect, view)) = self.last_view else { return };
        let scene = self.scene();
        let f = self.frame();
        let Some(cage) = self.edit_cage(&scene, &st.id, f, rect, &view, self.edit_xray) else { return };
        if !matches!(tool, Tool::LoopCut | Tool::Knife) && st.selection.is_empty() {
            self.toast(crate::lf!("{}: select something first", "{}：请先选中一些元素", tr(tool.name())));
            return;
        }
        let center = cage.center.unwrap_or_else(|| cage.world.w_axis.truncate());
        let (o, d) = ray_at(rect, &view, pointer);
        let n = (view.view.inverse().w_axis.truncate() - center).normalize_or_zero();
        let start_hit = plane_hit(o, d, center, n).unwrap_or(center);
        let base = self.edit_cache.as_ref().map(|c| c.mesh.clone()).unwrap_or_default();
        let mut rail = None;
        if tool == Tool::Slide {
            let rails = match me::slide_rails(&base, &st.selection) {
                Ok(r) => r,
                Err(e) => {
                    self.toast(crate::lf!("Edge slide: {e}", "滑边：{e}"));
                    return;
                }
            };
            // read the pointer against the rail of the vertex nearest to it
            let w = |v: u32| cage.world.transform_point3(Vec3::from_array(base.positions[v as usize]));
            let best = rails
                .iter()
                .filter_map(|(&v, r)| {
                    let a = r[0].or(r[1])?;
                    let b = if r[0].is_some() { r[1] } else { None };
                    let d = cage.pts.get(v as usize).copied().flatten().map(|p| (p - pointer).length()).unwrap_or(1e9);
                    Some((d, v, a, b, r[0].is_none()))
                })
                .min_by(|x, y| x.0.total_cmp(&y.0));
            match best {
                // a vertex with only side B: read the pointer along B, negated
                Some((_, v, a, _, true)) => rail = Some((w(v), w(v), Some(w(a)))),
                Some((_, v, a, b, false)) => rail = Some((w(v), w(a), b.map(w))),
                None => {
                    self.toast(tr("Edge slide: no neighbouring edges to slide along"));
                    return;
                }
            }
        }
        self.edit_modal = Some(Modal {
            tool,
            base,
            sel: st.selection.clone(),
            start: pointer,
            start_hit,
            center,
            axis,
            segments: if tool == Tool::Bevel {
                self.edit_amounts.segments.max(1)
            } else {
                self.edit_amounts.cuts.max(1)
            },
            tried: None,
            changed: false,
            error: None,
            release_confirms,
            typed: String::new(),
            hover_edge: None,
            preview: Vec::new(),
            shown: String::new(),
            pointer,
            knife: Vec::new(),
            rail,
            snap_to: None,
        });
    }

    /// Undo the preview and drop the tool.
    pub(super) fn edit_cancel(&mut self) {
        if let Some(m) = self.edit_modal.take() {
            if m.changed {
                self.run("history.discard", json!({}));
            }
            self.set_edit_sel(m.sel);
        }
    }

    /// Keep the preview as the result.
    pub(super) fn edit_confirm(&mut self) {
        let Some(mut m) = self.edit_modal.take() else { return };
        if m.tool == Tool::Knife {
            // clicking the last point again finishes, like Enter
            let last = m.knife.last().and_then(|p| self.last_view.and_then(|(r, v)| project(r, &v, *p)));
            if last.is_some_and(|l| (l - m.pointer).length() < 6.0) {
                self.edit_knife(m.knife);
                return;
            }
            if let Some(p) = self.edit_knife_point(m.pointer, m.knife.last().copied()) {
                m.knife.push(p);
            }
            self.edit_modal = Some(m);
            return;
        }
        if m.tool == Tool::LoopCut {
            if let Some(e) = m.hover_edge {
                let cuts = m.segments;
                self.edit_amounts.cuts = cuts;
                self.edit_apply("mesh.loopCut", json!({"edge": e, "cuts": cuts}));
            }
            return;
        }
        if m.tool == Tool::Bevel {
            self.edit_amounts.segments = m.segments;
        }
        // the preview already is the real command
    }

    /// Follow the pointer: work out the tool's amount and re-run its command when it changes.
    pub(super) fn edit_modal_update(
        &mut self,
        cage: &Cage,
        rect: Rect,
        view: &View,
        pointer: Pos2,
        modifiers: egui::Modifiers,
        scroll: f32,
    ) {
        let Some(st) = self.edit_state() else { return };
        let Some(mut m) = self.edit_modal.clone() else { return };
        m.pointer = pointer;
        let (o, d) = ray_at(rect, view, pointer);
        let world = cage.world;
        let inv = world.inverse();
        let eye = view.view.inverse().w_axis.truncate();
        let cs = project(rect, view, m.center).unwrap_or(rect.center());
        let right = view.view.inverse().x_axis.truncate().normalize_or_zero();
        let px_per_m = project(rect, view, m.center + right).map(|p| (p - cs).length()).unwrap_or(100.0).max(1e-3);
        let wpp = 1.0 / px_per_m;
        let obj_scale =
            ((world.x_axis.truncate().length() + world.y_axis.truncate().length() + world.z_axis.truncate().length())
                / 3.0)
                .max(1e-6);
        let typed: Option<f32> = m.typed.parse::<f32>().ok();
        let snap =
            |v: f32| if self.snap && !modifiers.command { (v / self.snap_step).round() * self.snap_step } else { v };
        if scroll != 0.0 && self.edit_prop && matches!(m.tool, Tool::Grab | Tool::Rotate | Tool::Scale) {
            let r = &mut self.edit_amounts.prop_radius;
            *r = (*r * (1.0 + scroll * 0.002).clamp(0.5, 2.0)).clamp(0.001, 1000.0);
        }
        if scroll != 0.0 && matches!(m.tool, Tool::Bevel | Tool::LoopCut) {
            self.edit_scroll += scroll;
            let steps = (self.edit_scroll / 60.0).trunc();
            if steps != 0.0 {
                self.edit_scroll -= steps * 60.0;
                m.segments = (m.segments as i32 + steps as i32).clamp(1, 16) as u32;
            }
        }
        let prop = (self.edit_prop && matches!(m.tool, Tool::Grab | Tool::Rotate | Tool::Scale)).then(
            || json!({"radius": self.edit_amounts.prop_radius / obj_scale, "falloff": self.edit_amounts.falloff}),
        );
        let sel_json = |extra: Value| {
            let mut p = serde_json::to_value(&m.sel).unwrap_or(json!({}));
            p["id"] = json!(st.id);
            if let Some(pr) = &prop {
                p["proportional"] = pr.clone();
            }
            if let (Some(o), Some(x)) = (p.as_object_mut(), extra.as_object()) {
                for (k, v) in x {
                    o.insert(k.clone(), v.clone());
                }
            }
            p
        };
        let axis_vec = m.axis.map(|i| [Vec3::X, Vec3::Y, Vec3::Z][i]);
        let (so, sd) = ray_at(rect, view, m.start);
        let pivot = inv.transform_point3(m.center).to_array();
        let want: Option<(String, Value)> = match m.tool {
            Tool::Grab if (modifiers.command || modifiers.ctrl) && typed.is_none() => {
                // hold Cmd / Ctrl: snap onto the nearest vertex under the pointer
                let moving = me::flush(&m.base, &m.sel).map(|f| f.verts).unwrap_or_default();
                let target = cage
                    .pts
                    .iter()
                    .enumerate()
                    .filter(|(v, p)| {
                        p.is_some()
                            && !moving.contains(&(*v as u32))
                            && (self.edit_xray || cage.vert_vis.get(*v).copied().unwrap_or(false))
                    })
                    .filter_map(|(v, p)| p.map(|p| ((p - pointer).length(), v)))
                    .filter(|(d, _)| *d < 16.0)
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                let source = if moving.len() == 1 {
                    moving
                        .iter()
                        .next()
                        .map(|&v| world.transform_point3(Vec3::from_array(m.base.positions[v as usize])))
                } else {
                    Some(m.center)
                };
                match (target, source) {
                    (Some((_, v)), Some(src)) => {
                        let tw = world.transform_point3(Vec3::from_array(m.base.positions[v]));
                        m.snap_to = cage.pts[v];
                        let delta = tw - src;
                        m.shown = format!("snapped to vertex {v} · {:.3} m", delta.length());
                        Some((
                            "mesh.transformElements".into(),
                            sel_json(json!({"translate": inv.transform_vector3(delta).to_array()})),
                        ))
                    }
                    _ => {
                        m.snap_to = None;
                        m.shown = "hold over a vertex to snap to it".into();
                        m.tried.clone()
                    }
                }
            }
            Tool::Grab => {
                m.snap_to = None;
                let delta = match axis_vec {
                    Some(a) => {
                        let t0 = closest_on_line(m.center, a, so, sd).unwrap_or(0.0);
                        let t = closest_on_line(m.center, a, o, d).unwrap_or(t0);
                        a * typed.unwrap_or_else(|| snap(t - t0))
                    }
                    None => {
                        let n = (eye - m.center).normalize_or_zero();
                        let p = plane_hit(o, d, m.center, n).unwrap_or(m.start_hit);
                        let v = p - m.start_hit;
                        Vec3::new(snap(v.x), snap(v.y), snap(v.z))
                    }
                };
                m.shown = format!("{:.3} m", delta.length());
                Some((
                    "mesh.transformElements".into(),
                    sel_json(json!({"translate": inv.transform_vector3(delta).to_array()})),
                ))
            }
            Tool::Rotate => {
                let a0 = (-(m.start.y - cs.y)).atan2(m.start.x - cs.x);
                let a1 = (-(pointer.y - cs.y)).atan2(pointer.x - cs.x);
                let mut deg = (a1 - a0).to_degrees();
                if self.snap && !modifiers.command {
                    deg = (deg / 15.0).round() * 15.0;
                }
                let deg = typed.unwrap_or(deg);
                let toward = (eye - m.center).normalize_or_zero();
                let axis = match axis_vec {
                    Some(a) => a * if toward.dot(a) >= 0.0 { 1.0 } else { -1.0 },
                    None => toward,
                };
                let q_world = glam::Quat::from_axis_angle(axis, deg.to_radians());
                let (_, r, _) = world.to_scale_rotation_translation();
                let q_local = r.inverse() * q_world * r;
                let (y, x, z) = q_local.to_euler(glam::EulerRot::YXZ);
                m.shown = format!("{deg:.1}°");
                Some((
                    "mesh.transformElements".into(),
                    sel_json(json!({"rotate": [x.to_degrees(), y.to_degrees(), z.to_degrees()], "pivot": pivot})),
                ))
            }
            Tool::Scale => {
                let d0 = (m.start - cs).length().max(4.0);
                let k = typed.unwrap_or((pointer - cs).length() / d0);
                let k = if self.snap && !modifiers.command && typed.is_none() { (k * 10.0).round() / 10.0 } else { k };
                let mut s = [k; 3];
                if let Some(i) = m.axis {
                    s = [1.0; 3];
                    s[i] = k;
                }
                m.shown = format!("×{k:.2}");
                Some(("mesh.transformElements".into(), sel_json(json!({"scale": s, "pivot": pivot}))))
            }
            Tool::Extrude => {
                let faces = me::flush(&m.base, &m.sel).map(|f| f.faces).unwrap_or_default();
                if !faces.is_empty() {
                    let n_local = me::region_normal(&m.base, faces.iter().copied());
                    let n_world = world.transform_vector3(n_local);
                    let k = n_world.length().max(1e-6);
                    let nw = n_world / k;
                    let dist = match typed {
                        Some(v) => v,
                        None => {
                            let t0 = closest_on_line(m.center, nw, so, sd).unwrap_or(0.0);
                            let t = closest_on_line(m.center, nw, o, d).unwrap_or(t0);
                            snap(t - t0) / k
                        }
                    };
                    m.shown = format!("{:.3} m", dist * k);
                    Some(("mesh.extrude".into(), sel_json(json!({"distance": dist}))))
                } else {
                    let n = (eye - m.center).normalize_or_zero();
                    let p = plane_hit(o, d, m.center, n).unwrap_or(m.start_hit);
                    let v = p - m.start_hit;
                    let v = Vec3::new(snap(v.x), snap(v.y), snap(v.z));
                    m.shown = format!("{:.3} m", v.length());
                    Some(("mesh.extrude".into(), sel_json(json!({"offset": inv.transform_vector3(v).to_array()}))))
                }
            }
            Tool::Inset => {
                let d0 = (m.start - cs).length();
                let t = typed.unwrap_or_else(|| ((pointer - cs).length() - d0).abs() * wpp / obj_scale);
                m.shown = format!("{:.3} m", t * obj_scale);
                (t > 0.0).then(|| {
                    ("mesh.inset".to_string(), sel_json(json!({"thickness": t, "depth": self.edit_amounts.depth})))
                })
            }
            Tool::Bevel => {
                let d0 = (m.start - cs).length();
                let w = typed.unwrap_or_else(|| ((pointer - cs).length() - d0).abs() * wpp / obj_scale);
                m.shown = format!(
                    "{:.3} m · {} segment{} (scroll)",
                    w * obj_scale,
                    m.segments,
                    if m.segments == 1 { "" } else { "s" }
                );
                (w > 1e-5).then(|| ("mesh.bevel".to_string(), sel_json(json!({"width": w, "segments": m.segments}))))
            }
            Tool::LoopCut => {
                // hover an edge: show where the cuts go
                let hit = self.edit_ray_face(cage, o, d, self.edit_xray);
                let depth_ok = |p: Vec3| hit.is_none_or(|(_, t)| (p - o).dot(d) <= t * 1.02 + 1e-3);
                let edge = self.edit_pick_edge(cage, pointer, &depth_ok);
                if edge != m.hover_edge || m.preview.is_empty() || scroll != 0.0 {
                    m.hover_edge = edge;
                    m.preview.clear();
                    if let (Some(e), Some(c)) = (edge, self.edit_cache.as_ref())
                        && let Ok((out, sel)) = me::loop_cut(&c.mesh, e, m.segments, 0.5)
                    {
                        for x in sel.edges {
                            let w = |v: u32| world.transform_point3(Vec3::from_array(out.positions[v as usize]));
                            m.preview.push((w(x[0]), w(x[1])));
                        }
                    }
                }
                m.shown = if crate::i18n::zh() {
                    format!("切 {} 刀（滚动增加）", m.segments)
                } else {
                    format!("{} cut{} (scroll for more)", m.segments, if m.segments == 1 { "" } else { "s" })
                };
                None
            }
            Tool::Slide => {
                let (v, a, b) = m.rail.unwrap_or((m.center, m.center, None));
                let along = |t: Vec3| -> Option<f32> {
                    let (sv, st) = (project(rect, view, v)?, project(rect, view, t)?);
                    let dir = st - sv;
                    (dir.length_sq() > 1.0).then(|| (pointer - m.start).dot(dir) / dir.length_sq())
                };
                let mut f = if a == v { 0.0 } else { along(a).unwrap_or(0.0) };
                if f <= 0.0
                    && let Some(b) = b
                    && let Some(g) = along(b)
                    && g > 0.0
                {
                    f = -g;
                }
                let f = typed.unwrap_or(f).clamp(-1.0, 1.0);
                m.shown = format!("{:.0}%", f * 100.0);
                (f.abs() > 1e-4).then(|| ("mesh.edgeSlide".to_string(), sel_json(json!({"factor": f}))))
            }
            Tool::Knife => {
                m.shown = match m.knife.len() {
                    0 => "click where the cut starts".into(),
                    1 => "click the next point (inside a face to turn a corner)".into(),
                    n => crate::lf!(
                        "{} points · click more, Enter or click the last point again to cut",
                        "{} 个点 · 继续点，回车或再点最后一个点即可切开",
                        n
                    ),
                };
                None
            }
        };
        if m.tool != Tool::LoopCut && want != m.tried {
            if m.changed {
                self.run("history.discard", json!({}));
                m.changed = false;
            }
            m.tried = want.clone();
            m.error = None;
            if let Some((cmd, params)) = &want {
                match hub::dispatch(&self.hub, json!({"command": cmd, "params": params})) {
                    Ok(o) => {
                        m.changed = o.changed;
                        if let Ok(s) = serde_json::from_value::<Elements>(o.result["selection"].clone()) {
                            self.edit_modal = Some(m.clone());
                            self.edit_adopt(s);
                        }
                    }
                    Err(e) => {
                        m.error = Some(e.message);
                        self.set_edit_sel(m.sel.clone());
                    }
                }
            } else {
                self.set_edit_sel(m.sel.clone());
            }
        }
        if let Some(e) = &m.error {
            m.shown = format!("{} — {e}", m.shown);
        }
        self.edit_modal = Some(m);
    }

    /// Draw the cage, the selection, the hovered element and the tool hint.
    pub(super) fn edit_draw(&self, painter: &egui::Painter, cage: &Cage, rect: Rect, view: &View, hover: Option<Elem>) {
        let Some(c) = self.cache_for(cage) else { return };
        let Some(st) = self.hub.borrow().edit.clone() else { return };
        let fl = me::flush_with(&c.mesh, &c.topo, &st.selection).unwrap_or_default();
        let big = c.edges.len() > DRAW_LIMIT;
        // selected faces: a tint
        let mut tint = egui::epaint::Mesh::default();
        let mut add_face = |fi: usize, col: Color32| {
            let f = &c.mesh.faces[fi];
            let pts: Option<Vec<Pos2>> = f.iter().map(|&v| cage.pts[v as usize]).collect();
            let Some(pts) = pts else { return };
            let base = tint.vertices.len() as u32;
            for p in &pts {
                tint.colored_vertex(*p, col);
            }
            for i in 1..pts.len() as u32 - 1 {
                tint.add_triangle(base, base + i, base + i + 1);
            }
        };
        for &fi in &fl.faces {
            if cage.face_vis[fi as usize] {
                add_face(fi as usize, Color32::from_rgba_unmultiplied(255, 140, 30, 60));
            }
        }
        if let Some(Elem::Face(fi)) = hover
            && (fi as usize) < c.mesh.faces.len()
        {
            add_face(fi as usize, Color32::from_rgba_unmultiplied(255, 255, 255, 40));
        }
        painter.add(egui::Shape::mesh(tint));
        // edges
        let thin = if big { 0.0 } else { 1.0 };
        for (i, e) in c.edges.iter().enumerate() {
            let (Some(a), Some(b)) = (cage.pts[e[0] as usize], cage.pts[e[1] as usize]) else { continue };
            let selected = fl.edges.contains(&(e[0], e[1]));
            if !cage.edge_vis[i] && !selected {
                continue;
            }
            if selected {
                let col = if cage.edge_vis[i] { SEL } else { SEL.gamma_multiply(0.4) };
                painter.line_segment([a, b], (2.0, col));
            } else if thin > 0.0 {
                painter.line_segment([a, b], (thin, WIRE));
            }
        }
        if let Some(Elem::Edge(e)) = hover
            && let (Some(a), Some(b)) =
                (cage.pts.get(e[0] as usize).copied().flatten(), cage.pts.get(e[1] as usize).copied().flatten())
        {
            painter.line_segment([a, b], (3.0, Color32::WHITE));
        }
        // vertices (vertex mode), face dots (face mode)
        if st.mode == SelectMode::Vertex && !big {
            for (v, p) in cage.pts.iter().enumerate() {
                let Some(p) = p else { continue };
                if !cage.vert_vis[v] {
                    continue;
                }
                if fl.verts.contains(&(v as u32)) {
                    painter.circle_filled(*p, 3.5, SEL);
                } else {
                    painter.circle_filled(*p, 2.5, Color32::from_gray(20));
                }
            }
        }
        if let Some(Elem::Vert(v)) = hover
            && let Some(p) = cage.pts.get(v as usize).copied().flatten()
        {
            painter.circle_filled(p, 5.0, Color32::WHITE);
            painter.circle_stroke(p, 5.0, (1.0, Color32::from_gray(20)));
        }
        // loop cut preview
        if let Some(m) = &self.edit_modal {
            if let Some(p) = m.snap_to {
                painter.circle_stroke(p, 7.0, (2.0, Color32::from_rgb(255, 230, 60)));
            }
            if !m.knife.is_empty() {
                let yellow = Color32::from_rgb(255, 230, 60);
                let pts: Vec<Pos2> = m.knife.iter().filter_map(|p| project(rect, view, *p)).collect();
                for w in pts.windows(2) {
                    painter.line_segment([w[0], w[1]], (2.0, yellow));
                }
                for p in &pts {
                    painter.circle_filled(*p, 3.5, yellow);
                }
                if let Some(last) = pts.last() {
                    let p = painter.ctx().input(|i| i.pointer.hover_pos()).unwrap_or(m.pointer);
                    painter.add(egui::Shape::dashed_line(&[*last, p], (1.5, yellow), 6.0, 4.0));
                }
            }
            if self.edit_prop
                && matches!(m.tool, Tool::Grab | Tool::Rotate | Tool::Scale)
                && let Some(cs) = project(rect, view, m.center)
            {
                let right = view.view.inverse().x_axis.truncate().normalize_or_zero();
                if let Some(e) = project(rect, view, m.center + right * self.edit_amounts.prop_radius) {
                    painter.circle_stroke(cs, (e - cs).length(), (1.0, Color32::from_white_alpha(120)));
                }
            }
            for (a, b) in &m.preview {
                if let (Some(a), Some(b)) = (project(rect, view, *a), project(rect, view, *b)) {
                    painter.line_segment([a, b], (2.0, Color32::from_rgb(255, 230, 60)));
                }
            }
            if m.tool != Tool::LoopCut
                && let Some(cs) = project(rect, view, m.center)
                && matches!(m.tool, Tool::Scale | Tool::Rotate | Tool::Inset | Tool::Bevel)
                && let Some(p) = painter.ctx().input(|i| i.pointer.hover_pos())
            {
                painter.add(egui::Shape::dashed_line(&[cs, p], (1.0, Color32::from_white_alpha(140)), 5.0, 4.0));
            }
            if let (Some(i), Some(cs)) = (m.axis, project(rect, view, m.center)) {
                let a = [Vec3::X, Vec3::Y, Vec3::Z][i];
                let colors =
                    [Color32::from_rgb(235, 80, 80), Color32::from_rgb(110, 210, 90), Color32::from_rgb(80, 140, 255)];
                if let (Some(p), Some(q)) =
                    (project(rect, view, m.center - a * 100.0), project(rect, view, m.center + a * 100.0))
                {
                    painter.line_segment([p, q], (1.0, colors[i]));
                } else {
                    painter.circle_stroke(cs, 4.0, (1.0, colors[i]));
                }
            }
        } else if let Some((cp, arrows)) = cage.gizmo {
            // move arrows on the selection
            let colors =
                [Color32::from_rgb(235, 80, 80), Color32::from_rgb(110, 210, 90), Color32::from_rgb(80, 140, 255)];
            for (i, a) in arrows.iter().enumerate() {
                if let Some(a) = a {
                    let lit = self.edit_hot_axis == Some(i);
                    let col = if lit { Color32::WHITE } else { colors[i] };
                    painter.line_segment([cp, *a], (if lit { 4.0 } else { 2.5 }, col));
                    painter.circle_filled(*a, 5.0, col);
                }
            }
            painter.circle_stroke(cp, 4.0, (1.5, Color32::WHITE));
        }
        // header: mode, counts, hint
        let counts = format!(
            "{} / {} verts · {} / {} edges · {} / {} faces",
            fl.verts.len(),
            c.mesh.positions.len(),
            fl.edges.len(),
            c.edges.len(),
            fl.faces.len(),
            c.mesh.faces.len()
        );
        let hint = match &self.edit_modal {
            Some(m) => {
                let axis = m.axis.map(|i| format!(" along {}", ["X", "Y", "Z"][i])).unwrap_or_default();
                let typed = if m.typed.is_empty() { String::new() } else { format!(" · typed {}", m.typed) };
                let how = if m.release_confirms { "release to apply" } else { "click or Enter to apply" };
                let extra = match m.tool {
                    Tool::Grab | Tool::Rotate | Tool::Scale if self.edit_prop => {
                        " · X Y Z lock an axis · type a number · scroll: proportional radius"
                    }
                    Tool::Grab => " · X Y Z lock an axis · type a number · hold Cmd/Ctrl to snap to a vertex",
                    Tool::Rotate | Tool::Scale => " · X Y Z lock an axis · type a number",
                    Tool::Extrude | Tool::Inset | Tool::Bevel | Tool::Slide => " · type a number",
                    Tool::LoopCut | Tool::Knife => "",
                };
                format!("{}{axis}: {}{typed} · {how}, Esc / right-click to cancel{extra}", m.tool.name(), m.shown)
            }
            None => counts,
        };
        let font = egui::FontId::proportional(12.0);
        let g = painter.layout_no_wrap(hint, font, Color32::WHITE);
        let r =
            Rect::from_center_size(egui::pos2(rect.center().x, rect.bottom() - 64.0), g.size() + Vec2::new(16.0, 8.0));
        painter.rect_filled(r, 5.0, Color32::from_rgba_unmultiplied(20, 22, 28, 210));
        painter.galley(r.min + Vec2::new(8.0, 4.0), g, Color32::WHITE);
    }

    /// The floating bar in edit mode: select mode, x-ray and Done.
    pub(super) fn edit_bar(&mut self, ui: &egui::Ui, full: Rect) {
        let Some(st) = self.hub.borrow().edit.clone() else { return };
        let mut mode = st.mode;
        let mut leave = false;
        egui::Area::new(egui::Id::unique("edit_bar"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(full.center_top() + Vec2::new(0.0, 48.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tr("Edit mode")).strong().color(SEL));
                        ui.separator();
                        for (m, label, tip) in [
                            (SelectMode::Vertex, "Vertex", "Pick vertices (1)"),
                            (SelectMode::Edge, "Edge", "Pick edges (2)"),
                            (SelectMode::Face, "Face", "Pick faces (3)"),
                        ] {
                            ui.selectable_value(&mut mode, m, tr(label)).on_hover_text(tr(tip));
                        }
                        ui.separator();
                        ui.toggle_value(&mut self.edit_xray, tr("X-ray"))
                            .on_hover_text(tr("See and pick through the mesh (Alt+Z)"));
                        ui.toggle_value(&mut self.edit_prop, tr("◎ Proportional")).on_hover_text(tr("Moving, rotating or scaling also pulls nearby vertices, fading out (O; scroll while moving to resize)"),
                        );
                        if ui.button(tr("Done")).on_hover_text(tr("Back to objects (Tab)")).clicked() {
                            leave = true;
                        }
                    });
                });
            });
        if mode != st.mode {
            self.set_edit_mode(mode);
        }
        if leave {
            self.leave_edit();
        }
    }

    /// Keys in edit mode. Returns true when edit mode handled the keyboard this frame.
    pub(super) fn edit_shortcuts(&mut self, ui: &egui::Ui) -> bool {
        let tab = ui.input(|i| i.key_pressed(egui::Key::Tab) && !i.modifiers.any());
        if tab {
            // Tab also moves egui's keyboard focus to a button (at the end of this frame), which
            // would swallow every later key: take it back over the next frames
            self.edit_unfocus = 3;
        }
        let editing = self.edit_state().is_some();
        if !editing {
            if tab {
                self.enter_edit();
                return true;
            }
            return false;
        }
        if tab {
            self.leave_edit();
            return true;
        }
        let pointer = ui.input(|i| i.pointer.hover_pos()).or(self.last_view.map(|v| v.0.center()));
        let pointer = pointer.unwrap_or_default();
        // keys while a tool runs
        if self.edit_modal.is_some() {
            let gg = ui.input(|i| i.key_pressed(egui::Key::G) && !i.modifiers.any());
            if gg && self.edit_modal.as_ref().is_some_and(|m| m.tool == Tool::Grab) {
                // G twice: slide the edges along their neighbours instead
                self.edit_cancel();
                self.edit_start(Tool::Slide, pointer, None, false);
                return true;
            }
            let (esc, enter, axis, typed, back, undo) = ui.input(|i| {
                let axis = [egui::Key::X, egui::Key::Y, egui::Key::Z].iter().position(|k| i.key_pressed(*k));
                let mut typed = String::new();
                for e in &i.events {
                    if let egui::Event::Text(t) = e {
                        typed.extend(t.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-'));
                    }
                }
                (
                    i.key_pressed(egui::Key::Escape),
                    i.key_pressed(egui::Key::Enter),
                    axis,
                    typed,
                    i.key_pressed(egui::Key::Backspace),
                    i.modifiers.command && i.key_pressed(egui::Key::Z),
                )
            });
            if esc || undo {
                self.edit_cancel();
                return true;
            }
            if enter {
                if let Some(m) = self.edit_modal.take_if(|m| m.tool == Tool::Knife) {
                    self.edit_knife(m.knife);
                } else {
                    self.edit_confirm();
                }
                return true;
            }
            if let Some(m) = self.edit_modal.as_mut() {
                if let Some(a) = axis {
                    m.axis = if m.axis == Some(a) { None } else { Some(a) };
                }
                if !typed.is_empty() || back {
                    if back {
                        m.typed.pop();
                    }
                    m.typed.push_str(&typed);
                }
            }
            return true;
        }
        // modifiers as they were when each key went down (a quick Ctrl+B may be released before
        // the frame that sees it)
        let presses: Vec<(egui::Key, egui::Modifiers)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        let press = |key: egui::Key| presses.iter().find(|(k, _)| *k == key).map(|(_, m)| *m);
        let k = |key: egui::Key| press(key).is_some();
        let kp = |key: egui::Key| press(key).is_some_and(|m| !m.command && !m.ctrl && !m.alt);
        let kc = |key: egui::Key| press(key).is_some_and(|m| m.command || m.ctrl);
        let shift = ui.input(|i| i.modifiers.shift) || presses.iter().any(|(_, m)| m.shift);
        // history and file keys work as usual
        if kc(egui::Key::Z) {
            self.run(if shift { "history.redo" } else { "history.undo" }, json!({}));
            return true;
        }
        if kc(egui::Key::Y) {
            self.run("history.redo", json!({}));
            return true;
        }
        if kc(egui::Key::S) {
            self.save_scene();
            return true;
        }
        if k(egui::Key::Space) {
            self.playing = !self.playing;
        }
        if k(egui::Key::Num1) {
            self.set_edit_mode(SelectMode::Vertex);
        }
        if k(egui::Key::Num2) {
            self.set_edit_mode(SelectMode::Edge);
        }
        if k(egui::Key::Num3) {
            self.set_edit_mode(SelectMode::Face);
        }
        if let Some(m) = press(egui::Key::A) {
            self.edit_select_all(!m.alt);
        }
        if press(egui::Key::Z).is_some_and(|m| m.alt) {
            self.edit_xray = !self.edit_xray;
        }
        if kp(egui::Key::G) {
            self.edit_start(Tool::Grab, pointer, None, false);
        }
        if kp(egui::Key::R) {
            self.edit_start(Tool::Rotate, pointer, None, false);
        }
        if kp(egui::Key::S) {
            self.edit_start(Tool::Scale, pointer, None, false);
        }
        if kp(egui::Key::E) {
            self.edit_start(Tool::Extrude, pointer, None, false);
        }
        if kp(egui::Key::I) {
            self.edit_start(Tool::Inset, pointer, None, false);
        }
        if kc(egui::Key::B) {
            self.edit_start(Tool::Bevel, pointer, None, false);
        }
        // (Cmd+R reloads the page in a browser: Ctrl+R on a Mac, or C anywhere)
        if press(egui::Key::R).is_some_and(|m| m.ctrl) || kp(egui::Key::C) {
            self.edit_start(Tool::LoopCut, pointer, None, false);
        }
        if kp(egui::Key::O) {
            self.edit_prop = !self.edit_prop;
            self.toast(if self.edit_prop {
                "Proportional editing on (scroll while moving to resize)"
            } else {
                "Proportional editing off"
            });
        }
        if kp(egui::Key::K) {
            self.edit_start(Tool::Knife, pointer, None, false);
        }
        if kp(egui::Key::F) {
            self.edit_apply("mesh.fill", json!({}));
        }
        if kp(egui::Key::J) {
            self.edit_apply("mesh.connect", json!({}));
        }
        if kp(egui::Key::M) {
            self.edit_apply("mesh.merge", json!({}));
        }
        if let Some(m) = press(egui::Key::L) {
            if m.command || m.ctrl {
                self.edit_select_how("linked", None, false);
            } else if let Some((rect, view)) = self.last_view {
                let scene = self.scene();
                if let Some(st) = self.edit_state()
                    && let Some(cage) = self.edit_cage(&scene, &st.id, self.frame(), rect, &view, self.edit_xray)
                {
                    self.edit_select_linked_at(&cage, pointer, ray_at(rect, &view, pointer), shift);
                }
            }
        }
        if kc(egui::Key::Plus) || kc(egui::Key::Equals) {
            self.edit_select_how("more", None, false);
        }
        if k(egui::Key::Delete) || k(egui::Key::Backspace) || (kp(egui::Key::X)) {
            self.edit_apply("mesh.deleteElements", json!({}));
        }
        if k(egui::Key::Escape) {
            if self.edit_state().is_some_and(|s| !s.selection.is_empty()) {
                self.set_edit_sel(Elements::default());
            } else {
                self.leave_edit();
            }
        }
        true
    }

    /// The Edit section of the properties panel.
    pub(super) fn edit_panel(&mut self, ui: &mut egui::Ui, id: &str) {
        let editing = self.edit_state().filter(|s| s.id == id);
        let Some(st) = editing else {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(tr("✏ Edit mesh"))
                    .on_hover_text(tr("Edit vertices, edges and faces: extrude, bevel, loop cut… (Tab)"))
                    .clicked()
                {
                    self.enter_edit();
                }
            });
            return;
        };
        ui.horizontal_wrapped(|ui| {
            let mut mode = st.mode;
            for (m, label) in [(SelectMode::Vertex, "Vertex"), (SelectMode::Edge, "Edge"), (SelectMode::Face, "Face")] {
                ui.selectable_value(&mut mode, m, tr(label));
            }
            if mode != st.mode {
                self.set_edit_mode(mode);
            }
            if ui.button(tr("Done")).on_hover_text(tr("Tab")).clicked() {
                self.leave_edit();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Select"));
            if ui.small_button(tr("All")).on_hover_text(tr("A")).clicked() {
                self.edit_select_all(true);
            }
            if ui.small_button(tr("None")).on_hover_text(tr("Alt+A")).clicked() {
                self.edit_select_all(false);
            }
            if ui
                .small_button(tr("Linked"))
                .on_hover_text(tr("Everything joined to the selection (Ctrl+L; L under the pointer)"))
                .clicked()
            {
                self.edit_select_how("linked", None, false);
            }
            if ui.small_button(tr("More")).on_hover_text(tr("Grow by one step (Ctrl +)")).clicked() {
                self.edit_select_how("more", None, false);
            }
            let first_edge = self.edit_first_edge();
            if ui
                .add_enabled(first_edge.is_some(), egui::Button::new(tr("Loop")).small())
                .on_hover_text(tr("Edge loop through the first selected edge (Alt+click)"))
                .clicked()
                && let Some(e) = first_edge
            {
                self.edit_select_how_edge("loop", e);
            }
            if ui
                .add_enabled(first_edge.is_some(), egui::Button::new(tr("Ring")).small())
                .on_hover_text(tr("Edge ring across the first selected edge"))
                .clicked()
                && let Some(e) = first_edge
            {
                self.edit_select_how_edge("ring", e);
            }
        });
        ui.add_space(4.0);
        let center = self.last_view.map(|v| v.0.center()).unwrap_or_default();
        let a = &mut self.edit_amounts;
        let mut run: Option<(&str, Value)> = None;
        let mut start: Option<Tool> = None;
        egui::Grid::new("edit_tools").num_columns(3).spacing([6.0, 4.0]).show(ui, |ui| {
            if ui.button(tr("Extrude")).on_hover_text(tr("E: drag it out · or apply the distance here")).clicked() {
                run = Some(("mesh.extrude", json!({"distance": a.extrude})));
            }
            ui.add(egui::DragValue::new(&mut a.extrude).speed(0.01).range(-100.0..=100.0).suffix(" m"));
            if ui.small_button(tr("drag")).on_hover_text(tr("Extrude and follow the pointer (E)")).clicked() {
                start = Some(Tool::Extrude);
            }
            ui.end_row();
            if ui.button(tr("Inset")).on_hover_text(tr("I: a smaller face inside")).clicked() {
                run = Some(("mesh.inset", json!({"thickness": a.inset, "depth": a.depth})));
            }
            ui.add(egui::DragValue::new(&mut a.inset).speed(0.005).range(0.0..=100.0).suffix(" m"));
            ui.add(egui::DragValue::new(&mut a.depth).speed(0.005).range(-100.0..=100.0).prefix("depth ").suffix(" m"));
            ui.end_row();
            if ui.button(tr("Bevel")).on_hover_text(tr("Ctrl+B: round off edges (faces: their edges)")).clicked() {
                run = Some(("mesh.bevel", json!({"width": a.bevel, "segments": a.segments})));
            }
            ui.add(egui::DragValue::new(&mut a.bevel).speed(0.005).range(0.0001..=100.0).suffix(" m"));
            ui.add(egui::DragValue::new(&mut a.segments).range(1..=16).suffix(" seg"));
            ui.end_row();
            if ui
                .button(tr("Loop cut"))
                .on_hover_text(tr("Ctrl+R or C: hover an edge, scroll for more cuts, click to cut"))
                .clicked()
            {
                start = Some(Tool::LoopCut);
            }
            ui.add(egui::DragValue::new(&mut a.cuts).range(1..=32).suffix(" cuts"));
            ui.label(tr(""));
            ui.end_row();
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(tr("Edge slide"))
                .on_hover_text(tr("G G: slide the selected edges along their neighbours"))
                .clicked()
            {
                start = Some(Tool::Slide);
            }
            if ui
                .button(tr("Knife"))
                .on_hover_text(tr(
                    "K: click two points to cut along the line (the faces it crosses, or the selected faces)",
                ))
                .clicked()
            {
                start = Some(Tool::Knife);
            }
            if ui
                .button(tr("Bridge"))
                .on_hover_text(tr(
                    "Join two border loops with faces; with two faces selected, makes a tunnel between them",
                ))
                .clicked()
            {
                run = Some(("mesh.bridge", json!({})));
            }
        });
        ui.horizontal_wrapped(|ui| {
            let a = &mut self.edit_amounts;
            if ui
                .button(tr("Spin"))
                .on_hover_text(tr("Extrude the selected edges or faces round the object's origin (360° joins up)"))
                .clicked()
            {
                let mut axis = [0.0; 3];
                axis[a.spin_axis.min(2)] = 1.0;
                run = Some((
                    "mesh.spin",
                    json!({"angle": a.spin_angle, "steps": a.spin_steps, "axis": axis, "center": [0, 0, 0]}),
                ));
            }
            ui.add(egui::DragValue::new(&mut a.spin_angle).speed(1.0).range(-3600.0..=3600.0).suffix("°"));
            ui.add(egui::DragValue::new(&mut a.spin_steps).range(1..=256).suffix(" steps"));
            for (i, n) in ["X", "Y", "Z"].iter().enumerate() {
                ui.selectable_value(&mut a.spin_axis, i, tr(*n));
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.edit_prop, tr("Proportional"))
                .on_hover_text(tr("O: G / R / S also pull nearby vertices"));
            let a = &mut self.edit_amounts;
            ui.add(
                egui::DragValue::new(&mut a.prop_radius).speed(0.01).range(0.001..=1000.0).prefix("r ").suffix(" m"),
            );
            egui::ComboBox::from_id_salt("falloff").selected_text(format!("{:?}", a.falloff)).show_ui(ui, |ui| {
                for f in [
                    me::Falloff::Smooth,
                    me::Falloff::Linear,
                    me::Falloff::Sharp,
                    me::Falloff::Sphere,
                    me::Falloff::Constant,
                ] {
                    ui.selectable_value(&mut a.falloff, f, format!("{f:?}"));
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            for (label, cmd, tip) in [
                ("Subdivide", "mesh.subdivideFaces", "Split the selected faces into smaller quads"),
                ("Connect", "mesh.connect", "J: a new edge between two selected vertices of a face"),
                ("Merge", "mesh.merge", "M: merge the selected vertices at their centre"),
                ("Fill", "mesh.fill", "F: make a face from a ring of border edges or 3+ vertices"),
                ("Flip", "mesh.flipNormals", "Turn the selected faces inside out"),
                ("Delete", "mesh.deleteElements", "X / Delete: remove the selection"),
            ] {
                if ui.button(tr(label)).on_hover_text(tr(tip)).clicked() {
                    run = Some((cmd, json!({})));
                }
            }
            if ui
                .button(tr("Clean up"))
                .on_hover_text(tr("Merge vertices closer than 0.1 mm (whole mesh): fixes split imported meshes"))
                .clicked()
                && let Some(st) = self.edit_state()
            {
                match hub::dispatch(&self.hub, json!({"command": "mesh.mergeByDistance", "params": {"id": st.id}})) {
                    Ok(o) => self.toast(crate::lf!("Merged {} vertices", "合并了 {} 个顶点", o.result["removed"])),
                    Err(e) => self.toast(tr(e.message)),
                }
            }
        });
        if let Some((cmd, extra)) = run {
            self.edit_apply(cmd, extra);
        }
        if let Some(t) = start {
            self.edit_start(t, center, None, false);
        }
        ui.add_space(2.0);
        ui.weak(tr("Click to pick · Shift+click add · Alt+click loop · Cmd/Ctrl+drag box · drag a selection to move it · G R S move, rotate, scale (X Y Z lock, type a number) · G G edge slide · K knife · double-click or L linked"),
        );
    }

    /// Where a knife click lands: on the surface under the pointer, else at the depth of the
    /// previous point (or the mesh's centre) facing the view.
    fn edit_knife_point(&self, pointer: Pos2, prev: Option<Vec3>) -> Option<Vec3> {
        let st = self.hub.borrow().edit.clone()?;
        let (rect, view) = self.last_view?;
        let scene = self.scene();
        let cage = self.edit_cage(&scene, &st.id, self.frame(), rect, &view, self.edit_xray)?;
        let (o, d) = ray_at(rect, &view, pointer);
        if let Some((_, t)) = self.edit_ray_face(&cage, o, d, false) {
            return Some(o + d * t);
        }
        let anchor = prev.or(cage.center).unwrap_or_else(|| cage.world.w_axis.truncate());
        let n = (view.view.inverse().w_axis.truncate() - anchor).normalize_or_zero();
        plane_hit(o, d, anchor, n)
    }

    /// Cut along the knife path: on the selected faces, else on the faces you can see.
    fn edit_knife(&mut self, path: Vec<Vec3>) {
        if path.len() < 2 {
            self.toast(tr("Knife: click at least two points"));
            return;
        }
        let Some(st) = self.edit_state() else { return };
        let Some((rect, view)) = self.last_view else { return };
        let scene = self.scene();
        let Some(cage) = self.edit_cage(&scene, &st.id, self.frame(), rect, &view, self.edit_xray) else { return };
        let inv = cage.world.inverse();
        let local: Vec<[f32; 3]> = path.iter().map(|p| inv.transform_point3(*p).to_array()).collect();
        // perspective views share an eye; orthographic ones share a direction
        let (_, d0) = ray_at(rect, &view, rect.left_top());
        let (_, d1) = ray_at(rect, &view, rect.right_bottom());
        let mut p = json!({ "path": local });
        if d0.dot(d1) > 0.99999 {
            p["direction"] = json!(inv.transform_vector3(d0).to_array());
        } else {
            p["eye"] = json!(inv.transform_point3(view.view.inverse().w_axis.truncate()).to_array());
        }
        let faces: Vec<u32> = {
            let Some(c) = self.cache_for(&cage) else { return };
            let picked = me::flush_with(&c.mesh, &c.topo, &st.selection).map(|f| f.faces).unwrap_or_default();
            if !picked.is_empty() {
                picked.into_iter().collect()
            } else {
                (0..c.mesh.faces.len() as u32)
                    .filter(|&f| self.edit_xray || cage.face_vis.get(f as usize).copied().unwrap_or(false))
                    .collect()
            }
        };
        p["faces"] = json!(faces);
        self.edit_apply("mesh.knife", p);
    }

    /// Combine this mesh with another one: union, difference or intersect (`mesh.boolean`).
    pub(super) fn boolean_panel(&mut self, ui: &mut egui::Ui, id: &str) {
        use shotstage_core::csg::BoolOp;
        let scene = self.scene();
        let others: Vec<(String, String)> = scene
            .entities
            .iter()
            .filter(|e| e.id != id && matches!(e.kind, shotstage_core::EntityKind::Mesh { .. }))
            .map(|e| (e.id.clone(), e.name.clone()))
            .collect();
        if others.is_empty() {
            ui.weak(tr("Add a second mesh to cut with or merge in."));
            return;
        }
        if self.bool_with.as_ref().is_none_or(|w| !others.iter().any(|(i, _)| i == w)) {
            self.bool_with = Some(others[0].0.clone());
        }
        let with = self.bool_with.clone().unwrap_or_default();
        let label = others.iter().find(|(i, _)| *i == with).map(|(_, n)| n.clone()).unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("With"));
            egui::ComboBox::from_id_salt("bool_with").selected_text(label).show_ui(ui, |ui| {
                for (i, n) in &others {
                    ui.selectable_value(&mut self.bool_with, Some(i.clone()), n);
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            for (op, name, tip) in [
                (BoolOp::Difference, "Difference", "Carve the other mesh out of this one"),
                (BoolOp::Union, "Union", "Merge both into one solid"),
                (BoolOp::Intersect, "Intersect", "Keep only where they overlap"),
            ] {
                ui.selectable_value(&mut self.bool_op, op, tr(name)).on_hover_text(tr(tip));
            }
        });
        if ui
            .button(tr("Apply"))
            .on_hover_text(tr("Changes this mesh and hides the other one. Both should be closed solids."))
            .clicked()
        {
            let op = serde_json::to_value(self.bool_op).unwrap_or(json!("difference"));
            let req = json!({"command": "mesh.boolean", "params": {"id": id, "with": with, "operation": op}});
            match hub::dispatch(&self.hub, req) {
                Ok(o) => self.toast(crate::lf!("Boolean done: {} faces", "布尔运算完成：{} 个面", o.result["faces"])),
                Err(e) => self.toast(tr(e.message)),
            }
        }
    }

    fn edit_first_edge(&self) -> Option<[u32; 2]> {
        let st = self.hub.borrow().edit.clone()?;
        let c = self.edit_cache.as_ref()?;
        let f = me::flush(&c.mesh, &st.selection).ok()?;
        f.edges.iter().next().map(|&(a, b)| [a, b])
    }

    fn edit_select_how_edge(&mut self, how: &str, edge: [u32; 2]) {
        let Some(st) = self.edit_state() else { return };
        let Some(r) = self.run("mesh.select", json!({"id": st.id, "how": how, "edge": edge})) else { return };
        let Ok(found) = serde_json::from_value::<Elements>(r["selection"].clone()) else { return };
        let Some(c) = self.edit_cache.as_ref() else { return };
        let mode = if st.mode == SelectMode::Vertex { SelectMode::Vertex } else { SelectMode::Edge };
        let s = convert(&c.mesh, &found, mode);
        if let Some(e) = self.hub.borrow_mut().edit.as_mut() {
            e.mode = mode;
            e.selection = s;
        }
    }
}

impl EditorApp {
    /// A drag starting in the view in edit mode: an arrow of the gizmo moves along its axis, an
    /// element moves the selection (picking it first), Cmd/Ctrl box-selects, empty space orbits.
    pub(super) fn edit_drag_start(
        &mut self,
        cage: &Cage,
        start: Pos2,
        ray: (Vec3, Vec3),
        modifiers: egui::Modifiers,
        can_navigate: bool,
    ) -> Gesture {
        if self.edit_modal.is_some() {
            return Gesture::Cancelled;
        }
        let Some(st) = self.edit_state() else { return Gesture::Idle };
        if let Some((cp, arrows)) = cage.gizmo {
            for (i, a) in arrows.iter().enumerate() {
                if let Some(a) = a
                    && seg_dist(start, cp, *a) < 7.0
                    && (start - cp).length() > 6.0
                {
                    self.edit_start(Tool::Grab, start, Some(i), true);
                    return Gesture::EditTool;
                }
            }
        }
        if modifiers.command {
            return Gesture::Marquee { start };
        }
        if let Some(e) = self.edit_pick(cage, st.mode, start, ray, self.edit_xray) {
            if !Self::edit_contains(&st.selection, e) {
                let mut sel = if modifiers.shift { st.selection.clone() } else { Elements::default() };
                Self::edit_toggle(&mut sel, e, true);
                self.set_edit_sel(sel);
            }
            self.edit_start(Tool::Grab, start, None, true);
            return Gesture::EditTool;
        }
        if can_navigate { Gesture::Navigate } else { Gesture::Idle }
    }

    /// Which gizmo arrow is under the pointer.
    pub(super) fn edit_axis_at(cage: &Cage, p: Pos2) -> Option<usize> {
        let (cp, arrows) = cage.gizmo?;
        arrows.iter().position(|a| a.is_some_and(|a| seg_dist(p, cp, a) < 7.0 && (p - cp).length() > 6.0))
    }
}
