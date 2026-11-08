//! The sky around the scene (built-in skies or an imported HDRI) and the shadow settings.

use std::sync::Arc;

use glam::{Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::environment::{self as env, SKIES};
use shotstage_core::{Asset, Entity, EntityKind, LightData, LightKind, Scene, Sky, Transform};

use crate::builtin::color;
use crate::{CmdResult, Command, CommandError, command};

/// Widest panorama kept in the scene (wider HDRIs are scaled down on import).
const MAX_HDRI_WIDTH: u32 = 2048;

#[derive(Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SetParams {
    /// studio (no sky light, default), color (one colour all round: a paper backdrop), day,
    /// sunset, overcast, night, softbox, or hdri (an imported panorama).
    pub sky: Option<Sky>,
    /// Colour sky: the colour; other skies: a tint. Linear RGB 0..1.
    pub color: Option<[f32; 3]>,
    /// Tint by colour temperature instead, Kelvin (6500 = neutral).
    pub temperature: Option<f32>,
    /// HDRI asset key (from environment.import); sets the sky to hdri.
    pub hdri: Option<String>,
    /// Brightness of the sky light and background, 0..=10.
    pub intensity: Option<f32>,
    /// Turn the sky (and its sun) about the vertical axis, degrees.
    pub rotation: Option<f32>,
    /// Show the sky behind the scene.
    pub background: Option<bool>,
}

pub(crate) fn set(scene: &mut Scene, p: SetParams) -> CmdResult<Value> {
    let mut e = scene.environment.clone();
    if let Some(k) = p.hdri {
        if !scene.assets.contains_key(&k) {
            return Err(CommandError::invalid(format!("no asset `{k}`: import the HDRI with environment.import")));
        }
        e.hdri = Some(k);
        e.sky = Sky::Hdri;
    }
    if let Some(s) = p.sky {
        if s == Sky::Hdri && e.hdri.is_none() {
            return Err(CommandError::invalid("no HDRI yet: import one with environment.import"));
        }
        e.sky = s;
    }
    if let Some(c) = p.color {
        e.color = color(c)?;
    }
    if let Some(k) = p.temperature {
        if !(k.is_finite() && (1000.0..=20000.0).contains(&k)) {
            return Err(CommandError::invalid("temperature must be 1000..=20000 K"));
        }
        e.color = shotstage_core::light::kelvin_to_rgb(k);
    }
    if let Some(v) = p.intensity {
        e.intensity = v;
    }
    if let Some(v) = p.rotation {
        e.rotation = v.rem_euclid(360.0);
    }
    if let Some(v) = p.background {
        e.background = v;
    }
    e.validate()?;
    scene.environment = e;
    Ok(summary(scene))
}

fn summary(scene: &Scene) -> Value {
    let e = &scene.environment;
    let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
    if let Some(sun) = env::sky_sun_elevation(e.sky) {
        v["sunElevation"] = json!(sun);
    }
    v
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ImportParams {
    /// An HDRI (Radiance .hdr) or an ordinary 2:1 panorama (PNG, JPEG, WebP), base64 (a
    /// `data:` URL works too).
    #[serde(default)]
    data: String,
    /// Or a file on disk to read (CLI and MCP only).
    path: Option<String>,
    name: Option<String>,
    /// Use it as the sky right away. Default true.
    apply: Option<bool>,
    rotation: Option<f32>,
    intensity: Option<f32>,
}

fn import(scene: &mut Scene, p: ImportParams) -> CmdResult<Value> {
    use base64::Engine as _;
    if p.path.is_some() {
        return Err(crate::host::needs_files("environment.import", "path"));
    }
    let raw = p.data.split_once("base64,").map_or(p.data.as_str(), |(_, d)| d).trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| CommandError::invalid("data is not base64"))?;
    let format = image::guess_format(&bytes)
        .map_err(|_| CommandError::invalid("not an HDRI (.hdr) or a PNG, JPEG or WebP panorama"))?;
    let stem = p.name.as_deref().unwrap_or("sky");
    let key = if format == image::ImageFormat::Hdr {
        // keep the light's full range: scale down if huge and store as .hdr again
        let map = env::decode_panorama(&bytes, MAX_HDRI_WIDTH)?;
        let pixels: Vec<f32> = map.rgb.iter().flatten().copied().collect();
        let img = image::Rgb32FImage::from_raw(map.width, map.height, pixels)
            .ok_or_else(|| CommandError::new("failed", "panorama size mismatch"))?;
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb32F(img)
            .write_to(&mut out, image::ImageFormat::Hdr)
            .map_err(|e| CommandError::new("failed", e.to_string()))?;
        let stem: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(32).collect();
        let key = (1..).map(|n| format!("{stem}_{n}")).find(|k| !scene.assets.contains_key(k)).expect("unbounded");
        let data = crate::model::encode(out.get_ref());
        scene.assets.insert(
            key.clone(),
            Asset { mime: "image/vnd.radiance".into(), width: map.width, height: map.height, data: Arc::from(data) },
        );
        key
    } else {
        crate::images::add_asset(scene, &bytes, stem)?.0
    };
    let a = &scene.assets[&key];
    let (w, h) = (a.width, a.height);
    let mut out = json!({ "asset": key, "width": w, "height": h, "hdr": format == image::ImageFormat::Hdr });
    if (w as f32 / h as f32 - 2.0).abs() > 0.2 {
        out["warning"] = json!("not a 2:1 panorama: the sky will look stretched");
    }
    if p.apply.unwrap_or(true) {
        out["environment"] = set(
            scene,
            SetParams { hdri: Some(key), rotation: p.rotation, intensity: p.intensity, ..Default::default() },
        )?;
    }
    Ok(out)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ShadowParams {
    /// Shadows on or off for the whole scene.
    enabled: Option<bool>,
    /// How dark shadows are, 0..1.
    strength: Option<f32>,
    /// Edge blur, 0 = crisp .. 1 = very soft.
    softness: Option<f32>,
    /// Turn shadow casting on or off for these lights (up to four lights cast at once).
    lights: Option<Vec<String>>,
    cast: Option<bool>,
}

fn shadows(scene: &mut Scene, p: ShadowParams) -> CmdResult<Value> {
    let mut s = scene.environment.shadows;
    if let Some(v) = p.enabled {
        s.enabled = v;
    }
    if let Some(v) = p.strength {
        s.strength = v;
    }
    if let Some(v) = p.softness {
        s.softness = v;
    }
    let mut e = scene.environment.clone();
    e.shadows = s;
    e.validate()?;
    if let Some(ids) = &p.lights {
        let cast = p.cast.ok_or_else(|| CommandError::invalid("give `cast` with `lights`"))?;
        for id in ids {
            let EntityKind::Light(l) = &mut scene.get_mut(id)?.kind else {
                return Err(CommandError::invalid(format!("`{id}` is not a light")));
            };
            l.cast_shadows = cast;
        }
    }
    scene.environment = e;
    let casting: Vec<&str> = scene
        .entities
        .iter()
        .filter(|e| matches!(&e.kind, EntityKind::Light(l) if l.cast_shadows))
        .map(|e| e.id.as_str())
        .collect();
    Ok(json!({ "shadows": s, "casting": casting }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SunParams {
    /// Brightness of the sun. Default: from how clear the sky's sun is (about 1.2 for a clear day).
    intensity: Option<f32>,
    id: Option<String>,
    /// Put it in this group.
    parent: Option<String>,
}

/// A sun light shining from the sky's brightest spot, in its colour.
pub(crate) fn sun_from_sky(
    scene: &mut Scene,
    intensity: Option<f32>,
    id: Option<String>,
    parent: Option<String>,
) -> CmdResult<Value> {
    let e = scene.environment.clone();
    let map = env::resolve(&e, &scene.assets, 256)
        .ok_or_else(|| CommandError::invalid("the studio sky has no sun: set a sky first (environment.set)"))?;
    let l = env::lighting(&env::lighting_map(&e, &map), e.rotation, 1.0);
    let toward = Vec3::from_array(l.key_dir);
    // a clear sun (a strong bright spot) is a strong light; a hazy one is weaker
    let intensity = intensity.unwrap_or((0.6 + 2.0 * l.directionality).min(1.8));
    let light = LightData {
        kind: LightKind::Sun,
        color: l.key_color.map(|c| c.clamp(0.0, 1.0)),
        intensity: intensity.clamp(0.0, 1000.0),
        ..Default::default()
    };
    light.validate()?;
    let id = id.unwrap_or_else(|| scene.unique_id("sun"));
    // a sun's position does not matter; put it up where it shines from, for the gizmo
    let pos = toward * 6.0;
    let rot = shotstage_core::camera::look_rotation(pos, Vec3::ZERO).unwrap_or(Quat::IDENTITY);
    scene.insert(Entity {
        id: id.clone(),
        name: "sun".into(),
        parent,
        transform: Transform::from_parts(pos, rot, Vec3::ONE),
        visible: true,
        locked: false,
        kind: EntityKind::Light(light),
    })?;
    let el = toward.y.clamp(-1.0, 1.0).asin().to_degrees();
    Ok(json!({ "id": id, "elevation": el, "directionality": l.directionality, "light": light }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("environment.set", "Set the sky around the scene: studio (default), color (seamless backdrop), day, sunset, overcast, night, softbox, or an imported hdri. The sky lights the scene softly from all round and can show behind it; turn it, tint it (colour or Kelvin) and set its brightness.", mutates: true, SetParams, set),
        command!("environment.import", "Import an HDRI (.hdr) or a 2:1 panorama picture and use it as the sky: it lights the scene and shows behind it. Pair it with light.fromEnvironment for a sun that matches.", mutates: true, ImportParams, import),
        command!("environment.list", "The built-in skies and the scene's current sky and shadow settings.", mutates: false, crate::builtin::NoParams,
        |s, _| Ok(json!({
            "skies": SKIES.iter().map(|(_, k, d)| json!({ "sky": k, "description": d })).collect::<Vec<_>>(),
            "environment": summary(s),
            "note": "Sky light adds to the scene's lights. With no lights, the sky alone lights the scene (and its sun casts a soft shadow). Rotation turns the sky's sun; light.fromEnvironment adds a matching sun light.",
        }))),
        command!("light.shadows", "Shadow settings: on/off, how dark (strength), how soft; and which lights cast them (up to four at once, brightest first).", mutates: true, ShadowParams, shadows),
        command!("light.fromEnvironment", "Add a sun light shining from the sky's brightest spot (the sun of a built-in sky or an HDRI), in its colour, so shadows match the sky.", mutates: true, SunParams,
            |s, p: SunParams| sun_from_sky(s, p.intensity, p.id, p.parent)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn hdr_b64(w: u32, h: u32, hot: (u32, u32)) -> String {
        let mut img = image::Rgb32FImage::from_pixel(w, h, image::Rgb([0.2, 0.25, 0.3]));
        img.put_pixel(hot.0, hot.1, image::Rgb([500.0, 450.0, 400.0]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb32F(img).write_to(&mut out, image::ImageFormat::Hdr).unwrap();
        crate::model::encode(out.get_ref())
    }

    #[test]
    fn skies_and_shadows() {
        let mut s = Session::default();
        let r =
            s.dispatch("environment.set", json!({"sky": "sunset", "rotation": 400, "intensity": 0.8})).unwrap().result;
        assert_eq!(r["sky"], "sunset");
        assert_eq!(r["rotation"], 40.0);
        assert!(r["sunElevation"].as_f64().unwrap() < 10.0);
        assert!(s.dispatch("environment.set", json!({"sky": "hdri"})).is_err());
        assert!(s.dispatch("environment.set", json!({"intensity": 11})).is_err());
        let r = s.dispatch("environment.set", json!({"temperature": 3000})).unwrap().result;
        assert!(r["color"][2].as_f64().unwrap() < 0.6);
        // a sun that matches the sky: low, from where the sky was turned to
        let r = s.dispatch("light.fromEnvironment", json!({})).unwrap().result;
        assert!(r["elevation"].as_f64().unwrap() < 12.0, "{r}");
        let id = r["id"].as_str().unwrap().to_string();
        let r = s
            .dispatch("light.shadows", json!({"strength": 0.5, "softness": 0.8, "lights": [id], "cast": false}))
            .unwrap()
            .result;
        assert_eq!(r["shadows"]["strength"], 0.5);
        assert_eq!(r["casting"].as_array().unwrap().len(), 0);
        assert!(s.dispatch("light.shadows", json!({"strength": 2})).is_err());
        // saved with the scene, undoable
        let saved: Scene = serde_json::from_value(serde_json::to_value(s.scene()).unwrap()).unwrap();
        assert_eq!(saved.environment, s.scene().environment);
        s.dispatch("history.undo", json!({})).unwrap();
        assert_eq!(s.scene().environment.shadows.strength, 1.0);
        // default scenes keep their files unchanged
        assert!(serde_json::to_value(Scene::default()).unwrap().get("environment").is_none());
    }

    #[test]
    fn hdri_import() {
        let mut s = Session::default();
        let r = s
            .dispatch("environment.import", json!({"data": hdr_b64(64, 32, (16, 8)), "name": "studio loft"}))
            .unwrap()
            .result;
        assert_eq!(r["hdr"], true);
        let key = r["asset"].as_str().unwrap();
        assert_eq!(s.scene().environment.sky, Sky::Hdri);
        assert_eq!(s.scene().environment.hdri.as_deref(), Some(key));
        assert_eq!(s.scene().assets[key].mime, "image/vnd.radiance");
        // the sun comes from the hot pixel: up and to the left (u = 0.25 is -X)
        let r = s.dispatch("light.fromEnvironment", json!({})).unwrap().result;
        assert!(r["elevation"].as_f64().unwrap() > 20.0, "{r}");
        let sun = s.scene().get(r["id"].as_str().unwrap()).unwrap().transform.translation;
        assert!(sun[0] < -1.0, "{sun:?}");
        // big ones are scaled down
        let r = s
            .dispatch("environment.import", json!({"data": hdr_b64(4096, 2048, (0, 0)), "apply": false}))
            .unwrap()
            .result;
        assert_eq!(r["width"], 2048);
        assert!(s.dispatch("environment.import", json!({"data": "aGVsbG8="})).is_err());
    }
}
