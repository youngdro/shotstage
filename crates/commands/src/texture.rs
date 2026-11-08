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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UvTransformParams {
    id: String,
    /// Faces whose UVs move. Default: all.
    #[serde(default)]
    faces: Vec<u32>,
    /// Move by `[du, dv]` (1 = the whole picture).
    translate: Option<[f32; 2]>,
    /// Turn, degrees.
    rotate: Option<f32>,
    /// Scale `[su, sv]` about the faces' UV centre.
    scale: Option<[f32; 2]>,
}

fn uv_transform(s: &mut Scene, p: UvTransformParams) -> CmdResult<Value> {
    let m = edit_mesh(s, &p.id)?;
    let out = uv::transform_uvs(
        &m,
        &p.faces,
        p.translate.unwrap_or([0.0; 2]),
        p.rotate.unwrap_or(0.0),
        p.scale.unwrap_or([1.0; 2]),
    )?;
    store_uvs(s, &p.id, out)?;
    Ok(json!({ "faces": if p.faces.is_empty() { m.faces.len() } else { p.faces.len() } }))
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
        command!("mesh.uvTransform", "Move, turn or scale the UVs of some faces (all by default), e.g. to fit a label onto the front.", mutates: true, UvTransformParams, uv_transform),
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
}
