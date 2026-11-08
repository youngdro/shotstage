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
    /// Casts shadows (up to four lights do; the brightest win).
    pub cast_shadows: bool,
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
            cast_shadows: true,
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

/// Colour of a black body at `kelvin` (1000..=20000), linear RGB scaled so the brightest
/// channel is 1: 2700 K is a warm bulb, 5600 K daylight, 8000 K a blue shade.
pub fn kelvin_to_rgb(kelvin: f32) -> [f32; 3] {
    // Tanner Helland's fit, done in display space, then made linear
    let t = kelvin.clamp(1000.0, 20000.0) / 100.0;
    let r = if t <= 66.0 { 255.0 } else { 329.698_73 * (t - 60.0).powf(-0.133_204_76) };
    let g = if t <= 66.0 { 99.470_8 * t.ln() - 161.119_57 } else { 288.122_16 * (t - 60.0).powf(-0.075_514_85) };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_73 * (t - 10.0).ln() - 305.044_8
    };
    let c = [r, g, b].map(|v: f32| (v.clamp(0.0, 255.0) / 255.0).powf(2.2));
    let m = c.iter().cloned().fold(1e-6, f32::max);
    c.map(|v| v / m)
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
    pub cast_shadows: bool,
    /// Rough size of the emitter in metres (area panels), for softer shadows.
    pub size: f32,
}

#[cfg(test)]
mod tests {
    #[test]
    fn colour_temperature() {
        let warm = super::kelvin_to_rgb(2700.0);
        let day = super::kelvin_to_rgb(6500.0);
        let cool = super::kelvin_to_rgb(10000.0);
        assert!(warm[0] == 1.0 && warm[2] < 0.3, "{warm:?}");
        assert!(day.iter().all(|c| *c > 0.85), "{day:?}");
        assert!(cool[2] == 1.0 && cool[0] < 0.9, "{cool:?}");
    }
}
