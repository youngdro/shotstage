//! Lighting commands: edit a light, and set up classic lighting around a subject in one step.
//! Lights are optional: a scene without them keeps the flat studio clay light.

use glam::{Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::camera::look_rotation;
use shotstage_core::eval::world_at;
use shotstage_core::spatial;
use shotstage_core::{Entity, EntityKind, Environment, LightData, LightKind, Scene, Sky, Transform};

use crate::builtin::{color, frame_ok};
use crate::{CmdResult, Command, CommandError, command};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LightSetParams {
    id: String,
    kind: Option<LightKind>,
    /// Linear RGB 0..1.
    color: Option<[f32; 3]>,
    /// 1 = a normal key light (2 m away for point, spot and area lights).
    intensity: Option<f32>,
    /// Spot: full cone angle in degrees.
    cone_degrees: Option<f32>,
    /// Spot and area: edge softness 0..1.
    softness: Option<f32>,
    /// Area: width and height in metres.
    size: Option<[f32; 2]>,
    /// Set the colour from a colour temperature in Kelvin: 2700 warm bulb, 3200 tungsten,
    /// 5600 daylight, 7500 shade.
    temperature: Option<f32>,
    /// Whether it casts shadows (up to four lights do at once, the brightest).
    cast_shadows: Option<bool>,
}

fn light_set(scene: &mut Scene, p: LightSetParams) -> CmdResult<Value> {
    let EntityKind::Light(l) = &mut scene.get_mut(&p.id)?.kind else {
        return Err(CommandError::new("failed", format!("entity `{}` is not a light", p.id)));
    };
    let mut n = *l;
    if let Some(k) = p.kind {
        n.kind = k;
    }
    if let Some(c) = p.color {
        n.color = color(c)?;
    }
    if let Some(v) = p.intensity {
        n.intensity = v;
    }
    if let Some(v) = p.cone_degrees {
        n.cone_degrees = v;
    }
    if let Some(v) = p.softness {
        n.softness = v;
    }
    if let Some(v) = p.size {
        n.size = v;
    }
    if let Some(k) = p.temperature {
        if !(k.is_finite() && (1000.0..=20000.0).contains(&k)) {
            return Err(CommandError::invalid("temperature must be 1000..=20000 K"));
        }
        n.color = shotstage_core::light::kelvin_to_rgb(k);
    }
    if let Some(v) = p.cast_shadows {
        n.cast_shadows = v;
    }
    n.validate()?;
    *l = n;
    Ok(serde_json::to_value(n).unwrap_or(Value::Null))
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum LightPreset {
    /// Key (soft, 45° to the side and above), fill (weak, other side) and rim (behind).
    ThreePoint,
    /// One high key light 45° to the side: a triangle of light on the shadow cheek.
    Rembrandt,
    /// Key a little to the side and above, soft fill: the everyday portrait light.
    Loop,
    /// Key high in front (a butterfly shadow under the nose), soft fill from below.
    Butterfly,
    /// Two soft lights in front, above and below the eyes, plus a hair light: beauty light.
    Clamshell,
    /// One hard light from the side: half the face lit, half in shadow.
    Split,
    /// Two big softboxes left and right plus one overhead: even, glossy product light.
    ProductSoftbox,
    /// Bright and even on a white backdrop: catalogue, e-commerce.
    HighKey,
    /// One narrow light and a dark surround: dramatic, mostly shadow.
    LowKey,
    /// Strong light from behind for a glowing outline, with a little front fill.
    Backlight,
    /// A big soft window to one side and a dim overcast sky.
    Window,
    /// Magenta and cyan lights from the sides under a night sky.
    Neon,
    /// One hard spotlight from above in the dark.
    Stage,
    /// Shop counter: bright overhead panels, a cool front fill (fresh produce, deli, retail).
    FreshCounter,
    /// Warm low sun from the side, with a sunset sky.
    GoldenHour,
    /// High sun with a clear blue sky.
    Daylight,
    /// No direct light: a bright cloudy sky lights everything softly.
    Overcast,
    /// Moonlight under a night sky.
    Night,
    /// Remove every light and the sky: back to the flat studio clay light.
    Clear,
}

pub const PRESETS: &[(&str, &str)] = &[
    ("threePoint", "Key, fill and rim light"),
    ("rembrandt", "One high side key light, moody"),
    ("loop", "Everyday portrait: key a little to the side, soft fill"),
    ("butterfly", "Key high in front, soft fill from below: glamour"),
    ("clamshell", "Soft light above and below the face, hair light: beauty"),
    ("split", "Hard side light: half lit, half shadow"),
    ("productSoftbox", "Two side softboxes and one overhead: even product light"),
    ("highKey", "Bright, even, white backdrop: catalogue"),
    ("lowKey", "One narrow light, dark surround: dramatic"),
    ("backlight", "Rim light from behind, little front fill"),
    ("window", "Big soft window light from the side"),
    ("neon", "Magenta and cyan side lights at night"),
    ("stage", "One hard spotlight from above in the dark"),
    ("freshCounter", "Shop counter: overhead panels, cool front fill"),
    ("goldenHour", "Warm low sun with a sunset sky"),
    ("daylight", "High sun with a blue sky"),
    ("overcast", "Soft cloudy sky light, no sun"),
    ("night", "Moonlight under a night sky"),
    ("clear", "Remove all lights and the sky (studio clay light)"),
];

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PresetParams {
    preset: LightPreset,
    /// What to light. Default: the selection's group is not known here, so the whole scene's
    /// bounds are used.
    subject: Option<String>,
    /// The camera the lighting is arranged for (key light to its left, rim behind the
    /// subject). Default: the active camera, else the subject's front.
    camera: Option<String>,
    /// Multiply every light's brightness. Default 1.
    intensity: Option<f32>,
    /// Remove the lights a previous preset made first. Default true.
    replace: Option<bool>,
    /// Also set the sky that goes with the preset (daylight, sunset, a white or black
    /// backdrop...). Default true.
    sky: Option<bool>,
    frame: Option<f32>,
}

/// Root ids of preset light groups.
fn preset_groups(scene: &Scene) -> Vec<String> {
    scene.entities.iter().filter(|e| e.parent.is_none() && e.id.starts_with("lighting")).map(|e| e.id.clone()).collect()
}

fn light_preset(scene: &mut Scene, p: PresetParams) -> CmdResult<Value> {
    let frame = frame_ok(p.frame.unwrap_or(scene.timeline.start as f32))?;
    let gain = p.intensity.unwrap_or(1.0);
    if !(gain.is_finite() && (0.0..=100.0).contains(&gain)) {
        return Err(CommandError::invalid("intensity must be 0..=100"));
    }
    if p.preset == LightPreset::Clear {
        let ids: Vec<String> =
            scene.entities.iter().filter(|e| matches!(e.kind, EntityKind::Light(_))).map(|e| e.id.clone()).collect();
        for id in &ids {
            if scene.contains(id) {
                scene.remove(id)?;
            }
        }
        for g in preset_groups(scene) {
            if scene.children(Some(&g)).next().is_none() {
                scene.remove(&g)?;
            }
        }
        if p.sky.unwrap_or(true) {
            let shadows = scene.environment.shadows;
            scene.environment = Environment { shadows, ..Default::default() };
        }
        return Ok(json!({ "removed": ids }));
    }
    if p.replace.unwrap_or(true) {
        for g in preset_groups(scene) {
            scene.remove(&g)?;
        }
    }
    // what to light, and from where it is seen
    let b = match &p.subject {
        Some(s) => spatial::bounds(scene, s, frame)?,
        None => scene
            .entities
            .iter()
            .filter(|e| e.parent.is_none())
            .try_fold(None, |acc, e| spatial::bounds(scene, &e.id, frame).map(|b| spatial::Aabb::union(acc, b)))?,
    };
    let b = b.ok_or_else(|| CommandError::new("failed", "nothing to light: the scene has no geometry"))?;
    let center = b.center();
    let size = b.size().max_element().max(0.05);
    let cam = p.camera.or_else(|| shotstage_core::eval::camera_id_at(scene, frame));
    let toward_cam = match (&cam, &p.subject) {
        (Some(c), _) => world_at(scene, c, frame)?.w_axis.truncate() - center,
        (None, Some(s)) => world_at(scene, s, frame)?.transform_vector3(Vec3::Z),
        (None, None) => Vec3::Z,
    };
    let flat = Vec3::new(toward_cam.x, 0.0, toward_cam.z).normalize_or_zero();
    let flat = if flat == Vec3::ZERO { Vec3::Z } else { flat };
    // lights stand a few subject-sizes away; brightness is defined at 2 m, so scale it to keep
    // the same look at any distance
    let dist = (size * 2.2).clamp(0.6, 8.0);
    let at = |az: f32, el: f32| {
        let d = Quat::from_rotation_y(az.to_radians()) * flat;
        center + (d * el.to_radians().cos() + Vec3::Y * el.to_radians().sin()) * dist
    };
    let warm = [1.0, 0.82, 0.62];
    let cool = [0.82, 0.88, 1.0];
    let soft = size.max(0.4);
    let area = |i: f32, s: f32| LightData { kind: LightKind::Area, intensity: i, size: [s, s], ..Default::default() };
    let spot =
        |i: f32, cone: f32| LightData { kind: LightKind::Spot, intensity: i, cone_degrees: cone, ..Default::default() };
    let sun = |i: f32, c: [f32; 3]| LightData { kind: LightKind::Sun, intensity: i, color: c, ..Default::default() };
    let tinted = |mut l: LightData, c: [f32; 3]| {
        l.color = c;
        l
    };
    let fill = |mut l: LightData| {
        l.cast_shadows = false;
        l
    };
    // the sky that goes with it: (sky, colour, intensity, show it behind)
    let backdrop = |c: [f32; 3], i: f32| Some((Sky::Color, c, i, true));
    let mut sky: Option<(Sky, [f32; 3], f32, bool)> = None;
    // (name, azimuth from the camera side, elevation, light); positive azimuth = camera left
    let rig: Vec<(&str, f32, f32, LightData)> = match p.preset {
        LightPreset::ThreePoint => vec![
            ("key", 45.0, 30.0, area(1.3, soft)),
            ("fill", -55.0, 15.0, fill(area(0.45, soft))),
            ("rim", 160.0, 40.0, spot(1.2, 40.0)),
        ],
        LightPreset::Rembrandt => {
            vec![("key", 50.0, 45.0, spot(1.6, 50.0)), ("fill", -60.0, 10.0, fill(area(0.15, soft)))]
        }
        LightPreset::Loop => vec![("key", 35.0, 35.0, area(1.3, soft)), ("fill", -40.0, 10.0, fill(area(0.35, soft)))],
        LightPreset::Butterfly => {
            vec![("key", 0.0, 50.0, area(1.4, soft * 0.7)), ("fill", 0.0, -15.0, fill(area(0.2, soft)))]
        }
        LightPreset::Clamshell => vec![
            ("key", 0.0, 40.0, area(1.2, soft)),
            ("fill", 0.0, -20.0, fill(area(0.55, soft))),
            ("hair", 180.0, 55.0, spot(0.8, 40.0)),
        ],
        LightPreset::Split => vec![("key", 90.0, 10.0, spot(1.6, 50.0))],
        LightPreset::ProductSoftbox => vec![
            ("left", 75.0, 20.0, area(1.0, size.max(0.3) * 1.5)),
            ("right", -75.0, 20.0, fill(area(1.0, size.max(0.3) * 1.5))),
            ("top", 0.0, 80.0, fill(area(0.6, size.max(0.3) * 1.5))),
        ],
        LightPreset::HighKey => {
            sky = backdrop([1.0, 1.0, 1.0], 0.5);
            vec![
                ("key", 15.0, 25.0, area(0.75, soft * 2.0)),
                ("left", 70.0, 15.0, fill(area(0.3, soft * 2.0))),
                ("right", -70.0, 15.0, fill(area(0.3, soft * 2.0))),
            ]
        }
        LightPreset::LowKey => {
            sky = backdrop([0.02, 0.02, 0.025], 1.0);
            let mut key = spot(1.8, 35.0);
            key.softness = 0.5;
            vec![("key", 70.0, 30.0, key), ("rim", 160.0, 30.0, fill(spot(0.5, 30.0)))]
        }
        LightPreset::Backlight => vec![
            ("rim", 180.0, 30.0, spot(1.8, 45.0)),
            ("rim_2", 150.0, 25.0, fill(spot(0.8, 45.0))),
            ("fill", 0.0, 10.0, fill(area(0.25, soft))),
        ],
        LightPreset::Window => {
            sky = Some((Sky::Overcast, [1.0; 3], 0.35, false));
            let mut w = tinted(area(1.6, 1.0), [0.9, 0.94, 1.0]);
            w.size = [(size * 1.2).clamp(0.8, 3.0), (size * 1.6).clamp(1.2, 3.0)];
            vec![("window", 80.0, 20.0, w)]
        }
        LightPreset::Neon => {
            sky = Some((Sky::Night, [1.0; 3], 0.6, true));
            vec![
                ("magenta", 75.0, 10.0, tinted(spot(1.6, 60.0), [1.0, 0.15, 0.75])),
                ("cyan", -75.0, 10.0, tinted(spot(1.6, 60.0), [0.1, 0.85, 1.0])),
            ]
        }
        LightPreset::Stage => {
            sky = backdrop([0.0, 0.0, 0.0], 1.0);
            let mut s = spot(2.4, 30.0);
            s.softness = 0.15;
            vec![("spot", 10.0, 65.0, s)]
        }
        LightPreset::FreshCounter => {
            sky = Some((Sky::Color, [0.95, 0.95, 0.92], 0.45, false));
            let panel = |i: f32| {
                let mut l = tinted(area(i, 1.0), shotstage_core::light::kelvin_to_rgb(4500.0));
                l.size = [(size * 1.4).clamp(0.6, 4.0), 0.4];
                l
            };
            vec![
                ("top_front", 0.0, 65.0, panel(0.8)),
                ("top_back", 180.0, 70.0, fill(panel(0.45))),
                ("front", 0.0, 5.0, fill(tinted(area(0.3, soft), cool))),
            ]
        }
        LightPreset::GoldenHour => {
            sky = Some((Sky::Sunset, [1.0; 3], 0.55, true));
            vec![("sun", 80.0, 7.0, sun(1.6, warm))]
        }
        LightPreset::Daylight => {
            sky = Some((Sky::Day, [1.0; 3], 0.4, true));
            vec![("sun", 30.0, 55.0, sun(1.1, [1.0, 0.97, 0.92]))]
        }
        LightPreset::Overcast => {
            sky = Some((Sky::Overcast, [1.0; 3], 1.0, true));
            vec![]
        }
        LightPreset::Night => {
            sky = Some((Sky::Night, [1.0; 3], 2.0, true));
            vec![("moon", -60.0, 35.0, sun(0.7, [0.7, 0.8, 1.0]))]
        }
        LightPreset::Clear => unreachable!(),
    };
    let mut env = Value::Null;
    if let (Some((kind, c, i, bg)), true) = (sky, p.sky.unwrap_or(true)) {
        // turn the sky so its sun stands where the preset's sun is
        let sun_az = rig.iter().find(|(_, _, _, l)| l.kind == LightKind::Sun).map(|(_, az, ..)| *az).unwrap_or(0.0);
        let d = Quat::from_rotation_y(sun_az.to_radians()) * flat;
        let e = &mut scene.environment;
        e.sky = kind;
        e.color = c;
        e.intensity = i;
        e.background = bg;
        e.rotation = (-d.x).atan2(-d.z).to_degrees().rem_euclid(360.0);
        e.validate()?;
        env = serde_json::to_value(&*e).unwrap_or(Value::Null);
    }
    if rig.is_empty() {
        return Ok(json!({ "group": Value::Null, "lights": [], "environment": env }));
    }
    let group = scene.unique_id("lighting");
    scene.insert(Entity {
        id: group.clone(),
        name: format!("lighting: {:?}", p.preset),
        parent: None,
        transform: Transform::default(),
        visible: true,
        locked: false,
        kind: EntityKind::Empty,
    })?;
    let mut ids = vec![];
    for (name, az, el, mut l) in rig {
        let pos = at(az, el);
        if l.kind != LightKind::Sun {
            l.intensity *= (dist / 2.0).powi(2);
        }
        l.intensity = (l.intensity * gain).min(1000.0);
        l.validate()?;
        let rot = look_rotation(pos, center).unwrap_or(Quat::IDENTITY);
        let id = format!("{group}_{name}");
        scene.insert(Entity {
            id: id.clone(),
            name: name.to_string(),
            parent: Some(group.clone()),
            transform: Transform::from_parts(pos, rot, Vec3::ONE),
            visible: true,
            locked: false,
            kind: EntityKind::Light(l),
        })?;
        ids.push(id);
    }
    Ok(json!({ "group": group, "lights": ids, "environment": env }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("light.set", "Change a light: kind (sun, point, spot, area), colour or colour temperature (Kelvin), intensity, cone angle, softness, panel size, whether it casts shadows.", mutates: true, LightSetParams, light_set),
        command!("light.preset", "Light a subject in one step. Portrait: threePoint, rembrandt, loop, butterfly, clamshell, split. Product and shop: productSoftbox, highKey, freshCounter. Mood: lowKey, backlight, window, neon, stage. Outdoor: goldenHour, daylight, overcast, night. clear goes back to the flat studio light. Lights are placed for the active camera; presets that come with a sky (outdoor ones, white or black backdrops) set it too.", mutates: true, PresetParams, light_preset),
        command!("light.list", "Lighting presets for light.preset, and the scene's lights.", mutates: false, crate::builtin::NoParams,
        |s, _| Ok(json!({
            "presets": PRESETS.iter().map(|(k, d)| json!({ "preset": k, "description": d })).collect::<Vec<_>>(),
            "lights": s.entities.iter().filter_map(|e| match &e.kind {
                EntityKind::Light(l) => Some(json!({ "id": e.id, "name": e.name, "light": l, "visible": e.visible })),
                _ => None,
            }).collect::<Vec<_>>(),
            "environment": s.environment,
            "note": "Without visible lights or a sky the clay pass uses a flat studio light. Lights shine along their -Z; aim them with camera.lookAt. Intensity 1 = a normal key light 2 m away. Up to four lights cast shadows (light.shadows); the sky is set with environment.set.",
        }))),
    ]
}
