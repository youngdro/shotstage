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
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Interp {
    /// Hold the value until the next key.
    Step,
    Linear,
    /// Smooth ease-in/ease-out between keys (the default: reads like a real camera move).
    #[default]
    Ease,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum KeyValue {
    Transform(Transform),
    Scalar(f32),
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
        let t = match a.interp {
            Interp::Step => 0.0,
            Interp::Linear => t,
            Interp::Ease => ease(t),
        };
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
    fn set_key_replaces_same_frame() {
        let mut t = scalar_track(Interp::Linear);
        t.set_key(Key { frame: 10.0, value: KeyValue::Scalar(80.0), interp: Interp::Linear });
        assert_eq!(t.keys.len(), 2);
        assert_eq!(val(t.sample(10.0)), 80.0);
    }
}
