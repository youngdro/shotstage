//! Lighting commands: edit a light, and set up classic lighting around a subject in one step.
//! Lights are optional: a scene without them keeps the flat studio clay light.

use glam::{Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::camera::look_rotation;
use shotstage_core::eval::world_at;
use shotstage_core::spatial;
use shotstage_core::{Entity, EntityKind, LightData, LightKind, Scene, Transform};

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
    /// Two big softboxes left and right plus one overhead: even, glossy product light.
    ProductSoftbox,
    /// Strong light from behind for a glowing outline, with a little front fill.
    Backlight,
    /// Warm low sun from the side.
    GoldenHour,
    /// High sun.
    Daylight,
    /// Remove every light: back to the flat studio clay light.
    Clear,
}

pub const PRESETS: &[(&str, &str)] = &[
    ("threePoint", "Key, fill and rim light"),
    ("rembrandt", "One high side key light, moody"),
    ("productSoftbox", "Two side softboxes and one overhead: even product light"),
    ("backlight", "Rim light from behind, little front fill"),
    ("goldenHour", "Warm low sun from the side"),
    ("daylight", "High neutral sun"),
    ("clear", "Remove all lights (studio clay light)"),
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
    let area = |i: f32, s: f32| LightData { kind: LightKind::Area, intensity: i, size: [s, s], ..Default::default() };
    let spot =
        |i: f32, cone: f32| LightData { kind: LightKind::Spot, intensity: i, cone_degrees: cone, ..Default::default() };
    let sun = |i: f32, c: [f32; 3]| LightData { kind: LightKind::Sun, intensity: i, color: c, ..Default::default() };
    // (name, azimuth from the camera side, elevation, light); positive azimuth = camera left
    let rig: Vec<(&str, f32, f32, LightData)> = match p.preset {
        LightPreset::ThreePoint => vec![
            ("key", 45.0, 30.0, area(1.3, size.max(0.4))),
            ("fill", -55.0, 15.0, area(0.45, size.max(0.4))),
            ("rim", 160.0, 40.0, spot(1.2, 40.0)),
        ],
        LightPreset::Rembrandt => {
            vec![("key", 50.0, 45.0, spot(1.6, 50.0)), ("fill", -60.0, 10.0, area(0.15, size.max(0.4)))]
        }
        LightPreset::ProductSoftbox => vec![
            ("left", 75.0, 20.0, area(1.0, size.max(0.3) * 1.5)),
            ("right", -75.0, 20.0, area(1.0, size.max(0.3) * 1.5)),
            ("top", 0.0, 80.0, area(0.6, size.max(0.3) * 1.5)),
        ],
        LightPreset::Backlight => vec![
            ("rim", 180.0, 30.0, spot(1.8, 45.0)),
            ("rim_2", 150.0, 25.0, spot(0.8, 45.0)),
            ("fill", 0.0, 10.0, area(0.25, size.max(0.4))),
        ],
        LightPreset::GoldenHour => {
            vec![("sun", 80.0, 12.0, sun(1.2, warm)), ("sky", -100.0, 60.0, sun(0.25, cool))]
        }
        LightPreset::Daylight => {
            vec![("sun", 30.0, 55.0, sun(1.0, [1.0, 0.97, 0.92])), ("sky", -150.0, 70.0, sun(0.3, cool))]
        }
        LightPreset::Clear => unreachable!(),
    };
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
    Ok(json!({ "group": group, "lights": ids }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("light.set", "Change a light: kind (sun, point, spot, area), colour, intensity, cone angle, softness, panel size.", mutates: true, LightSetParams, light_set),
        command!("light.preset", "Light a subject in one step: threePoint, rembrandt, productSoftbox, backlight, goldenHour, daylight, or clear (back to the flat studio light). Lights are placed for the active camera.", mutates: true, PresetParams, light_preset),
        command!("light.list", "Lighting presets for light.preset, and the scene's lights.", mutates: false, crate::builtin::NoParams,
        |s, _| Ok(json!({
            "presets": PRESETS.iter().map(|(k, d)| json!({ "preset": k, "description": d })).collect::<Vec<_>>(),
            "lights": s.entities.iter().filter_map(|e| match &e.kind {
                EntityKind::Light(l) => Some(json!({ "id": e.id, "name": e.name, "light": l, "visible": e.visible })),
                _ => None,
            }).collect::<Vec<_>>(),
            "note": "Without visible lights the clay pass uses a flat studio light. Lights shine along their -Z; aim them with camera.lookAt. Intensity 1 = a normal key light 2 m away.",
        }))),
    ]
}
