//! Textures on meshes: give a mesh a picture (`mesh.setMaterial`), lay it on by projection or
//! through a UV map made with `mesh.unwrap`, and nudge the UVs of some faces
//! (`mesh.uvTransform`). `mesh.uvData` returns the coordinates for scripts and UV editors.
//!
//! ```json
//! {"batch": [
//!   {"command": "mesh.setMaterial", "params": {"id": "can_1", "textureData": "<base64 PNG>", "mapping": "cylinder"}},
//!   {"command": "mesh.unwrap", "params": {"id": "box_1", "method": "smart"}}
//! ]}
//! ```

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::uv::{self, Mapping, Material, UnwrapMethod};
use shotstage_core::uvedit::{self, AlignTo, Corner};
use shotstage_core::{EntityKind, MeshSource, Scene};

use crate::builtin::color;
use crate::meshedit::edit_mesh;
use crate::{CmdResult, Command, CommandError, command};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaterialParams {
    id: String,
    /// Key of an image asset already in the scene (from `image.import`, `image.get` or an
    /// earlier `mesh.setMaterial`). `null` removes the texture.
    #[serde(default, deserialize_with = "some_null")]
    texture: Option<Option<String>>,
    /// A new picture for the texture: PNG, JPEG or WebP, base64 (a `data:` URL works too).
    texture_data: Option<String>,
    /// `box` (default), `planar`, `cylinder`, `sphere` or `uv` (the mesh's own UV map).
    mapping: Option<Mapping>,
    /// Planar / cylinder axis: 0 = X, 1 = Y, 2 = Z.
    axis: Option<u8>,
    /// Repeats of the picture across `[u, v]`.
    repeat: Option<[f32; 2]>,
    /// Shift of the picture, in pictures.
    offset: Option<[f32; 2]>,
    /// Turn of the picture, degrees.
    rotation: Option<f32>,
    /// Mesh colour (multiplies the picture). Default when a texture is first set: white, so the
    /// picture shows as it is.
    color: Option<[f32; 3]>,
    /// Remove the whole material (back to plain colour).
    clear: Option<bool>,
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn some_null<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

fn mesh_parts<'a>(
    scene: &'a mut Scene,
    id: &str,
) -> CmdResult<(&'a mut [f32; 3], &'a mut Option<Material>, &'a mut MeshSource)> {
    match &mut scene.get_mut(id)?.kind {
        EntityKind::Mesh { color, material, source, .. } => Ok((color, material, source)),
        _ => Err(CommandError::new("failed", format!("entity `{id}` is not a mesh"))),
    }
}

fn set_material(s: &mut Scene, p: MaterialParams) -> CmdResult<Value> {
    mesh_parts(s, &p.id)?;
    let new_asset = match &p.texture_data {
        Some(data) => {
            use base64::Engine as _;
            let b64 = data.rsplit_once("base64,").map(|(_, d)| d).unwrap_or(data);
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| CommandError::invalid(format!("`textureData` is not base64: {e}")))?;
            Some(crate::images::add_asset(s, &bytes, &format!("{}_texture", p.id))?.0)
        }
        None => None,
    };
    if let Some(Some(key)) = &p.texture
        && !s.assets.contains_key(key)
    {
        return Err(CommandError::invalid(format!("no image asset `{key}` in the scene")));
    }
    if p.texture.is_some() && new_asset.is_some() {
        return Err(CommandError::invalid("give `texture` or `textureData`, not both"));
    }
    let new_color = p.color.map(color).transpose()?;
    let (col, material, _) = mesh_parts(s, &p.id)?;
    if p.clear.unwrap_or(false) {
        *material = None;
        if let Some(c) = new_color {
            *col = c;
        }
        return Ok(json!({ "material": Value::Null }));
    }
    let mut m = material.clone().unwrap_or_default();
    let had_texture = m.texture.is_some();
    if let Some(t) = p.texture {
        m.texture = t;
    }
    if let Some(k) = new_asset {
        m.texture = Some(k);
    }
    if let Some(v) = p.mapping {
        m.mapping = v;
    }
    if p.axis.is_some() {
        m.axis = p.axis;
    }
    if let Some(v) = p.repeat {
        m.repeat = v;
    }
    if let Some(v) = p.offset {
        m.offset = v;
    }
    if let Some(v) = p.rotation {
        m.rotation = v;
    }
    m.validate()?;
    match new_color {
        Some(c) => *col = c,
        // a first texture shows as it is
        None if !had_texture && m.texture.is_some() => *col = [1.0; 3],
        None => {}
    }
    *material = Some(m.clone());
    Ok(json!({ "material": m }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UnwrapParams {
    id: String,
    /// `smart` (default: flat pieces packed without overlap), `box`, `planar`, `cylinder` or
    /// `sphere` (bake that projection).
    method: Option<UnwrapMethod>,
    /// Only these faces (the rest keep their UVs). Default: all.
    #[serde(default)]
    faces: Vec<u32>,
    /// Smart: faces bending less than this many degrees stay in one piece. Default 66.
    angle: Option<f32>,
    /// Planar / cylinder axis: 0 = X, 1 = Y, 2 = Z.
    axis: Option<u8>,
    /// Smart: gap between pieces, as a fraction of the square. Default 0.01.
    margin: Option<f32>,
}

fn store_uvs(s: &mut Scene, id: &str, mesh: shotstage_core::Mesh) -> CmdResult<()> {
    mesh.validate()?;
    let (_, material, source) = mesh_parts(s, id)?;
    *source = MeshSource::Data { mesh };
    // the UV map is what the texture follows from now on
    material.get_or_insert_with(Material::default).mapping = Mapping::Uv;
    Ok(())
}

fn unwrap(s: &mut Scene, p: UnwrapParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let out = uv::unwrap(&m, &p.faces, p.method.unwrap_or_default(), p.angle, p.axis, p.margin)?;
    let n = out.faces.len();
    store_uvs(s, &p.id, out)?;
    Ok(json!({ "faces": n, "mapping": "uv" }))
}

/// Which corners of the UV map a command works on.
#[derive(Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
struct UvPick {
    /// Faces: all their corners. With no `corners` either: the whole mesh.
    #[serde(default)]
    faces: Vec<u32>,
    /// Face corners `[face, index in the face]` (what the UV editor selects).
    #[serde(default)]
    corners: Vec<Corner>,
    /// Also take every corner at the same spot of the same vertex (Blender's sticky
    /// selection), so neighbouring faces stay joined. Default false.
    #[serde(default)]
    sticky: bool,
    /// Grow to whole islands (the flat pieces the corners are in). Default false.
    #[serde(default)]
    islands: bool,
}

impl UvPick {
    fn resolve(&self, m: &shotstage_core::Mesh) -> CmdResult<Vec<Corner>> {
        let mut c = uvedit::pick(m, &self.faces, &self.corners)?;
        if self.islands || self.sticky {
            let topo = uvedit::UvTopology::new(m);
            if self.islands {
                let faces: Vec<u32> = c.iter().map(|k| k[0]).collect();
                c = uvedit::face_corners(m, &topo.island_faces(&faces));
            }
            if self.sticky {
                c = topo.sticky(&c);
            }
        }
        Ok(c)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvTransformParams {
    id: String,
    #[serde(flatten)]
    pick: UvPick,
    /// Move by `[du, dv]` (1 = the whole picture; v runs down the picture).
    translate: Option<[f32; 2]>,
    /// Turn, degrees, counter-clockwise as seen in the UV editor (the picture on the surface
    /// turns the other way).
    rotate: Option<f32>,
    /// Scale `[su, sv]` (negative mirrors).
    scale: Option<[f32; 2]>,
    /// Point to turn and scale about. Default: the mean of the corners.
    pivot: Option<[f32; 2]>,
}

fn uv_transform(s: &mut Scene, p: UvTransformParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let corners = p.pick.resolve(&m)?;
    let out = uvedit::transform_corners(
        &m,
        &corners,
        p.translate.unwrap_or([0.0; 2]),
        p.rotate.unwrap_or(0.0),
        p.scale.unwrap_or([1.0; 2]),
        p.pivot,
    )?;
    store_uvs(s, &p.id, out)?;
    Ok(json!({ "corners": corners.len() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvSetParams {
    id: String,
    /// Face corners `[face, index in the face]`.
    corners: Vec<Corner>,
    /// New coordinate `[u, v]` of each corner, in the same order.
    uvs: Vec<[f32; 2]>,
}

fn uv_set(s: &mut Scene, p: UvSetParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    uvedit::pick(&m, &[], &p.corners)?;
    let out = uvedit::set_corners(&m, &p.corners, &p.uvs)?;
    store_uvs(s, &p.id, out)?;
    Ok(json!({ "corners": p.corners.len() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvAlignParams {
    id: String,
    #[serde(flatten)]
    pick: UvPick,
    /// `u`: line them up in a column (same u); `v`: in a row (same v).
    axis: UvAxis,
    /// `min` (left / top), `center` (default) or `max` (right / bottom).
    to: Option<AlignTo>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "camelCase")]
enum UvAxis {
    U,
    V,
}

fn uv_align(s: &mut Scene, p: UvAlignParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let corners = p.pick.resolve(&m)?;
    let out = uvedit::align(&m, &corners, p.axis as usize, p.to.unwrap_or_default())?;
    store_uvs(s, &p.id, out)?;
    Ok(json!({ "corners": corners.len() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvWeldParams {
    id: String,
    #[serde(flatten)]
    pick: UvPick,
    /// `center` (default): all the corners to one point. `stitch`: the corners of each mesh
    /// vertex to their own middle, joining pieces that were cut apart along a seam.
    mode: Option<WeldMode>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
enum WeldMode {
    #[default]
    Center,
    Stitch,
}

fn uv_weld(s: &mut Scene, p: UvWeldParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let corners = p.pick.resolve(&m)?;
    let out = uvedit::weld(&m, &corners, p.mode.unwrap_or_default() == WeldMode::Stitch)?;
    let islands = uvedit::UvTopology::new(&out).islands.len();
    store_uvs(s, &p.id, out)?;
    Ok(json!({ "corners": corners.len(), "islands": islands }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvPackParams {
    id: String,
    /// Pack only the islands these faces are in. Default: all.
    #[serde(default)]
    faces: Vec<u32>,
    /// Gap between islands, as a fraction of the square. Default 0.01.
    margin: Option<f32>,
    /// Turn tall islands to lie flat (packs tighter). Default true.
    rotate: Option<bool>,
}

fn uv_pack(s: &mut Scene, p: UvPackParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let out = uvedit::pack_islands(&m, &p.faces, p.margin.unwrap_or(0.01), p.rotate.unwrap_or(true))?;
    let islands = uvedit::UvTopology::new(&out).islands.len();
    store_uvs(s, &p.id, out)?;
    Ok(json!({ "islands": islands }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvSelectParams {
    id: String,
    #[serde(flatten)]
    pick: UvPick,
    /// Instead: every corner whose coordinate lies in this box `[[u0, v0], [u1, v1]]`.
    rect: Option<[[f32; 2]; 2]>,
}

fn uv_select(s: &mut Scene, p: UvSelectParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let corners = match p.rect {
        Some([a, b]) => {
            let (lo, hi) = ([a[0].min(b[0]), a[1].min(b[1])], [a[0].max(b[0]), a[1].max(b[1])]);
            let all = uvedit::pick(&m, &[], &[])?;
            let inside: Vec<Corner> = all
                .into_iter()
                .filter(|c| {
                    let u = m.uvs[c[0] as usize][c[1] as usize];
                    (lo[0]..=hi[0]).contains(&u[0]) && (lo[1]..=hi[1]).contains(&u[1])
                })
                .collect();
            UvPick { corners: inside, ..p.pick }.resolve_nonempty(&m)?
        }
        None => p.pick.resolve(&m)?,
    };
    let topo = uvedit::UvTopology::new(&m);
    // faces whose every corner is picked
    let set: std::collections::BTreeSet<Corner> = corners.iter().copied().collect();
    let faces: Vec<u32> = (0..m.faces.len() as u32)
        .filter(|&f| (0..m.faces[f as usize].len() as u32).all(|k| set.contains(&[f, k])))
        .collect();
    let islands: std::collections::BTreeSet<u32> = corners.iter().map(|c| topo.island[c[0] as usize]).collect();
    Ok(json!({ "corners": corners, "faces": faces, "islands": islands }))
}

impl UvPick {
    /// Like [`UvPick::resolve`] but an empty `corners` list stays empty (a box around nothing).
    fn resolve_nonempty(&self, m: &shotstage_core::Mesh) -> CmdResult<Vec<Corner>> {
        if self.corners.is_empty() && self.faces.is_empty() {
            uvedit::pick(m, &[], &[])?;
            return Ok(Vec::new());
        }
        self.resolve(m)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvIslandsParams {
    id: String,
}

fn uv_islands(s: &mut Scene, p: UvIslandsParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    if !m.has_uvs() {
        return Err(CommandError::new("failed", "the mesh has no UV map yet: unwrap it first"));
    }
    Ok(json!({ "islands": uvedit::island_info(&m) }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvDataParams {
    id: String,
}

fn uv_data(s: &mut Scene, p: UvDataParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let material = match &s.get(&p.id)?.kind {
        EntityKind::Mesh { material, .. } => material.clone(),
        _ => None,
    };
    Ok(json!({
        "hasUvMap": m.has_uvs(),
        "uvMap": if m.has_uvs() { json!(m.uvs) } else { Value::Null },
        "material": material,
        // where the texture actually lands, after repeat / offset / rotation
        "final": uv::face_uvs(&m, &material.unwrap_or_default()),
    }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("mesh.setMaterial", "Paint a picture on a mesh and say how it is laid on: box, planar, cylinder or sphere projection, or the mesh's UV map; repeat, offset, rotation. Give `textureData` (base64 image) or `texture` (an asset key); `clear` removes it.", mutates: true, MaterialParams, set_material),
        command!("mesh.unwrap", "Make a UV map for a mesh (all faces or some): smart (flat pieces packed without overlap) or a baked box / planar / cylinder / sphere projection. The texture then follows the UV map.", mutates: true, UnwrapParams, unwrap),
        command!("mesh.uvTransform", "Move, turn or scale part of the UV map: faces, face corners (`[face, index]`), optionally sticky (shared corners follow) or whole islands; all by default. E.g. fit a label onto the front.", mutates: true, UvTransformParams, uv_transform),
        command!("mesh.uvSet", "Put face corners of the UV map at exact coordinates (what dragging in the UV editor does).", mutates: true, UvSetParams, uv_set),
        command!("mesh.uvAlign", "Line UV corners up in a column (same u) or a row (same v): at their left / top, middle or right / bottom.", mutates: true, UvAlignParams, uv_align),
        command!("mesh.uvWeld", "Bring UV corners together: all to one point, or `stitch` each vertex's corners to their middle to join pieces cut apart along a seam.", mutates: true, UvWeldParams, uv_weld),
        command!("mesh.uvPack", "Lay the UV islands (all, or those of some faces) out again inside the picture square without overlap, keeping their relative sizes.", mutates: true, UvPackParams, uv_pack),
        command!("mesh.uvSelect", "Resolve a UV selection without changing anything: faces / corners / a box in UV space, grown sticky or to whole islands. Returns the corners, the faces fully inside and the islands touched.", mutates: false, UvSelectParams, uv_select),
        command!("mesh.uvIslands", "The UV map's islands (flat pieces): their faces, bounding box and area in UV space.", mutates: false, UvIslandsParams, uv_islands),
        command!("mesh.uvData", "A mesh's UV map (per face corner, if it has one), its material, and the final texture coordinates of every face corner.", mutates: false, UvDataParams, uv_data),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    /// A 2×2 PNG.
    fn png() -> String {
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
        crate::model::encode(out.get_ref())
    }

    #[test]
    fn texture_a_box_and_unwrap_it() {
        let mut s = Session::default();
        s.dispatch(
            "entity.create",
            json!({"id": "box", "type": "mesh", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        let r = s
            .dispatch(
                "mesh.setMaterial",
                json!({"id": "box", "textureData": png(), "mapping": "cylinder", "repeat": [2, 1]}),
            )
            .unwrap()
            .result;
        let key = r["material"]["texture"].as_str().unwrap().to_string();
        assert!(s.scene().assets.contains_key(&key));
        let d = s.dispatch("mesh.uvData", json!({"id": "box"})).unwrap().result;
        assert_eq!(d["hasUvMap"], false);
        assert_eq!(d["final"].as_array().unwrap().len(), 6);
        // the colour turned white so the picture shows as it is
        let e = s.dispatch("entity.get", json!({"id": "box"})).unwrap().result;
        assert_eq!(e["kind"]["color"], json!([1.0, 1.0, 1.0]));
        // unwrap: the material now follows the UV map
        s.dispatch("mesh.unwrap", json!({"id": "box"})).unwrap();
        let d = s.dispatch("mesh.uvData", json!({"id": "box"})).unwrap().result;
        assert_eq!(d["hasUvMap"], true);
        assert_eq!(d["material"]["mapping"], "uv");
        s.dispatch("mesh.uvTransform", json!({"id": "box", "faces": [0], "scale": [0.5, 0.5]})).unwrap();
        // moving vertices keeps the UV map; a topology tool drops it (the texture falls back to a box projection)
        s.dispatch("mesh.transformElements", json!({"id": "box", "vertices": [0], "translate": [0, -0.1, 0]})).unwrap();
        assert_eq!(s.dispatch("mesh.uvData", json!({"id": "box"})).unwrap().result["hasUvMap"], true);
        s.dispatch("mesh.extrude", json!({"id": "box", "faces": [4], "distance": 0.2})).unwrap();
        assert_eq!(s.dispatch("mesh.uvData", json!({"id": "box"})).unwrap().result["hasUvMap"], false);
        // reuse the asset on another mesh, then take it off
        s.dispatch("entity.create", json!({"id": "b2", "type": "mesh", "primitive": {"shape": "sphere", "radius": 0.5, "segments": 12, "rings": 6}}))
            .unwrap();
        s.dispatch("mesh.setMaterial", json!({"id": "b2", "texture": key, "mapping": "sphere"})).unwrap();
        s.dispatch("mesh.setMaterial", json!({"id": "b2", "texture": null})).unwrap();
        s.dispatch("mesh.setMaterial", json!({"id": "b2", "clear": true})).unwrap();
        // bad input changes nothing
        let before = s.scene().clone();
        assert!(s.dispatch("mesh.setMaterial", json!({"id": "box", "texture": "nope"})).is_err());
        assert!(s.dispatch("mesh.setMaterial", json!({"id": "box", "repeat": [0, 1]})).is_err());
        assert!(s.dispatch("mesh.uvTransform", json!({"id": "box", "scale": [2, 2]})).is_err());
        assert_eq!(&before, s.scene());
    }

    #[test]
    fn uv_editor_commands() {
        let mut s = Session::default();
        s.dispatch(
            "entity.create",
            json!({"id": "box", "type": "mesh", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        assert!(s.dispatch("mesh.uvIslands", json!({"id": "box"})).is_err());
        s.dispatch("mesh.unwrap", json!({"id": "box"})).unwrap();
        let isl = s.dispatch("mesh.uvIslands", json!({"id": "box"})).unwrap().result;
        assert_eq!(isl["islands"].as_array().unwrap().len(), 6);
        // drag one corner: sticky has nothing to share in separate pieces
        s.dispatch("mesh.uvSet", json!({"id": "box", "corners": [[0, 0]], "uvs": [[0.9, 0.9]]})).unwrap();
        let d = s.dispatch("mesh.uvData", json!({"id": "box"})).unwrap().result;
        assert!((d["uvMap"][0][0][0].as_f64().unwrap() - 0.9).abs() < 1e-6);
        // unknown fields are still refused, flattened or not
        assert!(s.dispatch("mesh.uvTransform", json!({"id": "box", "turn": 4})).is_err());
        // move a corner's whole island
        let r = s
            .dispatch(
                "mesh.uvTransform",
                json!({"id": "box", "corners": [[1, 0]], "islands": true, "translate": [0.5, 0]}),
            )
            .unwrap()
            .result;
        assert_eq!(r["corners"], 4);
        // select by box, align, stitch, pack
        let sel = s.dispatch("mesh.uvSelect", json!({"id": "box", "rect": [[-10, -10], [10, 10]]})).unwrap().result;
        assert_eq!(sel["corners"].as_array().unwrap().len(), 24);
        assert_eq!(sel["faces"].as_array().unwrap().len(), 6);
        s.dispatch("mesh.uvAlign", json!({"id": "box", "faces": [2], "axis": "u", "to": "min"})).unwrap();
        let w = s
            .dispatch("mesh.uvWeld", json!({"id": "box", "faces": [0, 1, 2, 3, 4, 5], "mode": "stitch"}))
            .unwrap()
            .result;
        assert!(w["islands"].as_u64().unwrap() < 6);
        s.dispatch("mesh.uvPack", json!({"id": "box", "margin": 0.02})).unwrap();
        let d = s.dispatch("mesh.uvData", json!({"id": "box"})).unwrap().result;
        for f in d["uvMap"].as_array().unwrap() {
            for c in f.as_array().unwrap() {
                let (u, v) = (c[0].as_f64().unwrap(), c[1].as_f64().unwrap());
                assert!((-1e-3..=1.001).contains(&u) && (-1e-3..=1.001).contains(&v));
            }
        }
        // each step is one undo
        s.dispatch("history.undo", json!({})).unwrap();
    }
}
