//! Motion presets: camera moves, object motion and character actions in one command. Every
//! preset writes ordinary keyframes (editable afterwards on the timeline) and is one undo step.

use std::collections::BTreeMap;

use glam::{Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::camera::look_rotation;
use shotstage_core::eval::{camera_data_at, local_at, world_at};
use shotstage_core::spatial;
use shotstage_core::{EntityKind, Interp, Key, KeyValue, Scene, TrackProperty, Transform};

use crate::builtin::finite3;
use crate::rig;
use crate::{CmdResult, Command, CommandError, command};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Camera,
    Object,
    Character,
}

pub struct Preset {
    pub name: &'static str,
    pub label: &'static str,
    pub group: Group,
    pub description: &'static str,
    /// Default length in seconds.
    pub seconds: f32,
}

macro_rules! preset {
    ($name:literal, $label:literal, $group:ident, $secs:literal, $desc:literal) => {
        Preset { name: $name, label: $label, group: Group::$group, seconds: $secs, description: $desc }
    };
}

pub const PRESETS: &[Preset] = &[
    preset!("dollyIn", "Dolly in", Camera, 3.0, "Move the camera forward along its view"),
    preset!("dollyOut", "Dolly out", Camera, 3.0, "Move the camera back along its view"),
    preset!("pushIn", "Push in", Camera, 4.0, "Slow push towards the subject, keeping it centred"),
    preset!(
        "pullBackReveal",
        "Pull-back reveal",
        Camera,
        5.0,
        "Start close on the subject and pull back and up to reveal the scene"
    ),
    preset!("truckLeft", "Truck left", Camera, 3.0, "Slide the camera sideways to the left"),
    preset!("truckRight", "Truck right", Camera, 3.0, "Slide the camera sideways to the right"),
    preset!("pedestalUp", "Pedestal up", Camera, 3.0, "Raise the camera straight up"),
    preset!("pedestalDown", "Pedestal down", Camera, 3.0, "Lower the camera straight down"),
    preset!("panLeft", "Pan left", Camera, 3.0, "Turn the camera to the left on the spot"),
    preset!("panRight", "Pan right", Camera, 3.0, "Turn the camera to the right on the spot"),
    preset!("tiltUp", "Tilt up", Camera, 3.0, "Tilt the camera up on the spot"),
    preset!("tiltDown", "Tilt down", Camera, 3.0, "Tilt the camera down on the spot"),
    preset!("orbitLeft", "Arc left", Camera, 4.0, "Circle around the subject to the left, looking at it"),
    preset!("orbitRight", "Arc right", Camera, 4.0, "Circle around the subject to the right, looking at it"),
    preset!("craneUp", "Crane up", Camera, 4.0, "Rise up while keeping the subject in view"),
    preset!("craneDown", "Crane down", Camera, 4.0, "Come down while keeping the subject in view"),
    preset!(
        "dollyZoom",
        "Dolly zoom",
        Camera,
        3.0,
        "Vertigo: pull back while zooming in, the subject stays the same size"
    ),
    preset!("zoomIn", "Zoom in", Camera, 2.0, "Zoom the lens in (longer focal length)"),
    preset!("zoomOut", "Zoom out", Camera, 2.0, "Zoom the lens out (shorter focal length)"),
    preset!("whipPan", "Whip pan", Camera, 0.5, "A very fast pan"),
    preset!("tracking", "Tracking", Camera, 4.0, "Follow the (animated) subject, keeping the same offset"),
    preset!("handheld", "Handheld", Camera, 4.0, "Add a gentle handheld shake on top of the camera's motion"),
    preset!(
        "droneFlyover",
        "Drone flyover",
        Camera,
        6.0,
        "Fly in low from behind, rise over the subject and look down"
    ),
    preset!("productSpin", "360 spin", Camera, 6.0, "Circle once around the subject at a constant speed"),
    preset!("spiralUp", "Spiral up", Camera, 6.0, "Circle half-way around the subject while rising, looking at it"),
    preset!("chase", "Chase", Camera, 4.0, "Follow behind the (moving) subject, looking where it goes"),
    preset!("topDown", "Top-down", Camera, 4.0, "Rise until straight above the subject, looking down"),
    preset!("turntable", "Turntable", Object, 6.0, "Spin the object once around its vertical axis"),
    preset!("pulse", "Pulse", Object, 1.5, "Beat in size three times, like a heartbeat"),
    preset!("shake", "Shake", Object, 0.8, "A quick nervous shake that dies down"),
    preset!("swing", "Swing", Object, 3.0, "Swing like a hanging sign, settling"),
    preset!("flip", "Flip", Object, 1.0, "Hop and do a full forward flip"),
    preset!("bounce", "Bounce", Object, 2.0, "Hop up and down a few times, settling"),
    preset!("float", "Float", Object, 4.0, "Hover gently up and down with a slight sway"),
    preset!("dropIn", "Drop in", Object, 1.2, "Fall into place from above with a small bounce"),
    preset!("slideIn", "Slide in", Object, 1.0, "Slide into place from the side"),
    preset!("popIn", "Pop in", Object, 0.6, "Grow from nothing with a springy overshoot"),
    preset!("wobble", "Wobble", Object, 1.5, "Rock from side to side, settling"),
    preset!("heroReveal", "Hero reveal", Object, 2.5, "Rise up, turn to the front and grow into place"),
    preset!("pathFollow", "Follow path", Object, 4.0, "Move through `points`, facing the way it goes"),
    preset!("walkCycle", "Walk", Character, 4.0, "Walk forward with a stepping cycle"),
    preset!("runCycle", "Run", Character, 3.0, "Run forward with long strides"),
    preset!("wave", "Wave", Character, 2.0, "Raise the right hand and wave"),
    preset!("pointAt", "Point", Character, 2.0, "Raise the right arm, point ahead, lower it again"),
    preset!("talk", "Talk", Character, 4.0, "Small hand gestures and nods, as if talking"),
    preset!("pickUp", "Pick up", Character, 2.5, "Bend down to pick something up and stand again"),
    preset!("turnHead", "Look around", Character, 3.0, "Turn the head left and right"),
    preset!("nod", "Nod", Character, 1.2, "Nod twice"),
    preset!("sitDown", "Sit down", Character, 1.5, "Sit down from standing"),
    preset!("standUp", "Stand up", Character, 1.5, "Stand up from sitting"),
];

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MotionParams {
    /// Preset name, see `motion.list`.
    preset: String,
    /// What to animate. Camera moves: a camera (default: the active camera). Object motion: any
    /// entity. Character actions: a person (root, joint or part).
    #[serde(alias = "id")]
    target: Option<String>,
    /// What camera moves aim at, orbit and track (default: what the camera looks at).
    subject: Option<String>,
    /// First frame. Default: the timeline start.
    start: Option<f32>,
    /// Length in seconds. Default: the preset's.
    duration: Option<f32>,
    /// How far / how strong, 0.1..=5. Default 1.
    intensity: Option<f32>,
    /// Easing of the move: ease (default), linear, easeIn, easeOut, overshoot.
    ease: Option<Interp>,
    /// pathFollow: world points to pass through.
    points: Option<Vec<[f32; 3]>>,
}

/// Keys written into one transform track (frame, value, interp).
type Keys = Vec<(f32, Transform, Interp)>;

fn set_keys(scene: &mut Scene, id: &str, start: f32, end: f32, keys: Keys) {
    let track = scene.timeline.track_mut_or_insert(id, TrackProperty::Transform);
    track.keys.retain(|k| k.frame < start - 1e-3 || k.frame > end + 1e-3);
    for (f, t, interp) in keys {
        track.set_key(Key { frame: f, value: KeyValue::Transform(t), interp });
    }
}

fn set_focal(scene: &mut Scene, id: &str, start: f32, end: f32, keys: Vec<(f32, f32, Interp)>) {
    let track = scene.timeline.track_mut_or_insert(id, TrackProperty::FocalLength);
    track.keys.retain(|k| k.frame < start - 1e-3 || k.frame > end + 1e-3);
    for (f, v, interp) in keys {
        track.set_key(Key { frame: f, value: KeyValue::Scalar(v.clamp(4.0, 1200.0)), interp });
    }
}

/// A world-space pose for `id` as its local transform (parents taken into account).
fn local_from_world(scene: &Scene, id: &str, frame: f32, pos: Vec3, rot: Quat, scale: Vec3) -> CmdResult<Transform> {
    let parent = match scene.get(id)?.parent.clone() {
        Some(p) => world_at(scene, &p, frame)?,
        None => Mat4::IDENTITY,
    };
    let m = parent.inverse() * Mat4::from_scale_rotation_translation(scale, rot, pos);
    let (s, r, t) = m.to_scale_rotation_translation();
    Ok(Transform::from_parts(t, r, s))
}

/// `n + 1` samples of `f(s)` over the range, `s` eased 0..1, linearly keyed in between.
#[allow(clippy::too_many_arguments)]
fn sampled(
    scene: &Scene,
    id: &str,
    start: f32,
    end: f32,
    n: usize,
    ease: Interp,
    f: impl Fn(f32) -> (Vec3, Quat),
    scale: Vec3,
) -> CmdResult<Keys> {
    let n = n.max(1);
    (0..=n)
        .map(|i| {
            let u = i as f32 / n as f32;
            let frame = start + (end - start) * u;
            let (p, r) = f(ease.apply(u));
            Ok((frame, local_from_world(scene, id, frame, p, r, scale)?, Interp::Linear))
        })
        .collect()
}

fn look(eye: Vec3, at: Vec3, fallback: Quat) -> Quat {
    look_rotation(eye, at).unwrap_or(fallback)
}

/// Smooth pseudo-random wobble in -1..1 (a few sines), deterministic.
fn shake(t: f32, seed: f32) -> f32 {
    ((t * 1.7 + seed).sin() * 0.5 + (t * 3.1 + seed * 2.3).sin() * 0.3 + (t * 7.3 + seed * 5.1).sin() * 0.2)
        .clamp(-1.0, 1.0)
}

fn apply(scene: &mut Scene, p: MotionParams) -> CmdResult<Value> {
    let preset = PRESETS.iter().find(|q| q.name == p.preset).ok_or_else(|| {
        CommandError::invalid(format!(
            "no preset `{}`; use one of {}",
            p.preset,
            PRESETS.iter().map(|q| q.name).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let fps = scene.timeline.fps;
    let start = p.start.unwrap_or(scene.timeline.start as f32);
    let secs = p.duration.unwrap_or(preset.seconds);
    let k = p.intensity.unwrap_or(1.0);
    if !(start.is_finite() && (0.0..=1_000_000.0).contains(&start)) {
        return Err(CommandError::invalid("start must be a frame >= 0"));
    }
    if !(secs.is_finite() && (0.05..=600.0).contains(&secs)) {
        return Err(CommandError::invalid("duration must be 0.05..=600 seconds"));
    }
    if !(k.is_finite() && (0.1..=5.0).contains(&k)) {
        return Err(CommandError::invalid("intensity must be 0.1..=5"));
    }
    let end = (start + secs * fps).round().max(start + 1.0);
    let ease = p.ease.unwrap_or(Interp::Ease);
    let frames = (end - start).max(1.0);
    // the timeline grows to fit the move
    if end > scene.timeline.end as f32 {
        scene.timeline.end = end.ceil() as u32;
    }

    let target = match (&p.target, preset.group) {
        (Some(t), Group::Character) => rig::person_root(scene, t)?,
        (Some(t), _) => {
            scene.get(t)?;
            t.clone()
        }
        (None, Group::Camera) => shotstage_core::eval::camera_id_at(scene, start)
            .ok_or_else(|| CommandError::new("failed", "no camera: give `target` or add a camera"))?,
        (None, _) => return Err(CommandError::invalid("give the `target` to animate")),
    };
    if preset.group == Group::Camera && !matches!(scene.get(&target)?.kind, EntityKind::Camera(_)) {
        return Err(CommandError::invalid(format!("`{}` moves a camera; `{target}` is not one", preset.name)));
    }

    let w0 = world_at(scene, &target, start)?;
    let (scale0, rot0, pos0) = w0.to_scale_rotation_translation();
    let fwd = (rot0 * Vec3::NEG_Z).normalize_or_zero();
    let right = (rot0 * Vec3::X).normalize_or_zero();
    // what a camera move is about: the subject's centre, or the point it looks at
    let subject = match &p.subject {
        Some(s) => {
            let b = spatial::bounds(scene, s, start)?;
            Some((
                b.map(|b| b.center()).unwrap_or(world_at(scene, s, start)?.w_axis.truncate()),
                b.map(|b| b.size().max_element()).unwrap_or(1.0),
            ))
        }
        None => None,
    };
    let (center, size) = subject.unwrap_or_else(|| {
        // what the camera looks at: the first surface ahead, else 4 m ahead
        let hit = spatial::raycast(
            scene,
            start,
            pos0,
            fwd,
            &spatial::RayOptions { ignore: std::slice::from_ref(&target), ground: true, ..Default::default() },
        );
        (hit.map(|h| Vec3::from_array(h.point)).unwrap_or(pos0 + fwd * 4.0), 1.0)
    });
    // A camera already pointed at the subject (framed with camera.frame, say) keeps its framing:
    // moves aim at the point it looks at, level with the subject, not at the subject's centre
    // (which would tilt a medium shot of a person down to the waist on the first frame).
    let center = {
        let along = (center - pos0).dot(fwd);
        let on_axis = pos0 + fwd * along;
        let off = (center - pos0).normalize_or_zero().dot(fwd);
        if subject.is_some() && along > 0.1 && off > 25f32.to_radians().cos() { on_axis } else { center }
    };
    let dist = (center - pos0).length().max(0.3);
    let step = (dist * 0.35).clamp(0.3, 4.0) * k;
    let dense = ((frames / 3.0).ceil() as usize).clamp(4, 240);
    let mut written = 0usize;

    let mut keys: Option<Keys> = None;
    let mut focal: Option<Vec<(f32, f32, Interp)>> = None;
    let line = |a: Vec3, b: Vec3, ra: Quat, rb: Quat| -> CmdResult<Keys> {
        Ok(vec![
            (start, local_from_world(scene, &target, start, a, ra, scale0)?, ease),
            (end, local_from_world(scene, &target, end, b, rb, scale0)?, ease),
        ])
    };
    match preset.name {
        "dollyIn" => keys = Some(line(pos0, pos0 + fwd * step.min(dist * 0.9), rot0, rot0)?),
        "dollyOut" => keys = Some(line(pos0, pos0 - fwd * step, rot0, rot0)?),
        "truckLeft" => keys = Some(line(pos0, pos0 - right * step, rot0, rot0)?),
        "truckRight" => keys = Some(line(pos0, pos0 + right * step, rot0, rot0)?),
        "pedestalUp" => keys = Some(line(pos0, pos0 + Vec3::Y * step * 0.6, rot0, rot0)?),
        "pedestalDown" => {
            keys = Some(line(pos0, pos0 - Vec3::Y * (step * 0.6).min(pos0.y - 0.1).max(0.0), rot0, rot0)?)
        }
        "panLeft" | "panRight" | "whipPan" => {
            let sign = if preset.name == "panRight" { -1.0 } else { 1.0 };
            let deg = if preset.name == "whipPan" { 90.0 } else { 30.0 } * k * sign;
            let r1 = Quat::from_rotation_y(deg.to_radians()) * rot0;
            keys = Some(line(pos0, pos0, rot0, r1)?);
        }
        "tiltUp" | "tiltDown" => {
            let deg = 20.0 * k * if preset.name == "tiltUp" { 1.0 } else { -1.0 };
            let r1 = Quat::from_axis_angle(right, deg.to_radians()) * rot0;
            keys = Some(line(pos0, pos0, rot0, r1)?);
        }
        "pushIn" => {
            let to = (center - pos0).normalize_or_zero();
            let d = (dist * 0.4 * k).min(dist * 0.85);
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                8,
                ease,
                |s| {
                    let e = pos0 + to * d * s;
                    (e, look(e, center, rot0))
                },
                scale0,
            )?);
        }
        "pullBackReveal" => {
            let to = (center - pos0).normalize_or_zero();
            let close = pos0 + to * dist * 0.6;
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                dense.min(24),
                Interp::EaseOut,
                |s| {
                    let e = close.lerp(pos0 - to * dist * 0.4 * k + Vec3::Y * dist * 0.25 * k, s);
                    (e, look(e, center, rot0))
                },
                scale0,
            )?);
        }
        "orbitLeft" | "orbitRight" | "productSpin" => {
            let deg = match preset.name {
                "productSpin" => 360.0 * k.round().max(1.0),
                "orbitLeft" => -60.0 * k,
                _ => 60.0 * k,
            };
            let off = pos0 - center;
            let ease = if preset.name == "productSpin" { Interp::Linear } else { ease };
            let n = ((deg.abs() / 10.0).ceil() as usize).max(6);
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                n,
                ease,
                |s| {
                    let e = center + Quat::from_rotation_y((deg * s).to_radians()) * off;
                    (e, look(e, center, rot0))
                },
                scale0,
            )?);
        }
        "craneUp" | "craneDown" => {
            let up = if preset.name == "craneUp" { 1.0 } else { -1.0 } * (dist * 0.5 * k).clamp(0.5, 6.0);
            let up = if up < 0.0 { up.max(-(pos0.y - 0.2)) } else { up };
            let back = -(center - pos0).normalize_or_zero() * dist * 0.15 * k;
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                10,
                ease,
                |s| {
                    let e = pos0 + (Vec3::Y * up + back) * s;
                    (e, look(e, center, rot0))
                },
                scale0,
            )?);
        }
        "dollyZoom" => {
            let f0 = camera_data_at(scene, &target, start)?.focal_length_mm;
            let to = (center - pos0).normalize_or_zero();
            let d1 = dist * (1.0 + 0.8 * k);
            let n = 12;
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                n,
                ease,
                |s| {
                    let e = center - to * (dist + (d1 - dist) * s);
                    (e, look(e, center, rot0))
                },
                scale0,
            )?);
            // focal length grows with the distance so the subject keeps its size
            focal = Some(
                (0..=n)
                    .map(|i| {
                        let u = i as f32 / n as f32;
                        let s = ease.apply(u);
                        (start + frames * u, f0 * (dist + (d1 - dist) * s) / dist, Interp::Linear)
                    })
                    .collect(),
            );
        }
        "zoomIn" | "zoomOut" => {
            let f0 = camera_data_at(scene, &target, start)?.focal_length_mm;
            let f1 = if preset.name == "zoomIn" { f0 * (1.0 + k) } else { f0 / (1.0 + k * 0.5) };
            focal = Some(vec![(start, f0, ease), (end, f1, ease)]);
        }
        "tracking" => {
            let s0 = match &p.subject {
                Some(s) => world_at(scene, s, start)?.w_axis.truncate(),
                None => return Err(CommandError::invalid("tracking needs a `subject` to follow")),
            };
            let sub = p.subject.clone().expect("checked");
            let off = pos0 - s0;
            let n = dense;
            let mut ks = vec![];
            for i in 0..=n {
                let f = start + frames * i as f32 / n as f32;
                let sp = world_at(scene, &sub, f)?.w_axis.truncate();
                let lookat = spatial::bounds(scene, &sub, f)?.map(|b| b.center()).unwrap_or(sp);
                let e = sp + off;
                ks.push((f, local_from_world(scene, &target, f, e, look(e, lookat, rot0), scale0)?, Interp::Linear));
            }
            keys = Some(ks);
        }
        "handheld" => {
            // layered on whatever the camera already does
            let n = ((frames / 2.0).ceil() as usize).clamp(2, 1000);
            let mut ks = vec![];
            for i in 0..=n {
                let f = start + frames * i as f32 / n as f32;
                let base = local_at(scene, &target, f)?;
                let t = f / fps;
                let jitter = Vec3::new(shake(t, 1.0), shake(t, 2.0), shake(t, 3.0)) * 0.012 * k;
                let rot = Quat::from_euler(
                    glam::EulerRot::YXZ,
                    (shake(t, 4.0) * 0.6 * k).to_radians(),
                    (shake(t, 5.0) * 0.5 * k).to_radians(),
                    (shake(t, 6.0) * 0.4 * k).to_radians(),
                );
                ks.push((
                    f,
                    Transform::from_parts(base.translation() + jitter, base.rotation() * rot, base.scale()),
                    Interp::Linear,
                ));
            }
            keys = Some(ks);
        }
        "spiralUp" => {
            let off = pos0 - center;
            let rise = (dist * 0.7 * k).clamp(0.5, 8.0);
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                24,
                ease,
                |s| {
                    let e = center
                        + Quat::from_rotation_y((180.0 * k.min(2.0) * s).to_radians()) * off
                        + Vec3::Y * rise * s;
                    (e, look(e, center, rot0))
                },
                scale0,
            )?);
        }
        "topDown" => {
            let above = Vec3::new(center.x, center.y + dist.max(1.5), center.z)
                + (pos0 - center).with_y(0.0).normalize_or_zero() * 0.05;
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                12,
                ease,
                |s| {
                    let e = pos0.lerp(above, s);
                    (e, look(e, center, rot0))
                },
                scale0,
            )?);
        }
        "chase" => {
            let sub = p.subject.clone().ok_or_else(|| CommandError::invalid("chase needs a `subject` to follow"))?;
            let s0 = world_at(scene, &sub, start)?.w_axis.truncate();
            let back = (pos0 - s0).with_y(0.0).length().clamp(1.5, 8.0);
            let height = (pos0.y - s0.y).clamp(0.3, 4.0);
            let mut ks = vec![];
            for i in 0..=dense {
                let f = start + frames * i as f32 / dense as f32;
                let w = world_at(scene, &sub, f)?;
                let sp = w.w_axis.truncate();
                let face = w.transform_vector3(Vec3::Z).with_y(0.0).normalize_or_zero();
                let face = if face == Vec3::ZERO { Vec3::Z } else { face };
                let e = sp - face * back + Vec3::Y * height;
                ks.push((
                    f,
                    local_from_world(
                        scene,
                        &target,
                        f,
                        e,
                        look(e, sp + face * 2.0 + Vec3::Y * height * 0.5, rot0),
                        scale0,
                    )?,
                    Interp::Linear,
                ));
            }
            keys = Some(ks);
        }
        "droneFlyover" => {
            let flat = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
            let flat = if flat == Vec3::ZERO { Vec3::NEG_Z } else { flat };
            let r = (size * 3.0).max(6.0) * k;
            let a = center - flat * r + Vec3::Y * (r * 0.15).max(1.0);
            let b = center + flat * r * 0.4 + Vec3::Y * r * 0.8;
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                24,
                ease,
                |s| {
                    // a gentle arc rather than a straight line
                    let e = a.lerp(b, s) + Vec3::Y * (s * std::f32::consts::PI).sin() * r * 0.15;
                    (e, look(e, center.lerp(center + flat * r * 0.3, s), rot0))
                },
                scale0,
            )?);
        }
        // ---------------------------------------------------------------- objects
        "turntable" => {
            let turns = k.round().max(1.0);
            let n = (12.0 * turns) as usize;
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                n,
                Interp::Linear,
                |s| (pos0, Quat::from_rotation_y((360.0 * turns * s).to_radians()) * rot0),
                scale0,
            )?);
        }
        "bounce" | "float" | "wobble" | "dropIn" => {
            let h =
                spatial::bounds(scene, &target, start)?.map(|b| b.size().max_element()).unwrap_or(1.0).clamp(0.05, 3.0);
            let n = ((frames / 2.0).ceil() as usize).clamp(8, 400);
            let name = preset.name;
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                n,
                Interp::Linear,
                |s| {
                    match name {
                        "bounce" => {
                            let hops = 3.0;
                            let y = (s * hops * std::f32::consts::PI).sin().abs() * h * 0.5 * k * (1.0 - s).powf(0.8);
                            (pos0 + Vec3::Y * y, rot0)
                        }
                        "float" => {
                            let y = (s * std::f32::consts::TAU * 2.0).sin() * h * 0.08 * k + h * 0.08 * k;
                            let sway =
                                Quat::from_rotation_z(((s * std::f32::consts::TAU).sin() * 3.0 * k).to_radians());
                            (pos0 + Vec3::Y * y, rot0 * sway)
                        }
                        "wobble" => {
                            let a = (s * std::f32::consts::TAU * 3.0).sin() * 12.0 * k * (1.0 - s);
                            (pos0, rot0 * Quat::from_rotation_z(a.to_radians()))
                        }
                        _ => {
                            // fall (accelerating), then a small hop
                            let fall = 1.5 * k.max(0.3);
                            let y = if s < 0.7 {
                                let u = s / 0.7;
                                fall * (1.0 - u * u)
                            } else {
                                let u = (s - 0.7) / 0.3;
                                (u * std::f32::consts::PI).sin() * fall * 0.08
                            };
                            (pos0 + Vec3::Y * y, rot0)
                        }
                    }
                },
                scale0,
            )?);
        }
        "pulse" | "shake" | "swing" | "flip" => {
            let h =
                spatial::bounds(scene, &target, start)?.map(|b| b.size().max_element()).unwrap_or(1.0).clamp(0.05, 3.0);
            let n = ((frames / 1.5).ceil() as usize).clamp(8, 400);
            let name = preset.name;
            let mut ks = vec![];
            for i in 0..=n {
                let u = i as f32 / n as f32;
                let f = start + frames * u;
                let (p, r, sc) = match name {
                    "pulse" => {
                        let beat = (u * 3.0 * std::f32::consts::PI).sin().abs().powf(3.0);
                        (pos0, rot0, scale0 * (1.0 + 0.12 * k * beat))
                    }
                    "shake" => {
                        let a = (u * 40.0).sin() * 0.03 * k * h.max(0.3) * (1.0 - u).powi(2);
                        (pos0 + right * a, rot0, scale0)
                    }
                    "swing" => {
                        let a = (u * std::f32::consts::TAU * 2.5).sin() * 25.0 * k * (1.0 - u);
                        (pos0, rot0 * Quat::from_rotation_z(a.to_radians()), scale0)
                    }
                    _ => {
                        let hop = (u * std::f32::consts::PI).sin() * h * 0.8 * k;
                        let spin = Interp::Ease.apply(u) * 360.0;
                        (pos0 + Vec3::Y * hop, Quat::from_axis_angle(right, -spin.to_radians()) * rot0, scale0)
                    }
                };
                ks.push((f, local_from_world(scene, &target, f, p, r, sc)?, Interp::Linear));
            }
            keys = Some(ks);
        }
        "slideIn" => {
            let from = pos0 - right * (2.0 * k);
            keys = Some(line(from, pos0, rot0, rot0)?);
            if let Some(ks) = keys.as_mut() {
                ks[0].2 = Interp::EaseOut;
            }
        }
        "popIn" => {
            let n = 10;
            let mut ks = vec![];
            for i in 0..=n {
                let u = i as f32 / n as f32;
                let s = Interp::Overshoot.apply(u).max(0.001);
                let f = start + frames * u;
                ks.push((f, local_from_world(scene, &target, f, pos0, rot0, scale0 * s)?, Interp::Linear));
            }
            keys = Some(ks);
        }
        "heroReveal" => {
            let h = spatial::bounds(scene, &target, start)?.map(|b| b.size().y).unwrap_or(1.0);
            let n = 16;
            let mut ks = vec![];
            for i in 0..=n {
                let u = i as f32 / n as f32;
                let s = Interp::EaseOut.apply(u);
                let f = start + frames * u;
                let p = pos0 - Vec3::Y * h * 0.4 * k * (1.0 - s);
                let r = Quat::from_rotation_y((-120.0 * k * (1.0 - s)).to_radians()) * rot0;
                ks.push((f, local_from_world(scene, &target, f, p, r, scale0 * (0.6 + 0.4 * s))?, Interp::Linear));
            }
            keys = Some(ks);
        }
        "pathFollow" => {
            let pts: Vec<Vec3> = p
                .points
                .as_ref()
                .ok_or_else(|| CommandError::invalid("pathFollow needs `points`"))?
                .iter()
                .map(|v| finite3(*v, "points"))
                .collect::<CmdResult<_>>()?;
            if pts.len() < 2 {
                return Err(CommandError::invalid("pathFollow needs at least two points"));
            }
            let mut all = vec![pos0];
            all.extend(pts);
            // Catmull-Rom through the points, facing along the path
            let at = |s: f32| -> Vec3 {
                let seg = (all.len() - 1) as f32;
                let x = (s * seg).min(seg - 1e-4);
                let i = x.floor() as usize;
                let t = x - i as f32;
                let p = |j: isize| all[(j.clamp(0, all.len() as isize - 1)) as usize];
                let (p0, p1, p2, p3) = (p(i as isize - 1), p(i as isize), p(i as isize + 1), p(i as isize + 2));
                0.5 * ((2.0 * p1)
                    + (-p0 + p2) * t
                    + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
                    + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t)
            };
            keys = Some(sampled(
                scene,
                &target,
                start,
                end,
                dense.max(all.len() * 6),
                ease,
                |s| {
                    let e = at(s);
                    let ahead = at((s + 0.01).min(1.0)) - e;
                    let flat = Vec3::new(ahead.x, 0.0, ahead.z);
                    let r =
                        if flat.length_squared() > 1e-8 { Quat::from_rotation_y(flat.x.atan2(flat.z)) } else { rot0 };
                    (e, r)
                },
                scale0,
            )?);
        }
        _ => {}
    }

    if let Some(ks) = keys {
        written += ks.len();
        set_keys(scene, &target, start, end, ks);
    }
    if let Some(fk) = focal {
        written += fk.len();
        set_focal(scene, &target, start, end, fk);
    }
    if preset.group == Group::Character {
        written += character(scene, &target, preset.name, start, end, k, fps)?;
    }
    Ok(json!({ "target": target, "preset": preset.name, "start": start, "end": end, "keys": written }))
}

/// Joint angles of a named pose (missing joints straight).
fn pose_map(name: &str) -> BTreeMap<&'static str, [f32; 3]> {
    let mut m: BTreeMap<&'static str, [f32; 3]> = rig::JOINTS.iter().map(|j| (*j, [0.0; 3])).collect();
    if let Some(p) = rig::POSES.iter().find(|p| p.name == name) {
        for (j, r) in p.joints {
            m.insert(j, *r);
        }
    }
    m
}

fn mirrored(m: &BTreeMap<&'static str, [f32; 3]>) -> BTreeMap<&'static str, [f32; 3]> {
    m.iter()
        .map(|(j, r)| {
            let other = j
                .strip_suffix("_l")
                .map(|b| format!("{b}_r"))
                .or_else(|| j.strip_suffix("_r").map(|b| format!("{b}_l")));
            let key = other.and_then(|o| rig::JOINTS.iter().find(|x| **x == o).copied()).unwrap_or(j);
            (key, [r[0], -r[1], -r[2]])
        })
        .collect()
}

fn pelvis_drop(name: &str) -> f32 {
    rig::POSES.iter().find(|p| p.name == name).map(|p| p.pelvis_drop).unwrap_or(0.0)
}

/// Key a whole pose (every joint, plus the pelvis height) at `frame`.
fn key_pose(
    scene: &mut Scene,
    root: &str,
    frame: f32,
    pose: &BTreeMap<&'static str, [f32; 3]>,
    drop: f32,
    interp: Interp,
) -> CmdResult<usize> {
    let rest = rig::pelvis_rest(scene, root)?;
    let k = rest / 0.95;
    let mut n = 0;
    for (j, r) in pose {
        let id = format!("{root}_{j}");
        let mut t = scene.get(&id)?.transform;
        t.set_euler_degrees(*r);
        if *j == "pelvis" {
            t.translation = [0.0, rest - drop * k, 0.0];
        }
        scene.timeline.track_mut_or_insert(&id, TrackProperty::Transform).set_key(Key {
            frame,
            value: KeyValue::Transform(t),
            interp,
        });
        n += 1;
    }
    Ok(n)
}

fn character(scene: &mut Scene, root: &str, name: &str, start: f32, end: f32, k: f32, fps: f32) -> CmdResult<usize> {
    // Upper-body actions (wave, point, talk, look, nod) start from the current pose, so a seated
    // person keeps sitting; whole-body actions use full poses.
    let rest = rig::pelvis_rest(scene, root)?;
    let kk = (rest / 0.95).max(1e-3);
    let mut current: BTreeMap<&'static str, [f32; 3]> = BTreeMap::new();
    for j in rig::JOINTS {
        current.insert(j, local_at(scene, &format!("{root}_{j}"), start)?.euler_degrees());
    }
    let cur_drop = (rest - local_at(scene, &format!("{root}_pelvis"), start)?.translation[1]) / kk;
    let upper = |base: &BTreeMap<&'static str, [f32; 3]>, pose: &str| {
        let mut m = base.clone();
        for (j, r) in pose_map(pose) {
            if j.starts_with("shoulder")
                || j.starts_with("elbow")
                || j.starts_with("wrist")
                || j == "neck"
                || j == "head"
            {
                m.insert(j, r);
            }
        }
        m
    };
    for j in rig::JOINTS {
        let id = format!("{root}_{j}");
        if let Some(t) =
            scene.timeline.tracks.iter_mut().find(|t| t.entity == id && t.property == TrackProperty::Transform)
        {
            t.keys.retain(|key| key.frame < start - 1e-3 || key.frame > end + 1e-3);
        }
    }
    let stand = current.clone();
    let frames = end - start;
    let mut n = 0;
    match name {
        "walkCycle" | "runCycle" => {
            let run = name == "runCycle";
            let walk = pose_map(if run { "run" } else { "walk" });
            let other = mirrored(&walk);
            let step_frames = ((if run { 0.33 } else { 0.5 }) * fps / k.clamp(0.5, 2.0)).max(2.0);
            let steps = (frames / step_frames).floor().max(1.0) as usize;
            for i in 0..=steps {
                let f = (start + i as f32 * step_frames).min(end);
                let pose = if i % 2 == 0 { &walk } else { &other };
                n += key_pose(scene, root, f, pose, pelvis_drop(if run { "run" } else { "walk" }), Interp::Ease)?;
            }
            // move forward along the person's front, 0.7 m per step
            let w = world_at(scene, root, start)?;
            let (s, r, t) = w.to_scale_rotation_translation();
            let front = r * Vec3::Z;
            let front = Vec3::new(front.x, 0.0, front.z).normalize_or_zero();
            let dist = if run { 1.4 } else { 0.7 } * steps as f32 * s.x;
            let last = (start + steps as f32 * step_frames).min(end);
            let a = local_from_world(scene, root, start, t, r, s)?;
            let b = local_from_world(scene, root, last, t + front * dist, r, s)?;
            set_keys(scene, root, start, last, vec![(start, a, Interp::Linear), (last, b, Interp::Linear)]);
            n += 2;
        }
        "wave" => {
            let wave = upper(&stand, "wave");
            n += key_pose(scene, root, start, &stand, cur_drop, Interp::Ease)?;
            let up = start + frames * 0.2;
            n += key_pose(scene, root, up, &wave, cur_drop, Interp::Ease)?;
            let waves = (3.0 * k).round().max(1.0) as usize;
            for i in 1..=waves * 2 {
                let f = up + (end - up - frames * 0.15) * i as f32 / (waves * 2) as f32;
                let mut p = wave.clone();
                let swing = if i % 2 == 1 { 25.0 } else { -10.0 };
                p.insert("elbow_r", [0.0, 0.0, -50.0 + swing]);
                n += key_pose(scene, root, f, &p, cur_drop, Interp::Ease)?;
            }
            n += key_pose(scene, root, end, &stand, cur_drop, Interp::Ease)?;
        }
        "pickUp" => {
            n += key_pose(scene, root, start, &pose_map("stand"), 0.0, Interp::Ease)?;
            n +=
                key_pose(scene, root, start + frames * 0.45, &pose_map("pickUp"), pelvis_drop("pickUp"), Interp::Ease)?;
            n += key_pose(scene, root, start + frames * 0.6, &pose_map("pickUp"), pelvis_drop("pickUp"), Interp::Ease)?;
            n += key_pose(scene, root, end, &pose_map("holdBasket"), 0.0, Interp::Ease)?;
        }
        "pointAt" => {
            n += key_pose(scene, root, start, &stand, cur_drop, Interp::Ease)?;
            n += key_pose(scene, root, start + frames * 0.3, &upper(&stand, "point"), cur_drop, Interp::Ease)?;
            n += key_pose(scene, root, start + frames * 0.75, &upper(&stand, "point"), cur_drop, Interp::Ease)?;
            n += key_pose(scene, root, end, &stand, cur_drop, Interp::Ease)?;
        }
        "talk" => {
            let beats = ((frames / fps) * 1.5 * k.clamp(0.5, 2.0)).round().max(2.0) as usize;
            for i in 0..=beats {
                let u = i as f32 / beats as f32;
                let mut p = stand.clone();
                let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                if i != 0 && i != beats {
                    p.insert("shoulder_r", [-25.0 - 10.0 * side, 0.0, -10.0]);
                    p.insert("elbow_r", [-70.0 + 15.0 * side, 0.0, 0.0]);
                    p.insert("shoulder_l", [-15.0 + 8.0 * side, 0.0, 10.0]);
                    p.insert("elbow_l", [-55.0 - 10.0 * side, 0.0, 0.0]);
                    p.insert("head", [6.0 * side, 8.0 * side, 0.0]);
                }
                n += key_pose(scene, root, start + frames * u, &p, cur_drop, Interp::Ease)?;
            }
        }
        "turnHead" => {
            let a = 45.0 * k.min(1.6);
            for (u, yaw) in [(0.0, 0.0), (0.3, a), (0.7, -a), (1.0, 0.0)] {
                let mut p = stand.clone();
                p.insert("head", [0.0, yaw, 0.0]);
                p.insert("neck", [0.0, yaw * 0.3, 0.0]);
                n += key_pose(scene, root, start + frames * u, &p, cur_drop, Interp::Ease)?;
            }
        }
        "nod" => {
            for (u, x) in [(0.0, 0.0), (0.2, 18.0), (0.4, -4.0), (0.6, 16.0), (0.8, -3.0), (1.0, 0.0)] {
                let mut p = stand.clone();
                p.insert("head", [x * k.min(2.0), 0.0, 0.0]);
                n += key_pose(scene, root, start + frames * u, &p, cur_drop, Interp::Ease)?;
            }
        }
        "sitDown" | "standUp" => {
            let (a, b) = if name == "sitDown" { ("stand", "sit") } else { ("sit", "stand") };
            n += key_pose(scene, root, start, &pose_map(a), pelvis_drop(a), Interp::Ease)?;
            n += key_pose(scene, root, end, &pose_map(b), pelvis_drop(b), Interp::Ease)?;
        }
        _ => {}
    }
    Ok(n)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyMoveParams {
    id: String,
    property: TrackProperty,
    /// Frame of the key to move.
    from: f32,
    /// Frame to move it to (replaces a key already there).
    to: f32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyInterpParams {
    id: String,
    property: TrackProperty,
    frame: f32,
    /// step, linear, ease, easeIn, easeOut or overshoot: how to move from this key to the next.
    interp: Interp,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RetimeParams {
    /// Entities whose keys move (every animated property of each; list a person's joints too).
    ids: Vec<String>,
    /// Only keys at or after this frame (default: all).
    #[serde(default)]
    from: Option<f32>,
    /// Only keys at or before this frame (default: all).
    #[serde(default)]
    to: Option<f32>,
    /// Frames to shift by (after scaling).
    #[serde(default)]
    offset: f32,
    /// Speed change: 0.5 plays twice as fast, 2 half as fast (around `pivot`).
    #[serde(default = "one")]
    scale: f32,
    /// Frame that stays put when scaling (default: the first key moved).
    #[serde(default)]
    pivot: Option<f32>,
}

fn one() -> f32 {
    1.0
}

/// Shift and/or stretch every key of `ids` in a frame range, in one step. A moved key replaces
/// one already sitting where it lands.
fn retime(s: &mut Scene, p: RetimeParams) -> CmdResult<Value> {
    if !(p.scale.is_finite() && p.scale > 0.01 && p.scale < 100.0) {
        return Err(CommandError::invalid("`scale` must be between 0.01 and 100"));
    }
    if !p.offset.is_finite() {
        return Err(CommandError::invalid("`offset` must be a number"));
    }
    let (lo, hi) = (p.from.unwrap_or(f32::NEG_INFINITY), p.to.unwrap_or(f32::INFINITY));
    let within = |f: f32| f >= lo - 1e-3 && f <= hi + 1e-3;
    let tracks: Vec<usize> =
        (0..s.timeline.tracks.len()).filter(|&i| p.ids.contains(&s.timeline.tracks[i].entity)).collect();
    let first = tracks
        .iter()
        .flat_map(|&i| s.timeline.tracks[i].keys.iter().map(|k| k.frame))
        .filter(|f| within(*f))
        .min_by(f32::total_cmp)
        .ok_or_else(|| CommandError::new("failed", "no keys to retime"))?;
    let pivot = p.pivot.unwrap_or(first);
    let map = |f: f32| ((pivot + (f - pivot) * p.scale + p.offset) * 1000.0).round() / 1000.0;
    if map(first) < 0.0 {
        return Err(CommandError::invalid(format!("that would move keys before frame 0 (to {:.1})", map(first))));
    }
    let (mut moved, mut last) = (0, f32::NEG_INFINITY);
    for i in tracks {
        let t = &mut s.timeline.tracks[i];
        let (inside, outside): (Vec<_>, Vec<_>) = t.keys.drain(..).partition(|k| within(k.frame));
        t.keys = outside;
        for mut k in inside {
            k.frame = map(k.frame);
            last = last.max(k.frame);
            moved += 1;
            t.set_key(k);
        }
    }
    Ok(json!({ "keys": moved, "first": map(first), "last": last }))
}

fn track_of<'a>(scene: &'a mut Scene, id: &str, property: TrackProperty) -> CmdResult<&'a mut shotstage_core::Track> {
    scene
        .timeline
        .tracks
        .iter_mut()
        .find(|t| t.entity == id && t.property == property)
        .ok_or_else(|| CommandError::new("failed", format!("`{id}` has no {property:?} keys")))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("motion.apply", "Animate with a preset in one step: camera moves (dolly, truck, pedestal, pan, tilt, arc, crane, push-in, pull-back reveal, dolly zoom, whip pan, tracking, handheld, drone flyover, 360 spin, zoom), object motion (turntable, bounce, float, drop-in, slide-in, pop-in, wobble, hero reveal, path follow) and character actions (walk, wave, pick up, look around, nod, sit down, stand up). Writes ordinary keyframes; see motion.list.", mutates: true, MotionParams, apply),
        command!("motion.list", "The motion presets with their groups, default lengths and descriptions.", mutates: false, crate::builtin::NoParams,
            |_, _| Ok(json!(PRESETS.iter().map(|p| json!({
                "preset": p.name, "label": p.label, "description": p.description, "seconds": p.seconds,
                "group": match p.group { Group::Camera => "camera", Group::Object => "object", Group::Character => "character" },
            })).collect::<Vec<_>>()))),
        command!("keyframe.move", "Move a keyframe to another frame (retiming).", mutates: true, KeyMoveParams,
        |s, p| {
            if !(p.to.is_finite() && (0.0..=1_000_000.0).contains(&p.to)) {
                return Err(CommandError::invalid("`to` must be a frame >= 0"));
            }
            let t = track_of(s, &p.id, p.property)?;
            let i = t.keys.iter().position(|k| (k.frame - p.from).abs() < 1e-3)
                .ok_or_else(|| CommandError::new("failed", format!("no key at frame {}", p.from)))?;
            let mut key = t.keys.remove(i);
            key.frame = p.to;
            t.set_key(key);
            Ok(json!({ "frame": p.to }))
        }),
        command!("keyframe.retime", "Shift and/or speed up / slow down all keys of some entities in a frame range in one step: new = pivot + (frame - pivot) * scale + offset.", mutates: true, RetimeParams,
            |s, p| retime(s, p)),
        command!("keyframe.setInterp", "Set how a keyframe eases into the next one: step, linear, ease, easeIn, easeOut, overshoot.", mutates: true, KeyInterpParams,
        |s, p| {
            let t = track_of(s, &p.id, p.property)?;
            let k = t.keys.iter_mut().find(|k| (k.frame - p.frame).abs() < 1e-3)
                .ok_or_else(|| CommandError::new("failed", format!("no key at frame {}", p.frame)))?;
            k.interp = p.interp;
            Ok(Value::Null)
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> crate::Session {
        let mut s = crate::Session::default();
        s.dispatch("prefab.create", json!({"kind": "woman", "id": "w"})).unwrap();
        s.dispatch("entity.create", json!({"type": "camera", "id": "cam", "position": [0, 1.5, 5]})).unwrap();
        s.dispatch("camera.lookAt", json!({"id": "cam", "target": "w"})).unwrap();
        s
    }

    fn cam_pos(s: &crate::Session, f: f32) -> Vec3 {
        world_at(s.scene(), "cam", f).unwrap().w_axis.truncate()
    }

    #[test]
    fn every_preset_runs_and_is_one_undo_step() {
        for p in PRESETS {
            let mut s = scene();
            let target = match p.group {
                Group::Camera => "cam",
                Group::Object | Group::Character => "w",
            };
            let mut params = json!({"preset": p.name, "target": target, "subject": "w"});
            if p.name == "pathFollow" {
                params["points"] = json!([[1, 0, 0], [2, 0, 1], [1, 0, 2]]);
            }
            let rev = s.revision();
            let r = s.dispatch("motion.apply", params).unwrap_or_else(|e| panic!("{}: {e}", p.name)).result;
            assert!(r["keys"].as_u64().unwrap() >= 2, "{}: {r}", p.name);
            assert_eq!(s.revision(), rev + 1);
            s.dispatch("history.undo", json!({})).unwrap();
            assert!(
                s.scene().timeline.tracks.iter().all(|t| t.entity != target || t.keys.is_empty())
                    || p.name == "handheld",
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn camera_moves_do_what_they_say() {
        let mut s = scene();
        let d0 = cam_pos(&s, 0.0).distance(Vec3::new(0.0, 0.85, 0.0));
        let r = s.dispatch("motion.apply", json!({"preset": "dollyIn", "subject": "w"})).unwrap().result;
        let end = r["end"].as_f64().unwrap() as f32;
        assert_eq!(end, 72.0, "3 s at 24 fps");
        assert!(cam_pos(&s, end).distance(Vec3::new(0.0, 0.85, 0.0)) < d0 - 0.3);
        // dolly zoom keeps the subject's framing while moving back
        let mut s = scene();
        let before =
            s.dispatch("camera.describe", json!({"id": "cam", "subject": "w"})).unwrap().result["subjectFrameFraction"]
                .as_f64()
                .unwrap();
        s.dispatch("motion.apply", json!({"preset": "dollyZoom", "subject": "w"})).unwrap();
        let after = s.dispatch("camera.describe", json!({"id": "cam", "subject": "w", "frame": 72})).unwrap().result["subjectFrameFraction"].as_f64().unwrap();
        assert!(cam_pos(&s, 72.0).z > 6.0);
        assert!((after - before).abs() / before < 0.15, "{before} -> {after}");
        // a 360 spin comes back to the start
        let mut s = scene();
        s.dispatch("motion.apply", json!({"preset": "productSpin", "subject": "w", "duration": 4})).unwrap();
        assert!(cam_pos(&s, 0.0).distance(cam_pos(&s, 96.0)) < 0.01);
        assert!(cam_pos(&s, 0.0).distance(cam_pos(&s, 48.0)) > 5.0);
        // cameras only for camera moves
        assert!(s.dispatch("motion.apply", json!({"preset": "dollyIn", "target": "w"})).is_err());
        assert!(s.dispatch("motion.apply", json!({"preset": "warp", "target": "w"})).is_err());
    }

    #[test]
    fn walking_moves_forward_and_keys_retime() {
        let mut s = scene();
        s.dispatch("motion.apply", json!({"preset": "walkCycle", "target": "w", "duration": 2})).unwrap();
        let p0 = world_at(s.scene(), "w", 0.0).unwrap().w_axis.truncate();
        let p1 = world_at(s.scene(), "w", 48.0).unwrap().w_axis.truncate();
        assert!(p1.z - p0.z > 1.0, "walks towards its front (+Z): {p0} -> {p1}");
        let keys = s.dispatch("keyframe.list", json!({"id": "w_knee_l"})).unwrap().result;
        assert!(keys[0]["frames"].as_array().unwrap().len() >= 4);
        s.dispatch("keyframe.move", json!({"id": "w", "property": "transform", "from": 48, "to": 60})).unwrap();
        s.dispatch("keyframe.setInterp", json!({"id": "w", "property": "transform", "frame": 0, "interp": "easeOut"}))
            .unwrap();
        let p = world_at(s.scene(), "w", 60.0).unwrap().w_axis.truncate();
        assert!((p - p1).length() < 1e-4);
    }

    #[test]
    fn retime_shifts_and_stretches() {
        let mut s = scene();
        s.dispatch(
            "entity.create",
            json!({"type": "mesh", "id": "box", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        s.dispatch("motion.apply", json!({"preset": "bounce", "target": "box", "start": 10, "duration": 2})).unwrap();
        let frames = |s: &crate::Session| -> Vec<f64> {
            let t = s.scene().timeline.tracks.iter().find(|t| t.entity == "box").unwrap();
            t.keys.iter().map(|k| k.frame as f64).collect()
        };
        let before = frames(&s);
        let mid = world_at(s.scene(), "box", before[1] as f32).unwrap().w_axis.truncate();
        // twice as slow, starting 5 frames later: one undo step
        let r = s.dispatch("keyframe.retime", json!({"ids": ["box"], "scale": 2, "offset": 5})).unwrap().result;
        assert_eq!(r["first"], 15.0);
        let after = frames(&s);
        assert_eq!(after.len(), before.len());
        for (a, b) in before.iter().zip(&after) {
            assert!((10.0 + (a - 10.0) * 2.0 + 5.0 - b).abs() < 1e-3, "{a} -> {b}");
        }
        let moved = world_at(s.scene(), "box", after[1] as f32).unwrap().w_axis.truncate();
        assert!((moved - mid).length() < 1e-4, "same pose at the matching key");
        s.dispatch("history.undo", json!({})).unwrap();
        assert_eq!(frames(&s), before);
        // a range: only the later keys move; nothing may go below frame 0
        assert!(s.dispatch("keyframe.retime", json!({"ids": ["box"], "offset": -100})).is_err());
        s.dispatch("keyframe.retime", json!({"ids": ["box"], "from": before[1], "offset": 3})).unwrap();
        let ranged = frames(&s);
        assert_eq!(ranged[0], before[0]);
        assert!((ranged[1] - before[1] - 3.0).abs() < 1e-3);
    }
}
