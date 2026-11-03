//! Optional scene lights. A scene without lights is shown with the flat studio clay light; as
//! soon as it has a visible light, the clay pass is lit by its lights instead (no shadows).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LightKind {
    /// Parallel light from far away (the sun); only its direction matters.
    Sun,
    /// Light from a point in every direction.
    #[default]
    Point,
    /// A cone of light along the light's -Z.
    Spot,
    /// A soft panel (softbox, window) facing its -Z.
    Area,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct LightData {
    pub kind: LightKind,
    /// Linear RGB 0..1.
    pub color: [f32; 3],
    /// Brightness: 1 is a normal key light. Point, spot and area lights have this brightness
    /// 2 m away and fall off with distance; the sun has it everywhere.
    pub intensity: f32,
    /// Spot: full cone angle in degrees.
    pub cone_degrees: f32,
    /// Spot and area: how soft the edge of the beam is, 0..1.
    pub softness: f32,
    /// Area: panel width and height in metres.
    pub size: [f32; 2],
}

impl Default for LightData {
    fn default() -> Self {
        Self {
            kind: LightKind::Point,
            color: [1.0; 3],
            intensity: 1.0,
            cone_degrees: 45.0,
            softness: 0.3,
            size: [0.6, 0.6],
        }
    }
}

impl LightData {
    pub fn validate(&self) -> Result<()> {
        if !self.color.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)) {
            return Err(CoreError::Invalid("light color components must be 0..=1".into()));
        }
        if !(self.intensity.is_finite() && (0.0..=1000.0).contains(&self.intensity)) {
            return Err(CoreError::Invalid(format!("intensity must be 0..=1000, got {}", self.intensity)));
        }
        if !(self.cone_degrees.is_finite() && (1.0..=179.0).contains(&self.cone_degrees)) {
            return Err(CoreError::Invalid(format!("coneDegrees must be 1..=179, got {}", self.cone_degrees)));
        }
        if !(self.softness.is_finite() && (0.0..=1.0).contains(&self.softness)) {
            return Err(CoreError::Invalid("softness must be 0..=1".into()));
        }
        if !self.size.iter().all(|s| s.is_finite() && *s > 0.0 && *s <= 100.0) {
            return Err(CoreError::Invalid("size must be in (0, 100] metres".into()));
        }
        Ok(())
    }
}

/// A light at a frame, in world space, ready for shading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldLight {
    pub kind: LightKind,
    pub position: [f32; 3],
    /// Direction the light shines (its -Z axis).
    pub direction: [f32; 3],
    /// Colour × intensity.
    pub radiance: [f32; 3],
    /// Cosines of the cone's inner and outer half angles (spot and area).
    pub cos_inner: f32,
    pub cos_outer: f32,
}
