//! Images as scene elements: reference cards standing on a surface, billboards that always face
//! the camera (cut-out people, trees), backdrops behind the scene and floor decals. The aspect
//! ratio of the picture is kept.

use std::sync::Arc;

use glam::{Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::eval::world_at;
use shotstage_core::spatial;
use shotstage_core::{Asset, Entity, EntityKind, ImageData, Scene, Transform};

use crate::builtin::{finite3, frame_ok};
use crate::model::encode;
use crate::{CmdResult, Command, CommandError, command};

/// Longest side kept; bigger pictures are scaled down on import.
const MAX_SIDE: u32 = 2048;

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub enum ImageMode {
    /// A standing card resting on the surface below (a reference photo, a sign).
    #[default]
    Card,
    /// A standing card that always turns to face the viewer (cut-out person, tree).
    Billboard,
    /// A big picture behind the scene, facing the active camera and filling its view.
    Backdrop,
    /// Lying flat on the surface below (a rug, a floor plan, a logo on the ground).
    Floor,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ImportParams {
    /// PNG, JPEG or WebP file, base64 (a `data:` URL works too).
    data: String,
    name: Option<String>,
    id: Option<String>,
    /// card, billboard, backdrop or floor. Default: billboard for a cut-out (transparent
    /// PNG), else card.
    mode: Option<ImageMode>,
    /// Height of the card in metres (width follows the picture's aspect). Default: 1.7 for a
    /// billboard, 1 for a card, 2 for a floor decal (its depth); backdrops size themselves.
    height: Option<f32>,
    /// Where the bottom centre goes. Default: in front of the active camera (backdrop: behind
    /// the scene), else the origin.
    position: Option<[f32; 3]>,
    /// Turn about the vertical axis, degrees. Default: face the active camera.
    rotation_y: Option<f32>,
    /// Drop onto what is below. Default true (not for backdrops).
    drop: Option<bool>,
}

/// Decode an image, scale it down if huge, and store it as an asset. Returns the asset key.
pub fn add_asset(scene: &mut Scene, bytes: &[u8], stem: &str) -> CmdResult<(String, bool)> {
    let format = image::guess_format(bytes).map_err(|_| CommandError::invalid("not a PNG, JPEG or WebP image"))?;
    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| CommandError::invalid(format!("cannot read the image: {e}")))?;
    let (w, h) = (img.width(), img.height());
    // a picture with see-through pixels is a cut-out
    let cutout = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 128);
    if w == 0 || h == 0 {
        return Err(CommandError::invalid("the image is empty"));
    }
    let keep = w.max(h) <= MAX_SIDE && matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg);
    let (data, mime, w, h) = if keep {
        let mime = if format == image::ImageFormat::Png { "image/png" } else { "image/jpeg" };
        (encode(bytes), mime, w, h)
    } else {
        let img = if w.max(h) > MAX_SIDE {
            img.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let mut out = std::io::Cursor::new(Vec::new());
        let (fmt, mime) = if img.color().has_alpha() {
            (image::ImageFormat::Png, "image/png")
        } else {
            (image::ImageFormat::Jpeg, "image/jpeg")
        };
        let img = if fmt == image::ImageFormat::Jpeg { image::DynamicImage::ImageRgb8(img.to_rgb8()) } else { img };
        img.write_to(&mut out, fmt).map_err(|e| CommandError::new("failed", e.to_string()))?;
        (encode(out.get_ref()), mime, img.width(), img.height())
    };
    let stem: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(32).collect();
    let key = (1..).map(|n| format!("{stem}_{n}")).find(|k| !scene.assets.contains_key(k)).expect("unbounded");
    scene.assets.insert(key.clone(), Asset { mime: mime.into(), width: w, height: h, data: Arc::from(data) });
    Ok((key, cutout))
}

fn import(scene: &mut Scene, p: ImportParams) -> CmdResult<Value> {
    let bytes = {
        use base64::Engine as _;
        let b64 = p.data.rsplit_once("base64,").map(|(_, d)| d).unwrap_or(&p.data);
        base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| CommandError::invalid(format!("`data` is not base64: {e}")))?
    };
    let name = p.name.clone().unwrap_or_else(|| "image".into());
    let stem = name.to_lowercase();
    let (asset, cutout) = add_asset(scene, &bytes, &stem)?;
    let (pw, ph) = {
        let a = &scene.assets[&asset];
        (a.width as f32, a.height as f32)
    };
    let aspect = pw / ph;
    // cut-outs (people, trees) turn to the camera; photos stand as cards
    let mode = p.mode.unwrap_or(if cutout { ImageMode::Billboard } else { ImageMode::Card });
    let frame = scene.timeline.start as f32;
    let cam = shotstage_core::eval::camera_id_at(scene, frame);
    let cam_world = cam.as_ref().and_then(|c| world_at(scene, c, frame).ok());
    let scene_box = scene
        .entities
        .iter()
        .filter(|e| e.parent.is_none())
        .filter_map(|e| spatial::bounds(scene, &e.id, frame).ok().flatten())
        .reduce(|a, b| spatial::Aabb::union(Some(a), Some(b)).expect("both"));
    let mut height = p.height.unwrap_or(match mode {
        ImageMode::Billboard => 1.7,
        ImageMode::Floor => 2.0,
        _ => 1.0,
    });
    // where it goes and which way it faces
    let (mut pos, mut yaw) = (Vec3::ZERO, 0.0f32);
    if let Some(cw) = cam_world {
        let eye = cw.w_axis.truncate();
        let fwd = cw.transform_vector3(Vec3::NEG_Z);
        let flat = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
        yaw = (-flat.x).atan2(-flat.z);
        pos = match mode {
            ImageMode::Backdrop => {
                // behind everything the camera sees, big enough to fill its view
                let far = scene_box
                    .map(|b| (b.center() - eye).dot(flat) + b.half().max_element() + 1.0)
                    .unwrap_or(6.0)
                    .max(3.0);
                let lens = shotstage_core::eval::camera_data_at(scene, cam.as_deref().unwrap_or_default(), frame)
                    .unwrap_or_default();
                let fill = 2.0 * far * (lens.vfov() / 2.0).tan() * 1.15;
                let fill_w = fill * lens.aspect;
                height = p.height.unwrap_or(fill.max(fill_w / aspect));
                let c = Vec3::new(eye.x, 0.0, eye.z) + flat * far;
                Vec3::new(c.x, (eye.y - height / 2.0).min(0.0), c.z)
            }
            _ => {
                let c = scene_box.map(|b| b.center()).unwrap_or(Vec3::ZERO);
                Vec3::new(c.x, 0.0, c.z)
            }
        };
    } else if mode == ImageMode::Backdrop {
        height = p.height.unwrap_or(4.0);
        pos = Vec3::new(0.0, 0.0, -scene_box.map(|b| b.max.z.abs().max(b.min.z.abs()) + 1.0).unwrap_or(4.0));
    }
    if !(height.is_finite() && height > 0.0 && height <= 10_000.0) {
        return Err(CommandError::invalid("height must be positive"));
    }
    if let Some(v) = p.position {
        pos = finite3(v, "position")?;
    }
    if let Some(y) = p.rotation_y {
        if !y.is_finite() {
            return Err(CommandError::invalid("rotationY must be finite"));
        }
        yaw = y.to_radians();
    }
    let size = [height * aspect, height];
    let (lift, rot) = match mode {
        // lying flat, its top edge towards -Z (away from a viewer looking down -Z)
        ImageMode::Floor => (0.002, Quat::from_rotation_y(yaw) * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
        _ => (height / 2.0, Quat::from_rotation_y(yaw)),
    };
    let data = ImageData { asset, size, billboard: mode == ImageMode::Billboard };
    data.validate()?;
    let id = match p.id {
        Some(id) => {
            shotstage_core::scene::validate_id(&id)?;
            id
        }
        None => scene.unique_id(&stem),
    };
    scene.insert(Entity {
        id: id.clone(),
        name,
        parent: None,
        transform: Transform::from_parts(pos + Vec3::Y * lift, rot, Vec3::ONE),
        visible: true,
        locked: mode == ImageMode::Backdrop,
        kind: EntityKind::Image(data),
    })?;
    if p.drop.unwrap_or(mode != ImageMode::Backdrop) {
        crate::placement::drop_down(scene, &id, frame, None)?;
        if mode == ImageMode::Floor {
            // decals sit a hair above the surface so they do not flicker
            let e = scene.get_mut(&id)?;
            e.transform.translation[1] += 0.002;
        }
    }
    Ok(json!({ "id": id, "size": size, "pixels": [pw, ph] }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParams {
    id: String,
    /// Always face the viewer.
    billboard: Option<bool>,
    /// New height in metres (the width follows the picture's aspect).
    height: Option<f32>,
}

fn set(scene: &mut Scene, p: SetParams) -> CmdResult<Value> {
    let e = scene.get(&p.id)?;
    let EntityKind::Image(img) = &e.kind else {
        return Err(CommandError::new("failed", format!("entity `{}` is not an image", p.id)));
    };
    let mut img = img.clone();
    if let Some(b) = p.billboard {
        img.billboard = b;
    }
    if let Some(h) = p.height {
        let aspect = img.size[0] / img.size[1];
        img.size = [h * aspect, h];
    }
    img.validate()?;
    let old_h = match &scene.get(&p.id)?.kind {
        EntityKind::Image(i) => i.size[1],
        _ => unreachable!(),
    };
    let e = scene.get_mut(&p.id)?;
    // keep the bottom edge where it was when the height changes
    let up = e.transform.rotation() * Vec3::Y;
    let shift = up * (img.size[1] - old_h) / 2.0;
    e.transform.translation = (e.transform.translation() + shift).to_array();
    e.kind = EntityKind::Image(img.clone());
    Ok(json!({ "size": img.size, "billboard": img.billboard }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetParams {
    /// Asset key (see `scene.inspect` → assets), or an image entity id.
    id: String,
    frame: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReferenceParams {
    /// Camera whose view the picture is laid over. Default: the active camera.
    camera: Option<String>,
    /// A new picture (PNG / JPEG / WebP, base64).
    data: Option<String>,
    /// Or an existing asset key.
    asset: Option<String>,
    /// 0..1. Default 0.5.
    opacity: Option<f32>,
    /// Remove the reference.
    clear: Option<bool>,
}

fn set_reference(scene: &mut Scene, p: ReferenceParams) -> CmdResult<Value> {
    let cam = match p.camera {
        Some(c) => {
            scene.camera(&c)?;
            c
        }
        None => {
            scene.active_camera.clone().ok_or_else(|| CommandError::new("failed", "no active camera: give `camera`"))?
        }
    };
    if p.clear.unwrap_or(false) {
        scene.references.remove(&cam);
        return Ok(json!({ "camera": cam, "cleared": true }));
    }
    let opacity = p.opacity.unwrap_or(0.5);
    if !(opacity.is_finite() && (0.0..=1.0).contains(&opacity)) {
        return Err(CommandError::invalid("opacity must be 0..=1"));
    }
    let asset = match (p.data, p.asset) {
        (Some(d), None) => {
            use base64::Engine as _;
            let b64 = d.rsplit_once("base64,").map(|(_, x)| x).unwrap_or(&d);
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| CommandError::invalid(format!("`data` is not base64: {e}")))?;
            add_asset(scene, &bytes, "reference")?.0
        }
        (None, Some(a)) => {
            if !scene.assets.contains_key(&a) {
                return Err(CommandError::new("failed", format!("no image asset `{a}`")));
            }
            a
        }
        (None, None) => match scene.references.get(&cam) {
            Some(r) => r.asset.clone(),
            None => return Err(CommandError::invalid("give `data` or `asset`")),
        },
        _ => return Err(CommandError::invalid("give `data` or `asset`, not both")),
    };
    scene.references.insert(cam.clone(), shotstage_core::CameraReference { asset: asset.clone(), opacity });
    Ok(json!({ "camera": cam, "asset": asset, "opacity": opacity }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("camera.setReference", "Lay a picture (storyboard frame, reference photo) over a camera's view to match its framing; set the opacity, or clear it.", mutates: true, ReferenceParams, set_reference),
        command!("image.import", "Put a picture (PNG, JPEG, WebP; base64) in the scene, keeping its aspect: a standing card, a billboard that always faces the camera (cut-out people, trees), a backdrop filling the camera's background, or a floor decal.", mutates: true, ImportParams, import),
        command!("image.set", "Change an image card: billboard on/off, height (aspect kept, bottom edge stays).", mutates: true, SetParams, set),
        command!("image.get", "An image asset's size and type (pass the asset key or an image entity id); add the asset data with scene.inspect.", mutates: false, GetParams,
        |s, p| {
            frame_ok(p.frame.unwrap_or(0.0))?;
            let key = match s.get(&p.id).map(|e| &e.kind) {
                Ok(EntityKind::Image(i)) => i.asset.clone(),
                _ => p.id.clone(),
            };
            let a = s.assets.get(&key).ok_or_else(|| CommandError::new("failed", format!("no image asset `{key}`")))?;
            Ok(json!({ "asset": key, "mime": a.mime, "width": a.width, "height": a.height, "bytes": a.data.len() * 3 / 4 }))
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> String {
        let img = image::RgbaImage::from_fn(w, h, |x, _| image::Rgba([(x % 255) as u8, 80, 160, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
        encode(out.get_ref())
    }

    #[test]
    fn cards_keep_their_aspect_and_stand_on_surfaces() {
        let mut s = crate::Session::default();
        s.dispatch("prefab.create", json!({"kind": "table", "id": "t"})).unwrap();
        let r = s
            .dispatch("image.import", json!({"data": png(200, 100), "name": "Ref", "position": [0, 2, 0]}))
            .unwrap()
            .result;
        let size = r["size"].as_array().unwrap();
        assert!((size[0].as_f64().unwrap() - 2.0).abs() < 1e-5 && (size[1].as_f64().unwrap() - 1.0).abs() < 1e-5);
        let b = s.dispatch("entity.bounds", json!({"id": r["id"]})).unwrap().result;
        assert!((b["min"][1].as_f64().unwrap() - 0.75).abs() < 1e-3, "stands on the table: {b}");
        // billboard and resize
        s.dispatch("image.set", json!({"id": r["id"], "billboard": true, "height": 0.5})).unwrap();
        let b = s.dispatch("entity.bounds", json!({"id": r["id"]})).unwrap().result;
        assert!(
            (b["size"][1].as_f64().unwrap() - 0.5).abs() < 1e-4 && (b["min"][1].as_f64().unwrap() - 0.75).abs() < 1e-3,
            "{b}"
        );
        // backdrop behind the scene, facing the camera
        s.dispatch("entity.create", json!({"type": "camera", "id": "cam", "position": [0, 1.5, 5]})).unwrap();
        s.dispatch("camera.lookAt", json!({"id": "cam", "target": [0, 1, 0]})).unwrap();
        let r = s.dispatch("image.import", json!({"data": png(160, 90), "mode": "backdrop"})).unwrap().result;
        let b = s.dispatch("entity.bounds", json!({"id": r["id"]})).unwrap().result;
        assert!(b["max"][2].as_f64().unwrap() < -0.5, "behind the table: {b}");
        // a floor decal lies flat
        let r = s
            .dispatch("image.import", json!({"data": png(100, 100), "mode": "floor", "position": [3, 0, 0]}))
            .unwrap()
            .result;
        let b = s.dispatch("entity.bounds", json!({"id": r["id"]})).unwrap().result;
        assert!(b["size"][1].as_f64().unwrap() < 0.01, "{b}");
        // two cards share one asset per import; history snapshots share the pixels
        assert_eq!(s.scene().assets.len(), 3);
        assert!(s.dispatch("image.import", json!({"data": "bm90IGFuIGltYWdl"})).is_err());
        let info = s.dispatch("image.get", json!({"id": "ref_1"})).unwrap().result;
        assert_eq!(info["width"], 200);
    }

    #[test]
    fn camera_references() {
        let mut s = crate::Session::default();
        assert!(s.dispatch("camera.setReference", json!({"data": png(16, 9)})).is_err(), "no camera yet");
        s.dispatch("entity.create", json!({"type": "camera", "id": "cam"})).unwrap();
        let r = s.dispatch("camera.setReference", json!({"data": png(16, 9), "opacity": 0.3})).unwrap().result;
        assert_eq!(r["camera"], "cam");
        assert_eq!(s.scene().references["cam"].opacity, 0.3);
        s.dispatch("camera.setReference", json!({"opacity": 0.8})).unwrap();
        assert_eq!(s.scene().references["cam"].opacity, 0.8);
        assert!(s.dispatch("camera.setReference", json!({"opacity": 2})).is_err());
        s.dispatch("entity.delete", json!({"id": "cam"})).unwrap();
        assert!(s.scene().references.is_empty(), "deleting the camera drops its reference");
    }

    #[test]
    fn huge_images_are_scaled_down() {
        let mut s = crate::Session::default();
        let r = s.dispatch("image.import", json!({"data": png(4000, 1000)})).unwrap().result;
        assert_eq!(r["pixels"][0], 2048.0);
        assert_eq!(r["pixels"][1], 512.0);
    }
}
