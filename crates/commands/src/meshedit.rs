//! Edit-mode commands: Blender-like tools on a mesh's vertices, edges and faces.
//!
//! Elements are addressed by index in the mesh's base geometry (before modifiers, which stay
//! live on top): `vertices: [3, 7]`, `edges: [[3, 7]]`, `faces: [0, 4]`. `mesh.editData`
//! returns the geometry with its edge list. Every tool returns `selection`: what is selected
//! afterwards (Blender keeps extruded faces selected, a loop cut selects its new loop...), in
//! the new indices, so a script can chain tools without tracking index changes:
//!
//! ```json
//! {"batch": [
//!   {"command": "mesh.extrude", "params": {"id": "box_1", "faces": [4], "distance": 0.3}},
//!   {"command": "mesh.inset", "params": {"id": "box_1", "faces": [4], "thickness": 0.05}}
//! ]}
//! ```
//!
//! Primitives turn into editable geometry on their first edit (their size sliders go away,
//! modifiers stay). Skinned (rigged) meshes cannot be edited here.

use glam::Vec3;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::csg::{self, BoolOp};
use shotstage_core::meshedit::{self as me, ClearSide, DeleteMode, Elements, Falloff, KnifeView, MergeAt, Offset};
use shotstage_core::{EntityKind, Mesh, MeshSource, Scene, mesh::evaluate_mesh};

use crate::builtin::finite3;
use crate::{CmdResult, Command, CommandError, command};

/// Largest mesh the edit tools accept, in faces.
const MAX_EDIT_FACES: usize = 500_000;

/// The editable base mesh of an entity (a primitive is built).
pub fn edit_mesh(scene: &Scene, id: &str) -> CmdResult<Mesh> {
    match &scene.get(id)?.kind {
        EntityKind::Mesh { source, .. } => match source {
            MeshSource::Primitive { primitive } => Ok(primitive.build()?),
            MeshSource::Data { mesh } => Ok(mesh.clone()),
            MeshSource::Skinned { .. } | MeshSource::Human { .. } => Err(CommandError::new(
                "failed",
                format!("`{id}` is a rigged (skinned) mesh: pose it instead, or apply its pose first"),
            )),
        },
        _ => Err(CommandError::new("failed", format!("entity `{id}` is not a mesh"))),
    }
}

fn store(scene: &mut Scene, id: &str, mesh: Mesh) -> CmdResult<()> {
    mesh.validate()?;
    if mesh.faces.len() > MAX_EDIT_FACES {
        return Err(CommandError::invalid(format!(
            "result would have {} faces (limit {MAX_EDIT_FACES})",
            mesh.faces.len()
        )));
    }
    match &mut scene.get_mut(id)?.kind {
        EntityKind::Mesh { source, .. } => {
            *source = MeshSource::Data { mesh };
            Ok(())
        }
        _ => Err(CommandError::new("failed", format!("entity `{id}` is not a mesh"))),
    }
}

fn result(mesh: &Mesh, sel: &Elements) -> Value {
    json!({ "selection": sel, "vertices": mesh.positions.len(), "faces": mesh.faces.len() })
}

fn edit(
    scene: &mut Scene,
    id: &str,
    f: impl FnOnce(&Mesh) -> shotstage_core::Result<(Mesh, Elements)>,
) -> CmdResult<Value> {
    let m = edit_mesh(scene, id)?;
    if m.faces.len() > MAX_EDIT_FACES {
        return Err(CommandError::invalid(format!(
            "mesh has {} faces: too big to edit (limit {MAX_EDIT_FACES})",
            m.faces.len()
        )));
    }
    let (out, sel) = f(&m)?;
    store(scene, id, out.clone())?;
    Ok(result(&out, &sel))
}

/// The selection part of a tool's parameters.
#[derive(Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
struct Sel {
    /// Vertex indices.
    #[serde(default)]
    vertices: Vec<u32>,
    /// Edges as vertex pairs, e.g. `[[0, 1], [1, 5]]`.
    #[serde(default)]
    edges: Vec<[u32; 2]>,
    /// Face indices.
    #[serde(default)]
    faces: Vec<u32>,
}

impl Sel {
    fn elements(&self) -> Elements {
        Elements { vertices: self.vertices.clone(), edges: self.edges.clone(), faces: self.faces.clone() }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct IdOnly {
    id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TransformParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// Move by this many metres (mesh space).
    translate: Option<[f32; 3]>,
    /// Turn by `[x, y, z]` degrees about the pivot.
    rotate: Option<[f32; 3]>,
    /// Scale about the pivot, per axis.
    scale: Option<[f32; 3]>,
    /// Pivot in mesh space. Default: centre of the selected vertices.
    pivot: Option<[f32; 3]>,
    /// Proportional editing: unselected vertices this close (metres) to the selection follow
    /// it, fading out (Blender's O).
    proportional: Option<Proportional>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Proportional {
    /// How far the effect reaches, metres (mesh space).
    radius: f32,
    /// `smooth` (default), `linear`, `sharp`, `sphere` or `constant`.
    falloff: Option<Falloff>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct KnifeParams {
    id: String,
    /// Points on the surface to cut through, in order (mesh space). Each step cuts the faces
    /// between its two points; a point inside a face becomes a vertex, so the cut can turn.
    path: Vec<[f32; 3]>,
    /// Where the cut is seen from (mesh space); the cut runs along the view rays like
    /// drawing on screen. Give this or `direction`.
    eye: Option<[f32; 3]>,
    /// View direction for an orthographic cut (mesh space).
    direction: Option<[f32; 3]>,
    /// Only cut these faces (the editor passes the ones you can see). Default: all faces, so
    /// the cut goes through to the back.
    faces: Option<Vec<u32>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SpinParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// Degrees to turn in total. Default 360 (joins up).
    angle: Option<f32>,
    /// How many copies along the turn. Default 12.
    steps: Option<u32>,
    /// Axis to turn about (mesh space). Default `[0, 1, 0]`.
    axis: Option<[f32; 3]>,
    /// A point on the axis (mesh space). Default: the mesh origin.
    center: Option<[f32; 3]>,
}

fn knife(s: &mut Scene, p: KnifeParams) -> CmdResult<Value> {
    let path: Vec<Vec3> = p.path.iter().map(|v| finite3(*v, "path")).collect::<CmdResult<_>>()?;
    let view = match (p.eye, p.direction) {
        (Some(e), None) => KnifeView::Eye(finite3(e, "eye")?),
        (None, Some(d)) => KnifeView::Direction(finite3(d, "direction")?),
        _ => return Err(CommandError::invalid("give either `eye` or `direction`")),
    };
    let faces = p.faces.unwrap_or_default();
    edit(s, &p.id, |m| me::knife(m, &faces, &path, view))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExtrudeParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// How far along the faces' normal (edges: outward along the surface). Default 0, so the
    /// new geometry can be moved afterwards, like Blender's E.
    distance: Option<f32>,
    /// Extra move of the new geometry, metres (mesh space).
    offset: Option<[f32; 3]>,
    /// Faces: each face extrudes along its own normal instead of as one region.
    individual: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InsetParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// How far in from the border, metres.
    thickness: f32,
    /// Raise (or, negative, sink) the inner faces along the normal, metres.
    depth: Option<f32>,
    /// Inset each face on its own instead of the selection as one region.
    individual: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BevelParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// How far the bevel cuts in from the edge, metres.
    width: f32,
    /// 1 = flat chamfer (default); more rounds it.
    segments: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LoopCutParams {
    id: String,
    /// Any edge of the quad ring to cut across, as a vertex pair.
    edge: [u32; 2],
    /// Number of parallel cuts. Default 1.
    cuts: Option<u32>,
    /// With one cut: where along the ring edges, 0..1 from `edge[0]`'s side. Default 0.5.
    factor: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SelParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DeleteParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// What to delete. Default: faces if faces are given, else edges, else vertices.
    mode: Option<DeleteMode>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MergeParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// Where the merged vertex goes. Default `center`.
    at: Option<MergeAt>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MergeDistanceParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// Vertices closer than this merge, metres. Default 0.0001.
    distance: Option<f32>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
enum SelectHow {
    /// Edge loop through `edge` (Blender Alt+click).
    Loop,
    /// Edge ring across `edge` (Ctrl+Alt+click).
    Ring,
    /// The quads of the ring across `edge` (face loop).
    FaceLoop,
    /// Everything connected to the selection (L / Ctrl+L).
    Linked,
    /// Grow by one step (Ctrl+Numpad+).
    More,
    /// The whole mesh.
    All,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SelectParams {
    id: String,
    how: SelectHow,
    /// The edge for `loop`, `ring` and `faceLoop`.
    edge: Option<[u32; 2]>,
    #[serde(flatten)]
    sel: Sel,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EditDataParams {
    id: String,
    /// Also return each face's centre and normal. Default false.
    face_info: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SlideParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// -1..1: positive slides toward side A (left of the loop as it runs), negative toward
    /// side B; 1 reaches the neighbouring loop.
    factor: f32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BisectParams {
    id: String,
    #[serde(flatten)]
    sel: Sel,
    /// A point on the cutting plane, mesh space.
    point: [f32; 3],
    /// The plane's normal, mesh space.
    normal: [f32; 3],
    /// Remove one side afterwards: `front` (where the normal points) or `back`. Default `none`.
    clear: Option<ClearSide>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BooleanParams {
    /// The mesh that changes.
    id: String,
    /// The other mesh (with its modifiers, where it is placed in the scene).
    with: String,
    /// `union`, `difference` (default: carve `with` out of `id`) or `intersect`.
    operation: Option<BoolOp>,
    /// Hide the other mesh afterwards, like applying Blender's boolean modifier. Default true.
    hide_other: Option<bool>,
}

fn boolean(s: &mut Scene, p: BooleanParams) -> CmdResult<Value> {
    if p.id == p.with {
        return Err(CommandError::invalid("pick two different meshes"));
    }
    let a = edit_mesh(s, &p.id)?;
    let b = match &s.get(&p.with)?.kind {
        EntityKind::Mesh { source, modifiers, .. } => evaluate_mesh(source, modifiers)?,
        _ => return Err(CommandError::new("failed", format!("entity `{}` is not a mesh", p.with))),
    };
    let b_to_a = s.world_matrix(&p.id)?.inverse() * s.world_matrix(&p.with)?;
    let out = csg::boolean(&a, &b, b_to_a, p.operation.unwrap_or_default())?;
    store(s, &p.id, out.clone())?;
    if p.hide_other.unwrap_or(true) {
        s.get_mut(&p.with)?.visible = false;
    }
    Ok(result(&out, &Elements::default()))
}

fn opt3(v: Option<[f32; 3]>, what: &str, default: Vec3) -> CmdResult<Vec3> {
    v.map(|v| finite3(v, what)).transpose().map(|v| v.unwrap_or(default))
}

fn edit_data(s: &mut Scene, p: EditDataParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let primitive = matches!(&s.get(&p.id)?.kind, EntityKind::Mesh { source: MeshSource::Primitive { .. }, .. });
    let mut out = json!({
        "positions": m.positions,
        "faces": m.faces,
        "edges": me::all_edges(&m),
        "primitive": primitive,
    });
    if p.face_info.unwrap_or(false) {
        let info: Vec<Value> = m
            .faces
            .iter()
            .map(
                |f| json!({ "center": me::face_center(&m, f).to_array(), "normal": me::face_normal(&m, f).to_array() }),
            )
            .collect();
        out["faceInfo"] = json!(info);
    }
    Ok(out)
}

fn select(s: &mut Scene, p: SelectParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let topo = me::Topology::new(&m);
    let edge = || p.edge.ok_or_else(|| CommandError::invalid("give `edge` (a vertex pair) for this selection"));
    let sel = match p.how {
        SelectHow::Loop => {
            let e = edge()?;
            Elements::edges(me::edge_loop(&m, &topo, (e[0], e[1]))?.into_iter().map(|(a, b)| [a, b]))
        }
        SelectHow::Ring => {
            let e = edge()?;
            Elements::edges(me::edge_ring(&m, &topo, (e[0], e[1]))?.edges.into_iter().map(|(a, b)| [a, b]))
        }
        SelectHow::FaceLoop => {
            let e = edge()?;
            Elements::faces(me::edge_ring(&m, &topo, (e[0], e[1]))?.faces.into_iter().map(|f| f as u32))
        }
        SelectHow::Linked => me::linked(&m, &p.sel.elements())?,
        SelectHow::More => me::grow(&m, &p.sel.elements())?,
        SelectHow::All => Elements {
            vertices: (0..m.positions.len() as u32).collect(),
            edges: me::all_edges(&m),
            faces: (0..m.faces.len() as u32).collect(),
        },
    };
    Ok(json!({ "selection": sel.normalized() }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("mesh.editData", "A mesh's editable geometry: vertex positions, faces and the edge list (indices used by the edit tools).", mutates: false, EditDataParams, edit_data),
        command!("mesh.makeEditable", "Turn a primitive into editable geometry (edit tools do this on their own).", mutates: true, IdOnly,
        |s, p| { let m = edit_mesh(s, &p.id)?; store(s, &p.id, m.clone())?; Ok(result(&m, &Elements::default())) }),
        command!("mesh.select", "Work out a selection: edge loop or ring, face loop, linked part, grow, or all. Returns `selection` (changes nothing).", mutates: false, SelectParams, select),
        command!("mesh.transformElements", "Move, rotate or scale selected vertices / edges / faces (Blender G, R, S in edit mode).", mutates: true, TransformParams,
        |s, p| {
            let t = opt3(p.translate, "translate", Vec3::ZERO)?;
            let r = opt3(p.rotate, "rotate", Vec3::ZERO)?;
            let sc = opt3(p.scale, "scale", Vec3::ONE)?;
            let pivot = p.pivot.map(|v| finite3(v, "pivot")).transpose()?;
            let sel = p.sel.elements();
            match p.proportional {
                Some(pr) => edit(s, &p.id, |m| {
                    me::transform_proportional(m, &sel, t, r, sc, pivot, pr.radius, pr.falloff.unwrap_or_default())
                }),
                None => edit(s, &p.id, |m| me::transform(m, &sel, t, r, sc, pivot)),
            }
        }),
        command!("mesh.extrude", "Extrude selected faces (as a region or each on its own) or edges, optionally moving the new geometry out. Keeps the extruded part selected.", mutates: true, ExtrudeParams,
        |s, p| {
            let o = Offset { distance: p.distance.unwrap_or(0.0), vector: opt3(p.offset, "offset", Vec3::ZERO)? };
            let sel = p.sel.elements();
            edit(s, &p.id, |m| me::extrude(m, &sel, o, p.individual.unwrap_or(false)))
        }),
        command!("mesh.inset", "Inset selected faces: a smaller face inside, joined by a ring of faces (Blender I).", mutates: true, InsetParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::inset(m, &sel, p.thickness, p.depth.unwrap_or(0.0), p.individual.unwrap_or(false))) }),
        command!("mesh.bevel", "Bevel selected edges (faces: their edges) into a chamfer or a rounded strip (Blender Ctrl+B).", mutates: true, BevelParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::bevel(m, &sel, p.width, p.segments.unwrap_or(1))) }),
        command!("mesh.loopCut", "Cut new edge loops across the quad ring of an edge (Blender Ctrl+R). Selects the new loops.", mutates: true, LoopCutParams,
        |s, p| edit(s, &p.id, |m| me::loop_cut(m, p.edge, p.cuts.unwrap_or(1), p.factor.unwrap_or(0.5)))),
        command!("mesh.subdivideFaces", "Split selected faces into smaller quads; neighbours stay joined.", mutates: true, SelParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::subdivide(m, &sel)) }),
        command!("mesh.connect", "Split faces with a new edge between two selected vertices on each (Blender J).", mutates: true, SelParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::connect(m, &sel)) }),
        command!("mesh.deleteElements", "Delete selected vertices, edges or faces (and what depends on them).", mutates: true, DeleteParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::delete(m, &sel, p.mode)) }),
        command!("mesh.merge", "Merge selected vertices into one (Blender M).", mutates: true, MergeParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::merge(m, &sel, p.at.unwrap_or_default())) }),
        command!("mesh.mergeByDistance", "Merge vertices closer than a distance (whole mesh, or the selection): cleans imported or mirrored meshes.", mutates: true, MergeDistanceParams,
        |s, p| {
            let sel = p.sel.elements();
            let m = edit_mesh(s, &p.id)?;
            let (out, sel, removed) = me::merge_by_distance(&m, &sel, p.distance.unwrap_or(1e-4))?;
            store(s, &p.id, out.clone())?;
            let mut r = result(&out, &sel);
            r["removed"] = json!(removed);
            Ok(r)
        }),
        command!("mesh.fill", "Make a face from the selection: a closed ring of border edges, or 3+ vertices (Blender F).", mutates: true, SelParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::fill(m, &sel)) }),
        command!("mesh.flipNormals", "Turn selected faces (all if none given) inside out.", mutates: true, SelParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::flip(m, &sel)) }),
        command!("mesh.bridge", "Join two border edge loops (or two selected faces / regions, which are removed first) with a strip of faces (Blender Bridge Edge Loops). Selects the new faces.", mutates: true, SelParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::bridge(m, &sel)) }),
        command!("mesh.edgeSlide", "Slide selected edges along their neighbouring edges (Blender GG): factor -1..1.", mutates: true, SlideParams,
        |s, p| { let sel = p.sel.elements(); edit(s, &p.id, |m| me::edge_slide(m, &sel, p.factor)) }),
        command!("mesh.bisect", "Cut faces along a plane (the selected faces, or all): the knife tool uses this. Optionally remove one side. Selects the new edges.", mutates: true, BisectParams,
        |s, p| {
            let point = finite3(p.point, "point")?;
            let normal = finite3(p.normal, "normal")?;
            let sel = p.sel.elements();
            edit(s, &p.id, |m| me::bisect(m, &sel, point, normal, p.clear.unwrap_or_default()))
        }),
        command!("mesh.knife", "Cut along a path of surface points as seen from an eye (or direction), like Blender's knife: the cut stops at the path's ends and turns at points inside faces. Selects the new edges.", mutates: true, KnifeParams, knife),
        command!("mesh.spin", "Extrude the selected edges or faces round and round an axis (Blender Spin): edges sweep into a surface, faces into a curved solid; 360° joins up.", mutates: true, SpinParams,
        |s, p| {
            let axis = opt3(p.axis, "axis", Vec3::Y)?;
            let center = opt3(p.center, "center", Vec3::ZERO)?;
            let sel = p.sel.elements();
            edit(s, &p.id, |m| me::spin(m, &sel, center, axis, p.angle.unwrap_or(360.0), p.steps.unwrap_or(12)))
        }),
        command!("mesh.boolean", "Combine a mesh with another: union, difference (carve `with` out) or intersect. Both should be closed solids. The other mesh is hidden unless hideOther is false.", mutates: true, BooleanParams, boolean),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    #[test]
    fn bridge_slide_bisect_boolean() {
        let mut s = Session::default();
        s.dispatch(
            "entity.create",
            json!({"id": "box", "type": "mesh", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        let cut = s.dispatch("mesh.loopCut", json!({"id": "box", "edge": [0, 4]})).unwrap().result;
        let lp = cut["selection"]["edges"].clone();
        s.dispatch("mesh.edgeSlide", json!({"id": "box", "edges": lp, "factor": 0.5})).unwrap();
        let b = s
            .dispatch("mesh.bisect", json!({"id": "box", "point": [0, 0, 0], "normal": [1, 0, 0], "clear": "front"}))
            .unwrap()
            .result;
        assert!(!b["selection"]["edges"].as_array().unwrap().is_empty());
        let d = s.dispatch("mesh.editData", json!({"id": "box"})).unwrap().result;
        assert!(d["positions"].as_array().unwrap().iter().all(|p| p[0].as_f64().unwrap() <= 1e-5));
        // carve a cylinder out of a fresh box
        s.dispatch(
            "entity.create",
            json!({"id": "a", "type": "mesh", "primitive": {"shape": "cube", "size": [2, 2, 2]}}),
        )
        .unwrap();
        s.dispatch(
            "entity.create",
            json!({"id": "rod", "type": "mesh", "primitive": {"shape": "cylinder", "radius": 0.3, "height": 4, "segments": 12}}),
        )
        .unwrap();
        let r = s.dispatch("mesh.boolean", json!({"id": "a", "with": "rod"})).unwrap().result;
        assert!(r["faces"].as_u64().unwrap() > 6);
        assert!(!s.scene().get("rod").unwrap().visible);
        // knife across the top of a fresh box, seen from above
        s.dispatch(
            "entity.create",
            json!({"id": "k", "type": "mesh", "primitive": {"shape": "cube", "size": [2, 2, 2]}}),
        )
        .unwrap();
        let kn = s
            .dispatch(
                "mesh.knife",
                json!({"id": "k", "path": [[-2, 1, 0], [0, 1, 0], [0, 1, 2]], "eye": [0, 10, 0], "faces": [4]}),
            )
            .unwrap()
            .result;
        assert_eq!(kn["selection"]["edges"].as_array().unwrap().len(), 2);
        assert!(s.dispatch("mesh.knife", json!({"id": "k", "path": [[0, 1, 0]], "eye": [0, 10, 0]})).is_err());
        // spin the box's side a quarter turn round an axis beside it
        s.dispatch(
            "entity.create",
            json!({"id": "sp", "type": "mesh", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        let sp = s
            .dispatch("mesh.spin", json!({"id": "sp", "faces": [1], "angle": 90, "steps": 3, "center": [0, 0, 2]}))
            .unwrap()
            .result;
        assert_eq!(sp["faces"], 6 + 3 * 4 - 1 + 1);
        // a lathe shape is a primitive like the others
        s.dispatch(
            "entity.create",
            json!({"id": "vase", "type": "mesh", "primitive": {"shape": "lathe", "profile": [[0, 0], [0.1, 0], [0.12, 0.2], [0.05, 0.3]], "segments": 12}}),
        )
        .unwrap();
        // proportional move drags the neighbours part way
        s.dispatch(
            "mesh.transformElements",
            json!({"id": "k", "vertices": [0], "translate": [0, -1, 0], "proportional": {"radius": 5, "falloff": "linear"}}),
        )
        .unwrap();
        assert!(s.dispatch("mesh.boolean", json!({"id": "a", "with": "a"})).is_err());
        // a single face has only one border loop: nothing to bridge to
        s.dispatch(
            "entity.create",
            json!({"id": "t", "type": "mesh", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        assert!(s.dispatch("mesh.bridge", json!({"id": "t", "faces": [0]})).is_err());
    }

    #[test]
    fn edit_a_box_through_commands() {
        let mut s = Session::default();
        s.dispatch(
            "entity.create",
            json!({"id": "box", "type": "mesh", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        let d = s.dispatch("mesh.editData", json!({"id": "box"})).unwrap().result;
        assert_eq!(d["edges"].as_array().unwrap().len(), 12);
        assert_eq!(d["primitive"], true);
        // extrude the top, then inset it, as one undo step
        let r = s
            .batch(
                serde_json::from_value(json!([
                    {"command": "mesh.extrude", "params": {"id": "box", "faces": [4], "distance": 0.5}},
                    {"command": "mesh.inset", "params": {"id": "box", "faces": [4], "thickness": 0.1, "depth": -0.2}},
                ]))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(r.result[1]["faces"], 14);
        let st = s.dispatch("mesh.stats", json!({"id": "box"})).unwrap().result;
        assert!((st["bounds"]["max"][1].as_f64().unwrap() - 1.0).abs() < 1e-5);
        let cut = s.dispatch("mesh.loopCut", json!({"id": "box", "edge": [0, 1], "cuts": 2})).unwrap().result;
        assert!(cut["selection"]["edges"].as_array().unwrap().len() >= 8);
        let lp = s.dispatch("mesh.select", json!({"id": "box", "how": "linked", "vertices": [0]})).unwrap().result;
        assert!(!lp["selection"]["faces"].as_array().unwrap().is_empty());
        // chain: bevel the loop the cut just made
        let loop_edges = cut["selection"]["edges"].clone();
        s.dispatch("mesh.bevel", json!({"id": "box", "edges": loop_edges, "width": 0.02, "segments": 2})).unwrap();
        // a tried-and-dropped preview leaves nothing to redo
        s.dispatch("history.discard", json!({})).unwrap();
        assert_eq!(s.dispatch("history.info", json!({})).unwrap().result["redo"], json!([]));
        // bad input is an error, not a panic, and changes nothing
        let before = s.scene().clone();
        assert!(s.dispatch("mesh.extrude", json!({"id": "box", "faces": [999]})).is_err());
        assert!(s.dispatch("mesh.bevel", json!({"id": "box", "edges": [[0, 2]], "width": -1})).is_err());
        assert!(s.dispatch("mesh.loopCut", json!({"id": "box", "edge": [0, 0]})).is_err());
        assert_eq!(&before, s.scene());
    }
}

#[cfg(test)]
mod select_tests {
    use serde_json::json;

    use crate::Session;

    #[test]
    fn loops_on_a_cut_cube() {
        let mut s = Session::default();
        s.dispatch(
            "entity.create",
            json!({"id": "box", "type": "mesh", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        let cut = s.dispatch("mesh.loopCut", json!({"id": "box", "edge": [0, 4]})).unwrap().result;
        let ring = cut["selection"]["edges"].clone();
        assert_eq!(ring.as_array().unwrap().len(), 4);
        // the new loop is found again from any of its edges
        let e = ring[0].clone();
        let l = s.dispatch("mesh.select", json!({"id": "box", "how": "loop", "edge": e})).unwrap().result;
        assert_eq!(l["selection"]["edges"], ring);
        // the face loop across it runs round four sides
        let f = s.dispatch("mesh.select", json!({"id": "box", "how": "faceLoop", "edge": [5, 11]})).unwrap().result;
        assert_eq!(f["selection"]["faces"].as_array().unwrap().len(), 4);
    }
}
