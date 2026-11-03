//! Model import and export.
//!
//! - glTF / GLB: hierarchy, transforms, base colours, skeletons and skins (imported rigs can be
//!   posed), and punctual lights. Export bakes every mesh at the current frame in world space
//!   and adds the cameras and lights.
//! - OBJ (with MTL colours) and STL.
//! - FBX and `.blend` go through a local Blender in the background (desktop and CLI only).
//!
//! File bytes travel as base64 (`data`) or plain text (`text`), so the same commands work from
//! the browser, the CLI and MCP.

use std::collections::HashMap;

use base64::Engine as _;
use glam::{Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::eval::{camera_data_at, posed_mesh, world_at};
use shotstage_core::spatial;
use shotstage_core::{Entity, EntityKind, LightData, LightKind, Mesh, MeshSource, Scene, Skin, Transform};

use crate::builtin::{finite3, frame_ok};
use crate::{CmdResult, Command, CommandError, command};

/// Biggest model accepted, in vertices.
const MAX_VERTICES: usize = 3_000_000;

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ModelFormat {
    Glb,
    Gltf,
    Obj,
    Stl,
    /// Converted with a local Blender (desktop editor and CLI).
    Fbx,
    /// Import only, converted with a local Blender.
    Blend,
}

impl ModelFormat {
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "glb" => Self::Glb,
            "gltf" => Self::Gltf,
            "obj" => Self::Obj,
            "stl" => Self::Stl,
            "fbx" => Self::Fbx,
            "blend" => Self::Blend,
            _ => return None,
        })
    }
    pub fn extension(self) -> &'static str {
        match self {
            Self::Glb => "glb",
            Self::Gltf => "gltf",
            Self::Obj => "obj",
            Self::Stl => "stl",
            Self::Fbx => "fbx",
            Self::Blend => "blend",
        }
    }
    fn mime(self) -> &'static str {
        match self {
            Self::Glb => "model/gltf-binary",
            Self::Gltf => "model/gltf+json",
            Self::Obj => "model/obj",
            Self::Stl => "model/stl",
            _ => "application/octet-stream",
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "camelCase")]
enum Unit {
    /// Guess from the size (OBJ / STL); glTF is always metres.
    #[default]
    Auto,
    M,
    Cm,
    Mm,
    In,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
enum UpAxis {
    Y,
    Z,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ImportParams {
    format: ModelFormat,
    /// File contents, base64.
    data: Option<String>,
    /// Or, for text files (`.gltf` with embedded buffers, `.obj`, ASCII `.stl`), the text.
    text: Option<String>,
    /// OBJ: the `.mtl` file's text, for colours.
    mtl: Option<String>,
    /// Id of the group the model is put in. Default: generated from `name`.
    id: Option<String>,
    name: Option<String>,
    /// Where the bottom centre goes, metres. Default `[0, 0, 0]`.
    position: Option<[f32; 3]>,
    /// Turn about the vertical axis, degrees.
    rotation_y: Option<f32>,
    /// Extra uniform scale (after `unit`). Default 1.
    scale: Option<f32>,
    /// Scale the model so it is this tall in metres (e.g. 1.75 for a person). Overrides `scale`.
    fit_height: Option<f32>,
    /// Length unit of the file. OBJ and STL have none: `auto` (default) guesses from the size.
    unit: Option<Unit>,
    /// Which axis is up in the file. Default: y (z for STL, which usually comes from CAD).
    up_axis: Option<UpAxis>,
    /// Drop it onto whatever is below. Default true.
    drop: Option<bool>,
}

/// A node of an imported model before it becomes entities.
struct Node {
    name: String,
    parent: Option<usize>,
    transform: Transform,
    kind: NodeKind,
}

enum NodeKind {
    Empty,
    Mesh { mesh: Mesh, color: [f32; 3], skin: Option<NodeSkin> },
    Light(LightData),
}

struct NodeSkin {
    /// Joint node indices (into the imported node list).
    joints: Vec<usize>,
    inverse_bind: Vec<[f32; 16]>,
    vertex_joints: Vec<[u16; 4]>,
    vertex_weights: Vec<[f32; 4]>,
}

fn bad(msg: impl Into<String>) -> CommandError {
    CommandError::invalid(msg)
}

fn decode(b64: &str) -> CmdResult<Vec<u8>> {
    let b64 = b64.rsplit_once("base64,").map(|(_, d)| d).unwrap_or(b64);
    base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| bad(format!("`data` is not base64: {e}")))
}

pub fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn clamp01(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.7 })
}

// ---------------------------------------------------------------- glTF

fn parse_gltf(bytes: &[u8]) -> CmdResult<Vec<Node>> {
    let g = gltf::Gltf::from_slice(bytes).map_err(|e| bad(format!("not a glTF file: {e}")))?;
    let blob = g.blob.clone();
    let mut buffers = Vec::new();
    for b in g.document.buffers() {
        buffers.push(match b.source() {
            gltf::buffer::Source::Bin => blob.clone().ok_or_else(|| bad("the GLB has no binary chunk"))?,
            gltf::buffer::Source::Uri(uri) => match uri.strip_prefix("data:") {
                Some(rest) => decode(rest.split_once(',').map(|(_, d)| d).unwrap_or(rest))?,
                None => {
                    return Err(bad(format!(
                        "the glTF refers to an external file `{uri}`; export a single .glb (or embedded .gltf) instead"
                    )));
                }
            },
        });
    }
    let doc = &g.document;
    let Some(scene) = doc.default_scene().or_else(|| doc.scenes().next()) else {
        return Err(bad("the glTF has no scene"));
    };
    let mut nodes: Vec<Node> = Vec::new();
    let mut index_of: HashMap<usize, usize> = HashMap::new();
    // meshes whose skin is resolved after every node exists: (our node, gltf skin)
    let mut skinned: Vec<(usize, gltf::Skin)> = Vec::new();
    let mut stack: Vec<(gltf::Node, Option<usize>)> = scene.nodes().map(|n| (n, None)).collect();
    stack.reverse();
    let mut vertices = 0usize;
    while let Some((node, parent)) = stack.pop() {
        let (t, r, s) = node.transform().decomposed();
        let name = node.name().map(str::to_string).unwrap_or_else(|| format!("node_{}", node.index()));
        let me = nodes.len();
        index_of.insert(node.index(), me);
        let transform = Transform::from_parts(Vec3::from_array(t), Quat::from_array(r), Vec3::from_array(s));
        nodes.push(Node { name: name.clone(), parent, transform, kind: NodeKind::Empty });
        if let Some(light) = node.light() {
            let mut l = LightData { color: clamp01(light.color()), ..Default::default() };
            l.kind = match light.kind() {
                gltf::khr_lights_punctual::Kind::Directional => LightKind::Sun,
                gltf::khr_lights_punctual::Kind::Point => LightKind::Point,
                gltf::khr_lights_punctual::Kind::Spot { outer_cone_angle, .. } => {
                    l.cone_degrees = (outer_cone_angle.to_degrees() * 2.0).clamp(1.0, 179.0);
                    LightKind::Spot
                }
            };
            nodes[me].kind = NodeKind::Light(l);
        }
        if let Some(mesh) = node.mesh() {
            let prims: Vec<_> = mesh.primitives().filter(|p| p.mode() == gltf::mesh::Mode::Triangles).collect();
            for (pi, prim) in prims.iter().enumerate() {
                let reader = prim.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
                let Some(pos) = reader.read_positions() else { continue };
                let positions: Vec<[f32; 3]> = pos.collect();
                vertices += positions.len();
                if vertices > MAX_VERTICES {
                    return Err(bad(format!("the model has more than {MAX_VERTICES} vertices")));
                }
                let idx: Vec<u32> = match reader.read_indices() {
                    Some(i) => i.into_u32().collect(),
                    None => (0..positions.len() as u32).collect(),
                };
                let faces = idx.as_chunks::<3>().0.iter().map(|t| t.to_vec()).collect();
                let pbr = prim.material().pbr_metallic_roughness();
                let f = pbr.base_color_factor();
                // textures are not drawn: a textured white material shows as light clay
                let color = if pbr.base_color_texture().is_some() && f[0] > 0.95 && f[1] > 0.95 && f[2] > 0.95 {
                    [0.78, 0.76, 0.72]
                } else {
                    clamp01([f[0], f[1], f[2]])
                };
                let skin = match (node.skin(), reader.read_joints(0), reader.read_weights(0)) {
                    (Some(_), Some(j), Some(w)) => Some(NodeSkin {
                        joints: vec![],
                        inverse_bind: vec![],
                        vertex_joints: j.into_u16().collect(),
                        vertex_weights: w.into_f32().collect(),
                    }),
                    _ => None,
                };
                let kind = NodeKind::Mesh { mesh: Mesh { positions, faces }, color, skin };
                let target = if prims.len() == 1 && matches!(nodes[me].kind, NodeKind::Empty) {
                    nodes[me].kind = kind;
                    me
                } else {
                    nodes.push(Node {
                        name: format!("{name}_{pi}"),
                        parent: Some(me),
                        transform: Transform::default(),
                        kind,
                    });
                    nodes.len() - 1
                };
                if let (Some(s), NodeKind::Mesh { skin: Some(_), .. }) = (node.skin(), &nodes[target].kind) {
                    skinned.push((target, s));
                }
            }
        }
        let kids: Vec<_> = node.children().collect();
        for c in kids.into_iter().rev() {
            stack.push((c, Some(me)));
        }
    }
    for (target, s) in skinned {
        let reader = s.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
        let joints: Vec<usize> = s.joints().filter_map(|j| index_of.get(&j.index()).copied()).collect();
        if joints.len() != s.joints().count() {
            return Err(bad("a skin uses a joint that is not in the scene"));
        }
        let ib: Vec<[f32; 16]> = match reader.read_inverse_bind_matrices() {
            Some(m) => m.map(|c| Mat4::from_cols_array_2d(&c).to_cols_array()).collect(),
            None => vec![Mat4::IDENTITY.to_cols_array(); joints.len()],
        };
        if let NodeKind::Mesh { skin: Some(sk), .. } = &mut nodes[target].kind {
            sk.joints = joints;
            sk.inverse_bind = ib;
        }
    }
    Ok(nodes)
}

// ---------------------------------------------------------------- OBJ / STL

fn parse_mtl(text: &str) -> HashMap<String, [f32; 3]> {
    let mut out = HashMap::new();
    let mut cur = None;
    for line in text.lines() {
        let mut t = line.split_whitespace();
        match t.next() {
            Some("newmtl") => cur = t.next().map(str::to_string),
            Some("Kd") => {
                let v: Vec<f32> = t.filter_map(|x| x.parse().ok()).collect();
                if let (Some(c), [r, g, b, ..]) = (&cur, v.as_slice()) {
                    out.insert(c.clone(), clamp01([*r, *g, *b]));
                }
            }
            _ => {}
        }
    }
    out
}

fn parse_obj(text: &str, mtl: Option<&str>) -> CmdResult<Vec<Node>> {
    let colors = mtl.map(parse_mtl).unwrap_or_default();
    let mut v: Vec<[f32; 3]> = Vec::new();
    // (object name, material) -> faces with global vertex indices
    let mut groups: Vec<(String, Option<String>, Vec<Vec<u32>>)> = Vec::new();
    let (mut object, mut material) = ("model".to_string(), None::<String>);
    for (ln, line) in text.lines().enumerate() {
        let mut t = line.split_whitespace();
        match t.next() {
            Some("v") => {
                let c: Vec<f32> = t.take(3).filter_map(|x| x.parse().ok()).collect();
                if c.len() != 3 {
                    return Err(bad(format!("line {}: bad vertex", ln + 1)));
                }
                v.push([c[0], c[1], c[2]]);
                if v.len() > MAX_VERTICES {
                    return Err(bad(format!("the model has more than {MAX_VERTICES} vertices")));
                }
            }
            Some("o") | Some("g") => object = t.collect::<Vec<_>>().join("_"),
            Some("usemtl") => material = t.next().map(str::to_string),
            Some("f") => {
                let mut face = Vec::new();
                for tok in t {
                    let i: i64 = tok
                        .split('/')
                        .next()
                        .and_then(|x| x.parse().ok())
                        .ok_or_else(|| bad(format!("line {}: bad face", ln + 1)))?;
                    let i = if i < 0 { v.len() as i64 + i } else { i - 1 };
                    if i < 0 || i as usize >= v.len() {
                        return Err(bad(format!("line {}: face uses a vertex that does not exist", ln + 1)));
                    }
                    face.push(i as u32);
                }
                if face.len() >= 3 {
                    match groups.last_mut() {
                        Some(g) if g.0 == object && g.1 == material => g.2.push(face),
                        _ => groups.push((object.clone(), material.clone(), vec![face])),
                    }
                }
            }
            _ => {}
        }
    }
    if groups.is_empty() {
        return Err(bad("the OBJ has no faces"));
    }
    let mut nodes = vec![];
    for (name, mat, faces) in groups {
        // keep only the vertices this group uses
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut positions = vec![];
        let faces = faces
            .into_iter()
            .map(|f| {
                f.into_iter()
                    .map(|i| {
                        *remap.entry(i).or_insert_with(|| {
                            positions.push(v[i as usize]);
                            positions.len() as u32 - 1
                        })
                    })
                    .collect()
            })
            .collect();
        let color = mat.as_ref().and_then(|m| colors.get(m)).copied().unwrap_or([0.72, 0.70, 0.66]);
        let name = match &mat {
            Some(m) if !colors.is_empty() => format!("{name}_{m}"),
            _ => name,
        };
        nodes.push(Node {
            name,
            parent: None,
            transform: Transform::default(),
            kind: NodeKind::Mesh { mesh: Mesh { positions, faces }, color, skin: None },
        });
    }
    Ok(nodes)
}

fn parse_stl(bytes: &[u8]) -> CmdResult<Vec<Node>> {
    let mut tris: Vec<[f32; 3]> = Vec::new();
    let binary = bytes.len() >= 84 && {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        84 + n.saturating_mul(50) == bytes.len()
    };
    if binary {
        let n = (bytes.len() - 84) / 50;
        for i in 0..n {
            let b = &bytes[84 + i * 50..84 + i * 50 + 50];
            for k in 0..3 {
                let o = 12 + k * 12;
                let f = |j: usize| f32::from_le_bytes([b[o + j], b[o + j + 1], b[o + j + 2], b[o + j + 3]]);
                tris.push([f(0), f(4), f(8)]);
            }
        }
    } else {
        let text = std::str::from_utf8(bytes).map_err(|_| bad("not an STL file"))?;
        for line in text.lines() {
            let mut t = line.split_whitespace();
            if t.next() == Some("vertex") {
                let c: Vec<f32> = t.take(3).filter_map(|x| x.parse().ok()).collect();
                if c.len() == 3 {
                    tris.push([c[0], c[1], c[2]]);
                }
            }
        }
    }
    if tris.len() < 3 || tris.len() > MAX_VERTICES * 3 {
        return Err(bad("the STL has no triangles (or far too many)"));
    }
    // weld the corners shared between triangles
    let mut index: HashMap<[i64; 3], u32> = HashMap::new();
    let mut positions = vec![];
    let mut faces = vec![];
    for t in tris.as_chunks::<3>().0 {
        let mut f = vec![];
        for p in t {
            if !p.iter().all(|v| v.is_finite()) {
                return Err(bad("the STL has non-finite coordinates"));
            }
            let key = p.map(|v| (v as f64 * 1e5).round() as i64);
            f.push(*index.entry(key).or_insert_with(|| {
                positions.push(*p);
                positions.len() as u32 - 1
            }));
        }
        if f[0] != f[1] && f[1] != f[2] && f[0] != f[2] {
            faces.push(f);
        }
    }
    Ok(vec![Node {
        name: "model".into(),
        parent: None,
        transform: Transform::default(),
        kind: NodeKind::Mesh { mesh: Mesh { positions, faces }, color: [0.72, 0.70, 0.66], skin: None },
    }])
}

// ---------------------------------------------------------------- import

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(40)
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() { "part".into() } else { s }
}

fn import(scene: &mut Scene, p: ImportParams) -> CmdResult<Value> {
    let bytes = match (&p.data, &p.text) {
        (Some(d), None) => decode(d)?,
        (None, Some(t)) => t.as_bytes().to_vec(),
        _ => return Err(bad("give the file as `data` (base64) or `text`")),
    };
    let mut warnings: Vec<String> = vec![];
    let mut nodes = match p.format {
        ModelFormat::Glb | ModelFormat::Gltf => parse_gltf(&bytes)?,
        ModelFormat::Obj => {
            parse_obj(std::str::from_utf8(&bytes).map_err(|_| bad("the OBJ is not text"))?, p.mtl.as_deref())?
        }
        ModelFormat::Stl => parse_stl(&bytes)?,
        ModelFormat::Fbx | ModelFormat::Blend => {
            let glb = blender_convert(&bytes, p.format.extension(), "glb")?;
            warnings.push("converted with the local Blender".into());
            parse_gltf(&glb)?
        }
    };
    if nodes.is_empty() {
        return Err(bad("the file has nothing to import"));
    }
    // file axes and units -> scene (metres, +Y up)
    let metric = matches!(p.format, ModelFormat::Glb | ModelFormat::Gltf | ModelFormat::Fbx | ModelFormat::Blend);
    let up = p.up_axis.unwrap_or(if p.format == ModelFormat::Stl { UpAxis::Z } else { UpAxis::Y });
    let z_up = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    if up == UpAxis::Z {
        for n in nodes.iter_mut().filter(|n| n.parent.is_none()) {
            let t = n.transform;
            n.transform = Transform::from_parts(z_up * t.translation(), z_up * t.rotation(), t.scale());
        }
    }
    let unit = match (p.unit.unwrap_or_default(), metric) {
        (Unit::Auto, true) | (Unit::M, _) => 1.0,
        (Unit::Cm, _) => 0.01,
        (Unit::Mm, _) => 0.001,
        (Unit::In, _) => 0.0254,
        (Unit::Auto, false) => {
            let size = nodes
                .iter()
                .filter_map(|n| match &n.kind {
                    NodeKind::Mesh { mesh, .. } => mesh.bounds(),
                    _ => None,
                })
                .fold(0.0f32, |a, (lo, hi)| a.max((hi - lo).max_element()));
            let (u, label) = if size > 200.0 {
                (0.001, "millimetres")
            } else if size > 20.0 {
                (0.01, "centimetres")
            } else {
                (1.0, "metres")
            };
            warnings.push(format!(
                "no unit given: the model is {size:.1} units big, read as {label} (pass `unit` to change)"
            ));
            u
        }
    };
    let scale = p.scale.unwrap_or(1.0);
    if !(scale.is_finite() && (1e-4..=1e4).contains(&scale)) {
        return Err(bad("scale must be 0.0001..=10000"));
    }
    let stem = sanitize(p.name.as_deref().unwrap_or("model")).to_lowercase();
    let root = match p.id {
        Some(id) => {
            shotstage_core::scene::validate_id(&id)?;
            id
        }
        None => scene.unique_id(&stem),
    };
    let pos = finite3(p.position.unwrap_or([0.0; 3]), "position")?;
    let yaw = p.rotation_y.unwrap_or(0.0);
    if !yaw.is_finite() {
        return Err(bad("rotationY must be finite"));
    }
    scene.insert(Entity {
        id: root.clone(),
        name: p.name.clone().unwrap_or_else(|| root.clone()),
        parent: None,
        transform: Transform::from_parts(pos, Quat::from_rotation_y(yaw.to_radians()), Vec3::splat(unit * scale)),
        visible: true,
        locked: false,
        kind: EntityKind::Empty,
    })?;
    // ids first (skins refer to joints by id)
    let mut ids: Vec<String> = Vec::with_capacity(nodes.len());
    for n in &nodes {
        let base = format!("{root}_{}", sanitize(&n.name));
        let id = if scene.contains(&base) || ids.contains(&base) {
            (2..).map(|k| format!("{base}_{k}")).find(|c| !scene.contains(c) && !ids.contains(c)).expect("unbounded")
        } else {
            base
        };
        ids.push(id);
    }
    let mut skins = 0;
    for (i, n) in nodes.into_iter().enumerate() {
        let kind = match n.kind {
            NodeKind::Empty => EntityKind::Empty,
            NodeKind::Light(l) => EntityKind::Light(l),
            NodeKind::Mesh { mesh, color, skin } => {
                mesh.validate()?;
                let source = match skin {
                    Some(s) if !s.joints.is_empty() => {
                        let skin = Skin {
                            joints: s.joints.iter().map(|j| ids[*j].clone()).collect(),
                            inverse_bind: s.inverse_bind,
                            vertex_joints: s.vertex_joints,
                            vertex_weights: s.vertex_weights,
                        };
                        skin.validate(mesh.positions.len())?;
                        skins += 1;
                        MeshSource::Skinned { mesh, skin }
                    }
                    _ => MeshSource::Data { mesh },
                };
                EntityKind::Mesh { source, modifiers: vec![], color }
            }
        };
        scene.insert(Entity {
            id: ids[i].clone(),
            name: n.name,
            parent: Some(n.parent.map(|p| ids[p].clone()).unwrap_or_else(|| root.clone())),
            transform: n.transform,
            visible: true,
            locked: false,
            kind,
        })?;
    }
    let frame = scene.timeline.start as f32;
    if let Some(h) = p.fit_height {
        if !(h.is_finite() && h > 0.0 && h < 1e4) {
            return Err(bad("fitHeight must be positive"));
        }
        if let Some(b) = spatial::bounds(scene, &root, frame)? {
            let k = h / b.size().y.max(1e-6);
            let e = scene.get_mut(&root)?;
            e.transform.scale = (e.transform.scale() * k).to_array();
        }
    }
    // put the bottom centre at `position`, on whatever is below
    if let Some(b) = spatial::bounds(scene, &root, frame)? {
        let e = scene.get_mut(&root)?;
        let shift = Vec3::new(pos.x - b.center().x, pos.y - b.min.y, pos.z - b.center().z);
        e.transform.translation = (e.transform.translation() + shift).to_array();
    }
    if p.drop.unwrap_or(true) {
        crate::placement::drop_down(scene, &root, frame, None)?;
    }
    let size = spatial::bounds(scene, &root, frame)?.map(|b| b.size().to_array());
    Ok(json!({ "id": root, "entities": ids.len() + 1, "skins": skins, "size": size, "warnings": warnings }))
}

// ---------------------------------------------------------------- export

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExportParams {
    /// glb (default), gltf, obj, stl or fbx (fbx needs a local Blender).
    format: Option<ModelFormat>,
    /// Only these entities (with their children). Default: everything visible.
    ids: Option<Vec<String>>,
    /// Frame to pose the scene at. Default: the timeline start.
    frame: Option<f32>,
    /// File name without extension. Default `scene`.
    name: Option<String>,
    /// glb / gltf / fbx: include the animation (camera moves, object motion, poses) sampled at
    /// every frame of the timeline. Default: when the scene has keyframes.
    animation: Option<bool>,
}

struct Baked {
    /// Entity the mesh belongs to.
    id: String,
    name: String,
    color: [f32; 3],
    /// World-space triangles.
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

/// Meshes posed at `frame`: in world space, or in each entity's own space when `local` (the
/// animated export moves them with node transforms).
fn bake(scene: &Scene, ids: &Option<Vec<String>>, frame: f32, local: bool) -> CmdResult<Vec<Baked>> {
    let wanted: Option<Vec<String>> = match ids {
        Some(ids) => {
            let mut all = vec![];
            for id in ids {
                scene.get(id)?;
                all.extend(spatial::subtree(scene, id));
            }
            Some(all)
        }
        None => None,
    };
    let mut out = vec![];
    for e in &scene.entities {
        let EntityKind::Mesh { color, .. } = &e.kind else { continue };
        if wanted.as_ref().is_some_and(|w| !w.contains(&e.id)) || spatial::is_hidden(scene, &e.id) {
            continue;
        }
        let world = if local { Mat4::IDENTITY } else { world_at(scene, &e.id, frame)? };
        let tri = posed_mesh(scene, &e.id, frame)?.transformed(world).triangulate(false);
        if tri.indices.is_empty() {
            continue;
        }
        out.push(Baked {
            id: e.id.clone(),
            name: e.name.clone(),
            color: *color,
            positions: tri.positions,
            normals: tri.normals,
            indices: tri.indices,
        });
    }
    Ok(out)
}

/// Frames an animated export samples: (start, end, fps).
type AnimRange = (f32, f32, f32);

/// glTF JSON + binary buffer for baked meshes, cameras and lights; with `anim`, the meshes are
/// local and every node that moves gets translation / rotation / scale channels.
fn build_gltf(
    scene: &Scene,
    meshes: &[Baked],
    frame: f32,
    with_extras: bool,
    anim: Option<AnimRange>,
) -> (Value, Vec<u8>) {
    // entity behind each node (for animation)
    let mut node_ids: Vec<Option<String>> = vec![];
    let mut bin: Vec<u8> = Vec::new();
    let (mut views, mut accessors, mut gmeshes, mut materials, mut nodes) = (vec![], vec![], vec![], vec![], vec![]);
    let mut push = |bin: &mut Vec<u8>, bytes: &[u8], target: u32| -> usize {
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let mut v = json!({ "buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len() });
        // animation data is not a vertex or index buffer: no target
        if target != 0 {
            v["target"] = json!(target);
        }
        views.push(v);
        bin.extend_from_slice(bytes);
        views.len() - 1
    };
    for m in meshes {
        let pos: Vec<u8> = m.positions.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        let nor: Vec<u8> = m.normals.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        let idx: Vec<u8> = m.indices.iter().flat_map(|v| v.to_le_bytes()).collect();
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &m.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let vp = push(&mut bin, &pos, 34962);
        let vn = push(&mut bin, &nor, 34962);
        let vi = push(&mut bin, &idx, 34963);
        let a = accessors.len();
        accessors.push(json!({ "bufferView": vp, "componentType": 5126, "count": m.positions.len(), "type": "VEC3", "min": lo, "max": hi }));
        accessors.push(json!({ "bufferView": vn, "componentType": 5126, "count": m.normals.len(), "type": "VEC3" }));
        accessors.push(json!({ "bufferView": vi, "componentType": 5125, "count": m.indices.len(), "type": "SCALAR" }));
        materials.push(json!({ "name": m.name, "pbrMetallicRoughness": { "baseColorFactor": [m.color[0], m.color[1], m.color[2], 1.0], "metallicFactor": 0.0, "roughnessFactor": 0.8 } }));
        gmeshes.push(json!({ "name": m.name, "primitives": [{ "attributes": { "POSITION": a, "NORMAL": a + 1 }, "indices": a + 2, "material": materials.len() - 1 }] }));
        let mut node = json!({ "name": m.name, "mesh": gmeshes.len() - 1 });
        if anim.is_some()
            && let Ok(w) = world_at(scene, &m.id, frame)
        {
            let (sc, r, t) = w.to_scale_rotation_translation();
            node["translation"] = json!(t.to_array());
            node["rotation"] = json!(r.to_array());
            node["scale"] = json!(sc.to_array());
        }
        nodes.push(node);
        node_ids.push(Some(m.id.clone()));
    }
    let mut cameras = vec![];
    let mut lights = vec![];
    if with_extras {
        for e in &scene.entities {
            if spatial::is_hidden(scene, &e.id) {
                continue;
            }
            let Ok(w) = world_at(scene, &e.id, frame) else { continue };
            let (_, r, t) = w.to_scale_rotation_translation();
            match &e.kind {
                EntityKind::Camera(_) => {
                    let Ok(c) = camera_data_at(scene, &e.id, frame) else { continue };
                    cameras.push(json!({ "name": e.name, "type": "perspective", "perspective": { "yfov": c.vfov(), "aspectRatio": c.aspect, "znear": c.near, "zfar": c.far } }));
                    nodes.push(json!({ "name": e.name, "camera": cameras.len() - 1, "translation": t.to_array(), "rotation": r.to_array() }));
                    node_ids.push(Some(e.id.clone()));
                }
                EntityKind::Light(l) => {
                    let mut j = json!({ "name": e.name, "color": l.color, "intensity": l.intensity * 10.0 });
                    j["type"] = json!(match l.kind {
                        LightKind::Sun => "directional",
                        LightKind::Spot | LightKind::Area => "spot",
                        LightKind::Point => "point",
                    });
                    if matches!(l.kind, LightKind::Spot | LightKind::Area) {
                        let outer = if l.kind == LightKind::Area { 1.4 } else { (l.cone_degrees / 2.0).to_radians() };
                        j["spot"] = json!({ "outerConeAngle": outer, "innerConeAngle": outer * (1.0 - l.softness) });
                    }
                    lights.push(j);
                    nodes.push(json!({ "name": e.name, "translation": t.to_array(), "rotation": r.to_array(), "extensions": { "KHR_lights_punctual": { "light": lights.len() - 1 } } }));
                    node_ids.push(Some(e.id.clone()));
                }
                _ => {}
            }
        }
    }
    let mut animations = vec![];
    if let Some((start, end, fps)) = anim {
        // sample every frame (at most 2000) and keep the nodes that actually move
        let n = ((end - start).max(0.0).round() as usize).min(2000);
        let frames: Vec<f32> = (0..=n).map(|i| start + (end - start) * i as f32 / n.max(1) as f32).collect();
        let times: Vec<f32> = frames.iter().map(|f| (f - start) / fps.max(1.0)).collect();
        let mut channels = vec![];
        let mut samplers = vec![];
        let mut input = None;
        for (ni, id) in node_ids.iter().enumerate() {
            let Some(id) = id else { continue };
            let Ok(trs) = frames
                .iter()
                .map(|f| world_at(scene, id, *f).map(|w| w.to_scale_rotation_translation()))
                .collect::<shotstage_core::Result<Vec<_>>>()
            else {
                continue;
            };
            let (s0, r0, t0) = trs[0];
            let moves = trs
                .iter()
                .any(|(s, r, t)| (*t - t0).length() > 1e-5 || r.angle_between(r0) > 1e-4 || (*s - s0).length() > 1e-5);
            if !moves {
                continue;
            }
            let input = *input.get_or_insert_with(|| {
                let bytes: Vec<u8> = times.iter().flat_map(|v| v.to_le_bytes()).collect();
                let v = push(&mut bin, &bytes, 0);
                accessors.push(json!({ "bufferView": v, "componentType": 5126, "count": times.len(), "type": "SCALAR", "min": [times[0]], "max": [*times.last().unwrap_or(&0.0)] }));
                accessors.len() - 1
            });
            // keep quaternions continuous so interpolation takes the short way
            let mut prev = r0;
            let rots: Vec<Quat> = trs
                .iter()
                .map(|(_, r, _)| {
                    let r = if r.dot(prev) < 0.0 { -*r } else { *r };
                    prev = r;
                    r
                })
                .collect();
            for (path, data, kind) in [
                ("translation", trs.iter().flat_map(|(_, _, t)| t.to_array()).collect::<Vec<f32>>(), "VEC3"),
                ("rotation", rots.iter().flat_map(|r| r.to_array()).collect(), "VEC4"),
                ("scale", trs.iter().flat_map(|(s, _, _)| s.to_array()).collect(), "VEC3"),
            ] {
                let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
                let v = push(&mut bin, &bytes, 0);
                accessors.push(json!({ "bufferView": v, "componentType": 5126, "count": times.len(), "type": kind }));
                samplers.push(json!({ "input": input, "output": accessors.len() - 1, "interpolation": "LINEAR" }));
                channels.push(json!({ "sampler": samplers.len() - 1, "target": { "node": ni, "path": path } }));
            }
        }
        if !channels.is_empty() {
            animations.push(json!({ "name": "shotstage", "channels": channels, "samplers": samplers }));
        }
    }
    let mut doc = json!({
        "asset": { "version": "2.0", "generator": "shotstage" },
        "scene": 0,
        "scenes": [{ "nodes": (0..nodes.len()).collect::<Vec<_>>() }],
        "nodes": nodes,
        "meshes": gmeshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{ "byteLength": bin.len() }],
    });
    if !cameras.is_empty() {
        doc["cameras"] = json!(cameras);
    }
    if !animations.is_empty() {
        doc["animations"] = json!(animations);
    }
    if !lights.is_empty() {
        doc["extensionsUsed"] = json!(["KHR_lights_punctual"]);
        doc["extensions"] = json!({ "KHR_lights_punctual": { "lights": lights } });
    }
    (doc, bin)
}

fn glb(doc: &Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(doc).unwrap_or_default();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = bin.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

fn export(scene: &mut Scene, p: ExportParams) -> CmdResult<Value> {
    let frame = frame_ok(p.frame.unwrap_or(scene.timeline.start as f32))?;
    let format = p.format.unwrap_or(ModelFormat::Glb);
    let animated = matches!(format, ModelFormat::Glb | ModelFormat::Gltf | ModelFormat::Fbx)
        && p.animation.unwrap_or(scene.timeline.tracks.iter().any(|t| !t.keys.is_empty()));
    let anim = animated.then_some((scene.timeline.start as f32, scene.timeline.end as f32, scene.timeline.fps));
    let frame = if animated { scene.timeline.start as f32 } else { frame };
    let meshes = bake(scene, &p.ids, frame, animated)?;
    if meshes.is_empty() {
        return Err(CommandError::new("failed", "nothing to export: no visible meshes"));
    }
    let name = sanitize(p.name.as_deref().unwrap_or("scene"));
    let tris: usize = meshes.iter().map(|m| m.indices.len() / 3).sum();
    let file = format!("{name}.{}", format.extension());
    let mut out = json!({ "format": format.extension(), "fileName": file, "mimeType": format.mime(), "meshes": meshes.len(), "triangles": tris, "animated": animated });
    match format {
        ModelFormat::Glb => {
            let (doc, bin) = build_gltf(scene, &meshes, frame, true, anim);
            out["data"] = json!(encode(&glb(&doc, &bin)));
        }
        ModelFormat::Gltf => {
            let (mut doc, bin) = build_gltf(scene, &meshes, frame, true, anim);
            doc["buffers"][0]["uri"] = json!(format!("data:application/octet-stream;base64,{}", encode(&bin)));
            out["text"] = json!(serde_json::to_string_pretty(&doc).unwrap_or_default());
        }
        ModelFormat::Obj => {
            let mut obj = format!("# shotstage\nmtllib {name}.mtl\n");
            let mut mtl = String::from("# shotstage\n");
            let mut base = 1u32;
            for (i, m) in meshes.iter().enumerate() {
                obj.push_str(&format!("o {}\nusemtl mat_{i}\n", sanitize(&m.name)));
                mtl.push_str(&format!("newmtl mat_{i}\nKd {} {} {}\n", m.color[0], m.color[1], m.color[2]));
                for v in &m.positions {
                    obj.push_str(&format!("v {} {} {}\n", v[0], v[1], v[2]));
                }
                for t in m.indices.chunks(3) {
                    obj.push_str(&format!("f {} {} {}\n", t[0] + base, t[1] + base, t[2] + base));
                }
                base += m.positions.len() as u32;
            }
            out["text"] = json!(obj);
            out["mtl"] = json!(mtl);
        }
        ModelFormat::Stl => {
            let mut b = vec![0u8; 80];
            b[..18].copy_from_slice(b"shotstage export  ");
            b.extend_from_slice(&(tris as u32).to_le_bytes());
            for m in &meshes {
                for t in m.indices.chunks(3) {
                    let p = |i: u32| Vec3::from_array(m.positions[i as usize]);
                    let n = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).normalize_or_zero();
                    for v in [n, p(t[0]), p(t[1]), p(t[2])] {
                        for c in v.to_array() {
                            b.extend_from_slice(&c.to_le_bytes());
                        }
                    }
                    b.extend_from_slice(&[0, 0]);
                }
            }
            out["data"] = json!(encode(&b));
        }
        ModelFormat::Fbx => {
            let (doc, bin) = build_gltf(scene, &meshes, frame, true, anim);
            out["data"] = json!(encode(&blender_convert(&glb(&doc, &bin), "glb", "fbx")?));
        }
        ModelFormat::Blend => return Err(bad("blend is import only; export glb, fbx, obj or stl")),
    }
    Ok(out)
}

// ---------------------------------------------------------------- Blender bridge

/// Convert between formats with a local Blender running in the background.
#[cfg(not(target_arch = "wasm32"))]
fn blender_convert(bytes: &[u8], from: &str, to: &str) -> CmdResult<Vec<u8>> {
    crate::blender::convert(bytes, from, to).map_err(|e| CommandError::new("failed", e))
}

#[cfg(target_arch = "wasm32")]
fn blender_convert(_: &[u8], from: &str, _: &str) -> CmdResult<Vec<u8>> {
    Err(CommandError::new(
        "failed",
        format!(
            "{from} files are converted with Blender, which the browser cannot run: use the desktop editor or `shotstage import`, or export a .glb from your tool"
        ),
    ))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("model.import", "Import a 3D model as a group: glTF/GLB (hierarchy, colours, skeleton and skin, lights), OBJ (+MTL), STL; FBX and .blend through a local Blender. File as base64 `data` or `text`.", mutates: true, ImportParams, import),
        command!("model.export", "Export the scene (or some entities) posed at a frame: glb (with cameras and lights), gltf, obj (+mtl), stl, or fbx through a local Blender. Returns base64 `data` or `text`.", mutates: false, ExportParams, export),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_box() -> crate::Session {
        let mut s = crate::Session::default();
        s.dispatch("prefab.create", json!({"kind": "table", "id": "t"})).unwrap();
        s
    }

    #[test]
    fn glb_round_trip_keeps_size_and_colour() {
        let mut s = session_with_box();
        let out = s.dispatch("model.export", json!({"format": "glb", "ids": ["t"]})).unwrap().result;
        assert_eq!(out["meshes"], 5);
        let data = out["data"].as_str().unwrap().to_string();
        let r = s
            .dispatch("model.import", json!({"format": "glb", "data": data, "name": "copy", "position": [3, 0, 0]}))
            .unwrap()
            .result;
        let size = r["size"].as_array().unwrap();
        assert!(
            (size[0].as_f64().unwrap() - 1.2).abs() < 1e-3 && (size[1].as_f64().unwrap() - 0.75).abs() < 1e-3,
            "{r}"
        );
        let b = s.dispatch("entity.bounds", json!({"id": r["id"]})).unwrap().result;
        assert!(
            (b["center"][0].as_f64().unwrap() - 3.0).abs() < 1e-3 && b["min"][1].as_f64().unwrap().abs() < 1e-3,
            "{b}"
        );
        // one undo removes the whole import
        s.dispatch("history.undo", json!({})).unwrap();
        assert!(!s.scene().contains("copy"));
    }

    #[test]
    fn animated_export_has_channels_for_what_moves() {
        let mut s = session_with_box();
        s.dispatch("entity.create", json!({"type": "camera", "id": "cam", "position": [0, 1.5, 5]})).unwrap();
        s.dispatch("motion.apply", json!({"preset": "dollyIn", "target": "cam", "duration": 2})).unwrap();
        s.dispatch("motion.apply", json!({"preset": "turntable", "target": "t", "duration": 2})).unwrap();
        let out = s.dispatch("model.export", json!({"format": "glb"})).unwrap().result;
        assert_eq!(out["animated"], true);
        let bytes = base64::engine::general_purpose::STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let g = gltf::Gltf::from_slice(&bytes).unwrap();
        let anim = g.document.animations().next().expect("an animation");
        // the camera and the table's five parts move; 3 channels each
        assert_eq!(anim.channels().count(), 6 * 3);
        let still = s.dispatch("model.export", json!({"format": "glb", "animation": false})).unwrap().result;
        assert_eq!(still["animated"], false);
    }

    #[test]
    fn obj_and_stl() {
        let mut s = session_with_box();
        let out = s.dispatch("model.export", json!({"format": "obj"})).unwrap().result;
        let (text, mtl) = (out["text"].as_str().unwrap().to_string(), out["mtl"].as_str().unwrap().to_string());
        let r =
            s.dispatch("model.import", json!({"format": "obj", "text": text, "mtl": mtl, "unit": "m"})).unwrap().result;
        assert_eq!(r["entities"], 6);
        let stl = s.dispatch("model.export", json!({"format": "stl", "ids": ["t"]})).unwrap().result;
        let r = s
            .dispatch("model.import", json!({"format": "stl", "data": stl["data"], "upAxis": "y", "unit": "m"}))
            .unwrap()
            .result;
        assert!((r["size"][1].as_f64().unwrap() - 0.75).abs() < 1e-3, "{r}");
        // a millimetre STL is read as millimetres
        let text = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 500 0 0\nvertex 0 500 0\nendloop\nendfacet\nendsolid";
        let r = s.dispatch("model.import", json!({"format": "stl", "text": text})).unwrap().result;
        assert!(r["warnings"][0].as_str().unwrap().contains("millimetres"), "{r}");
        assert!(s.dispatch("model.import", json!({"format": "obj", "text": "v 0 0 0\nf 1 2 3"})).is_err());
        assert!(s.dispatch("model.import", json!({"format": "glb", "data": "AAAA"})).is_err());
    }

    #[test]
    fn skinned_gltf_follows_its_joints() {
        // a two-joint "arm": a quad from y=0 to y=2, the top half bound to the second joint
        let positions: Vec<f32> = vec![-0.1, 0.0, 0.0, 0.1, 0.0, 0.0, -0.1, 2.0, 0.0, 0.1, 2.0, 0.0];
        let joints: Vec<u16> = vec![0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0];
        let weights: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let ibm: Vec<f32> = [Mat4::IDENTITY, Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0))]
            .iter()
            .flat_map(|m| m.to_cols_array())
            .collect();
        let idx: Vec<u16> = vec![0, 1, 3, 0, 3, 2];
        let mut bin = vec![];
        let mut views = vec![];
        for (bytes, target) in [
            (positions.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), Some(34962)),
            (joints.iter().flat_map(|v| v.to_le_bytes()).collect(), Some(34962)),
            (weights.iter().flat_map(|v| v.to_le_bytes()).collect(), Some(34962)),
            (ibm.iter().flat_map(|v| v.to_le_bytes()).collect(), None),
            (idx.iter().flat_map(|v| v.to_le_bytes()).collect(), Some(34963)),
        ] {
            let mut v = json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len()});
            if let Some(t) = target {
                v["target"] = json!(t);
            }
            views.push(v);
            bin.extend(bytes);
            while bin.len() % 4 != 0 {
                bin.push(0);
            }
        }
        let doc = json!({
            "asset": {"version": "2.0"}, "scene": 0,
            "scenes": [{"nodes": [0, 1]}],
            "nodes": [
                {"name": "arm", "mesh": 0, "skin": 0},
                {"name": "shoulder", "children": [2]},
                {"name": "elbow", "translation": [0, 1, 0]},
            ],
            "skins": [{"joints": [1, 2], "inverseBindMatrices": 3}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2}, "indices": 4}]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [-0.1, 0, 0], "max": [0.1, 2, 0]},
                {"bufferView": 1, "componentType": 5123, "count": 4, "type": "VEC4"},
                {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC4"},
                {"bufferView": 3, "componentType": 5126, "count": 2, "type": "MAT4"},
                {"bufferView": 4, "componentType": 5123, "count": 6, "type": "SCALAR"},
            ],
            "bufferViews": views,
            "buffers": [{"byteLength": bin.len()}],
        });
        let mut s = crate::Session::default();
        let data = encode(&glb(&doc, &bin));
        let r = s.dispatch("model.import", json!({"format": "glb", "data": data, "id": "rig"})).unwrap().result;
        assert_eq!(r["skins"], 1);
        let top = |s: &crate::Session| spatial::bounds(s.scene(), "rig_arm", 0.0).unwrap().unwrap().max.y;
        assert!((top(&s) - 2.0).abs() < 1e-4);
        // bend the elbow 90°: the upper half swings sideways, so the arm gets shorter
        s.dispatch("entity.setTransform", json!({"id": "rig_elbow", "rotationEuler": [0, 0, 90]})).unwrap();
        assert!((top(&s) - 1.1).abs() < 0.02, "{}", top(&s));
    }
}
