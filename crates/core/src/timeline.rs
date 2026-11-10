//! Keyframe tracks and shots. Sampling is pure and deterministic: the same scene and frame
//! always give the same pose, so the browser and the headless server render identical frames.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::scene::Transform;
use crate::{CoreError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TrackProperty {
    /// Local transform of any entity.
    Transform,
    /// A camera's focal length in mm (zooms).
    FocalLength,
    /// A realistic person's facial expression (track on the person's root).
    Expression,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Interp {
    /// Hold the value until the next key.
    Step,
    Linear,
    /// Smooth ease-in/ease-out between keys (the default: reads like a real camera move).
    #[default]
    Ease,
    /// Start slowly, arrive at full speed.
    EaseIn,
    /// Leave at full speed, settle gently.
    EaseOut,
    /// Overshoot a little and settle back (a springy arrival).
    Overshoot,
    /// A custom speed curve, like CSS `cubic-bezier(x1, y1, x2, y2)`: the curve runs from (0,0)
    /// to (1,1) through two handles. x is time (kept within 0..1), y is progress (may go below
    /// 0 or above 1 to anticipate or overshoot). `{"bezier": [0.42, 0, 0.58, 1]}` is ease-in-out.
    Bezier([f32; 4]),
}

impl Interp {
    /// Progress 0..1 between two keys for a time fraction `t` 0..1.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Interp::Step => 0.0,
            Interp::Linear => t,
            Interp::Ease => ease(t),
            Interp::EaseIn => t * t * t,
            Interp::EaseOut => 1.0 - (1.0 - t).powi(3),
            Interp::Overshoot => {
                let (c1, c3) = (1.70158f32, 2.70158f32);
                1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
            }
            Interp::Bezier(h) => cubic_bezier(Interp::clamp_handles(h), t),
        }
    }

    /// Bezier handles kept usable: x within 0..1 (so time only moves forward), y within -1..2.
    pub fn clamp_handles(h: [f32; 4]) -> [f32; 4] {
        let ok = |v: f32, d: f32| if v.is_finite() { v } else { d };
        [
            ok(h[0], 0.25).clamp(0.0, 1.0),
            ok(h[1], 0.1).clamp(-1.0, 2.0),
            ok(h[2], 0.25).clamp(0.0, 1.0),
            ok(h[3], 1.0).clamp(-1.0, 2.0),
        ]
    }

    /// The same curve as Bezier handles, for editing a preset by hand. `None` for step.
    pub fn as_bezier(self) -> Option<[f32; 4]> {
        Some(match self {
            Interp::Step => return None,
            Interp::Linear => [0.0, 0.0, 1.0, 1.0],
            // smoothstep is close to this ease-in-out
            Interp::Ease => [0.42, 0.0, 0.58, 1.0],
            Interp::EaseIn => [0.55, 0.0, 1.0, 0.45],
            Interp::EaseOut => [0.0, 0.55, 0.45, 1.0],
            Interp::Overshoot => [0.34, 1.56, 0.64, 1.0],
            Interp::Bezier(h) => Interp::clamp_handles(h),
        })
    }
}

/// CSS-style cubic Bezier from (0,0) to (1,1): find the curve parameter whose x is `t`, give
/// its y.
fn cubic_bezier([x1, y1, x2, y2]: [f32; 4], t: f32) -> f32 {
    let bez = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    let dx = |s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * x1 + 6.0 * u * s * (x2 - x1) + 3.0 * s * s * (1.0 - x2)
    };
    // Newton from s = t, then bisection if it wanders (x is monotonic since x1, x2 are in 0..1)
    let mut s = t;
    for _ in 0..8 {
        let err = bez(x1, x2, s) - t;
        if err.abs() < 1e-6 {
            return bez(y1, y2, s);
        }
        let d = dx(s);
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    s = t;
    for _ in 0..30 {
        let x = bez(x1, x2, s);
        if (x - t).abs() < 1e-6 {
            break;
        }
        if x < t {
            lo = s
        } else {
            hi = s
        }
        s = (lo + hi) / 2.0;
    }
    bez(y1, y2, s)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum KeyValue {
    Transform(Transform),
    Scalar(f32),
    Expression(ExpressionMix),
}

/// A mix of two named facial expressions (indices into `human::EXPRESSIONS`). A key holds one
/// expression (`b` = `a`, `bAmount` = 0); between keys the face cross-fades from one to the next.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionMix {
    pub a: u8,
    pub a_amount: f32,
    #[serde(default)]
    pub b: u8,
    #[serde(default)]
    pub b_amount: f32,
}

impl ExpressionMix {
    /// One named expression at `amount`.
    pub fn single(index: u8, amount: f32) -> Self {
        Self { a: index, a_amount: amount, b: index, b_amount: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Key {
    /// Frame number (may be fractional).
    pub frame: f32,
    pub value: KeyValue,
    /// Interpolation from this key to the next one.
    #[serde(default)]
    pub interp: Interp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Track {
    pub entity: String,
    pub property: TrackProperty,
    /// Sorted by frame, at most one key per frame.
    pub keys: Vec<Key>,
}

/// A contiguous range of frames seen through one camera.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Shot {
    pub id: String,
    pub name: String,
    pub camera: String,
    /// First frame (inclusive).
    pub start: u32,
    /// Last frame (inclusive).
    pub end: u32,
    /// Notes for the crew: action, dialogue, how the camera moves. Printed on the shot sheet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Timeline {
    pub fps: f32,
    pub start: u32,
    pub end: u32,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub shots: Vec<Shot>,
}

impl Default for Timeline {
    fn default() -> Self {
        // 5 s at 24 fps: a typical AI video clip length.
        Self { fps: 24.0, start: 0, end: 119, tracks: Vec::new(), shots: Vec::new() }
    }
}

/// One number of an animated value, for curve editing: a position or rotation axis, a scale
/// axis, or a camera's focal length. Rotations are Euler degrees (Y, X, Z order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Channel {
    X,
    Y,
    Z,
    RotX,
    RotY,
    RotZ,
    ScaleX,
    ScaleY,
    ScaleZ,
    Focal,
}

impl Channel {
    pub const TRANSFORM: [Channel; 9] = [
        Channel::X,
        Channel::Y,
        Channel::Z,
        Channel::RotX,
        Channel::RotY,
        Channel::RotZ,
        Channel::ScaleX,
        Channel::ScaleY,
        Channel::ScaleZ,
    ];

    /// The channels a property has.
    pub fn of(property: TrackProperty) -> &'static [Channel] {
        match property {
            TrackProperty::Transform => &Channel::TRANSFORM,
            TrackProperty::FocalLength => &[Channel::Focal],
            TrackProperty::Expression => &[],
        }
    }

    pub fn is_rotation(self) -> bool {
        matches!(self, Channel::RotX | Channel::RotY | Channel::RotZ)
    }

    /// This channel's number in `v` (None when `v` has no such number).
    pub fn get(self, v: &KeyValue) -> Option<f32> {
        match (self, v) {
            (Channel::Focal, KeyValue::Scalar(f)) => Some(*f),
            (Channel::X | Channel::Y | Channel::Z, KeyValue::Transform(t)) => {
                Some(t.translation[self as usize - Channel::X as usize])
            }
            (Channel::RotX | Channel::RotY | Channel::RotZ, KeyValue::Transform(t)) => {
                Some(t.euler_degrees()[self as usize - Channel::RotX as usize])
            }
            (Channel::ScaleX | Channel::ScaleY | Channel::ScaleZ, KeyValue::Transform(t)) => {
                Some(t.scale[self as usize - Channel::ScaleX as usize])
            }
            _ => None,
        }
    }

    /// `v` with this channel set to `x`. False when `v` has no such channel.
    pub fn set(self, v: &mut KeyValue, x: f32) -> bool {
        match (self, v) {
            (Channel::Focal, KeyValue::Scalar(f)) => *f = x,
            (Channel::X | Channel::Y | Channel::Z, KeyValue::Transform(t)) => {
                t.translation[self as usize - Channel::X as usize] = x
            }
            (Channel::RotX | Channel::RotY | Channel::RotZ, KeyValue::Transform(t)) => {
                let mut e = t.euler_degrees();
                e[self as usize - Channel::RotX as usize] = x;
                t.set_euler_degrees(e);
            }
            (Channel::ScaleX | Channel::ScaleY | Channel::ScaleZ, KeyValue::Transform(t)) => {
                t.scale[self as usize - Channel::ScaleX as usize] = x
            }
            _ => return false,
        }
        true
    }
}

/// Keep an angle in degrees within half a turn of `near`, so a rotation curve reads continuous.
pub fn unwrap_degrees(a: f32, near: f32) -> f32 {
    a - ((a - near) / 360.0).round() * 360.0
}

fn ease(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn blend(a: &KeyValue, b: &KeyValue, t: f32) -> KeyValue {
    match (a, b) {
        (KeyValue::Scalar(x), KeyValue::Scalar(y)) => KeyValue::Scalar(x + (y - x) * t),
        (KeyValue::Transform(x), KeyValue::Transform(y)) => KeyValue::Transform(Transform::from_parts(
            x.translation().lerp(y.translation(), t),
            x.rotation().slerp(y.rotation(), t),
            x.scale().lerp(y.scale(), t),
        )),
        (KeyValue::Expression(x), KeyValue::Expression(y)) => KeyValue::Expression(ExpressionMix {
            a: x.a,
            a_amount: x.a_amount * (1.0 - t),
            b: y.a,
            b_amount: y.a_amount * t,
        }),
        _ => *a,
    }
}

impl Track {
    pub fn sample(&self, frame: f32) -> Option<KeyValue> {
        let first = self.keys.first()?;
        if frame <= first.frame {
            return Some(first.value);
        }
        let last = self.keys.last()?;
        if frame >= last.frame {
            return Some(last.value);
        }
        let i = self.keys.partition_point(|k| k.frame <= frame);
        let (a, b) = (&self.keys[i - 1], &self.keys[i]);
        let t = ((frame - a.frame) / (b.frame - a.frame)).clamp(0.0, 1.0);
        let t = a.interp.apply(t);
        Some(blend(&a.value, &b.value, t))
    }

    /// Insert or replace the key at `key.frame`, keeping the keys sorted.
    pub fn set_key(&mut self, key: Key) {
        match self.keys.iter().position(|k| (k.frame - key.frame).abs() < 1e-4) {
            Some(i) => self.keys[i] = key,
            None => {
                let i = self.keys.partition_point(|k| k.frame < key.frame);
                self.keys.insert(i, key);
            }
        }
    }
}

impl Timeline {
    pub fn validate_range(fps: f32, start: u32, end: u32) -> Result<()> {
        if !(fps.is_finite() && (1.0..=240.0).contains(&fps)) {
            return Err(CoreError::Invalid(format!("fps must be 1..=240, got {fps}")));
        }
        if end < start || end - start > 100_000 {
            return Err(CoreError::Invalid(format!("need start <= end (and under 100000 frames), got {start}..{end}")));
        }
        Ok(())
    }

    pub fn track(&self, entity: &str, property: TrackProperty) -> Option<&Track> {
        self.tracks.iter().find(|t| t.entity == entity && t.property == property)
    }

    pub fn track_mut_or_insert(&mut self, entity: &str, property: TrackProperty) -> &mut Track {
        let i = match self.tracks.iter().position(|t| t.entity == entity && t.property == property) {
            Some(i) => i,
            None => {
                self.tracks.push(Track { entity: entity.into(), property, keys: Vec::new() });
                self.tracks.len() - 1
            }
        };
        &mut self.tracks[i]
    }

    /// The shot covering `frame`, if any (first match wins).
    pub fn shot_at(&self, frame: f32) -> Option<&Shot> {
        self.shots.iter().find(|s| frame >= s.start as f32 && frame <= s.end as f32 + 0.999)
    }

    pub fn duration_seconds(&self) -> f32 {
        (self.end - self.start + 1) as f32 / self.fps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_track(interp: Interp) -> Track {
        let mut t = Track { entity: "cam".into(), property: TrackProperty::FocalLength, keys: vec![] };
        t.set_key(Key { frame: 10.0, value: KeyValue::Scalar(50.0), interp });
        t.set_key(Key { frame: 0.0, value: KeyValue::Scalar(20.0), interp });
        t
    }

    fn val(v: Option<KeyValue>) -> f32 {
        match v {
            Some(KeyValue::Scalar(s)) => s,
            _ => panic!("expected scalar"),
        }
    }

    #[test]
    fn sampling() {
        let t = scalar_track(Interp::Linear);
        assert_eq!(t.keys[0].frame, 0.0);
        assert_eq!(val(t.sample(-5.0)), 20.0);
        assert_eq!(val(t.sample(5.0)), 35.0);
        assert_eq!(val(t.sample(99.0)), 50.0);
        assert_eq!(val(scalar_track(Interp::Step).sample(9.0)), 20.0);
        let e = val(scalar_track(Interp::Ease).sample(2.0));
        assert!(e > 20.0 && e < 26.0);
    }

    #[test]
    fn channels_read_and_write() {
        let mut v = KeyValue::Transform(Transform::default());
        assert!(Channel::Y.set(&mut v, 1.5));
        assert!(Channel::RotY.set(&mut v, 30.0));
        assert!(Channel::ScaleZ.set(&mut v, 2.0));
        assert_eq!(Channel::Y.get(&v), Some(1.5));
        assert!((Channel::RotY.get(&v).unwrap() - 30.0).abs() < 1e-3);
        assert_eq!(Channel::ScaleZ.get(&v), Some(2.0));
        assert_eq!(Channel::Focal.get(&v), None);
        assert!(!Channel::Focal.set(&mut v, 3.0));
        let mut f = KeyValue::Scalar(35.0);
        assert!(Channel::Focal.set(&mut f, 50.0));
        assert_eq!(Channel::Focal.get(&f), Some(50.0));
        assert!((unwrap_degrees(-170.0, 175.0) - 190.0).abs() < 1e-4);
    }

    #[test]
    fn bezier_curves() {
        let lin = Interp::Bezier([0.0, 0.0, 1.0, 1.0]);
        for t in [0.0, 0.1, 0.5, 0.77, 1.0] {
            assert!((lin.apply(t) - t).abs() < 1e-3, "linear at {t}");
        }
        let io = Interp::Bezier([0.42, 0.0, 0.58, 1.0]);
        assert!((io.apply(0.5) - 0.5).abs() < 1e-3);
        assert!(io.apply(0.1) < 0.05 && io.apply(0.9) > 0.95);
        // close to the built-in ease
        assert!((io.apply(0.3) - Interp::Ease.apply(0.3)).abs() < 0.05);
        // overshoots above 1 before settling
        let back = Interp::Bezier([0.34, 1.56, 0.64, 1.0]);
        assert!((1..20).any(|i| back.apply(i as f32 / 20.0) > 1.0));
        assert!((back.apply(1.0) - 1.0).abs() < 1e-4);
        // nonsense handles are clamped, never NaN
        let wild = Interp::Bezier([f32::NAN, 9.0, -3.0, -9.0]);
        assert!(wild.apply(0.4).is_finite());
        // serde: unit presets stay strings, custom curves are {"bezier": [...]}
        assert_eq!(serde_json::to_value(Interp::Ease).unwrap(), serde_json::json!("ease"));
        let b: Interp = serde_json::from_value(serde_json::json!({"bezier": [0.1, 0.2, 0.3, 0.4]})).unwrap();
        assert_eq!(b, Interp::Bezier([0.1, 0.2, 0.3, 0.4]));
        for i in [Interp::Linear, Interp::Ease, Interp::EaseIn, Interp::EaseOut, Interp::Overshoot] {
            let h = i.as_bezier().unwrap();
            let approx = Interp::Bezier(h);
            for t in [0.25, 0.5, 0.75] {
                assert!((approx.apply(t) - i.apply(t)).abs() < 0.12, "{i:?} at {t}");
            }
        }
    }

    #[test]
    fn set_key_replaces_same_frame() {
        let mut t = scalar_track(Interp::Linear);
        t.set_key(Key { frame: 10.0, value: KeyValue::Scalar(80.0), interp: Interp::Linear });
        assert_eq!(t.keys.len(), 2);
        assert_eq!(val(t.sample(10.0)), 80.0);
    }
}
