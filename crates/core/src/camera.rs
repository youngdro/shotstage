//! Physical camera model (focal length + sensor) and the matrices/intrinsics derived from it.

use glam::{Mat3, Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct CameraData {
    /// Lens focal length in millimetres.
    pub focal_length_mm: f32,
    /// Sensor (film back) width in millimetres; 36 = full frame.
    pub sensor_width_mm: f32,
    /// Frame aspect ratio, width / height (16:9 = 1.7778).
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for CameraData {
    fn default() -> Self {
        Self { focal_length_mm: 35.0, sensor_width_mm: 36.0, aspect: 16.0 / 9.0, near: 0.05, far: 500.0 }
    }
}

/// Pinhole intrinsics in pixels, as used by camera-conditioned video models.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Intrinsics {
    pub fx: f32,
    pub fy: f32,
    pub cx: f32,
    pub cy: f32,
    pub width: u32,
    pub height: u32,
}

impl CameraData {
    pub fn validate(&self) -> Result<()> {
        let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && v >= lo && v <= hi;
        if !ok(self.focal_length_mm, 1.0, 2000.0) {
            return Err(CoreError::Invalid(format!("focalLengthMm must be 1..=2000, got {}", self.focal_length_mm)));
        }
        if !ok(self.sensor_width_mm, 1.0, 200.0) {
            return Err(CoreError::Invalid(format!("sensorWidthMm must be 1..=200, got {}", self.sensor_width_mm)));
        }
        if !ok(self.aspect, 0.1, 10.0) {
            return Err(CoreError::Invalid(format!("aspect must be 0.1..=10, got {}", self.aspect)));
        }
        if !(ok(self.near, 1e-4, 1e4) && ok(self.far, 1e-3, 1e6) && self.far > self.near) {
            return Err(CoreError::Invalid("need 0 < near < far".into()));
        }
        Ok(())
    }

    /// Horizontal field of view in radians.
    pub fn hfov(&self) -> f32 {
        2.0 * (self.sensor_width_mm / (2.0 * self.focal_length_mm)).atan()
    }

    /// Vertical field of view in radians (sensor height = width / aspect).
    pub fn vfov(&self) -> f32 {
        let sensor_h = self.sensor_width_mm / self.aspect;
        2.0 * (sensor_h / (2.0 * self.focal_length_mm)).atan()
    }

    /// Right-handed perspective with a 0..1 depth range (wgpu/WebGPU convention).
    pub fn projection(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.vfov(), aspect, self.near, self.far)
    }

    pub fn intrinsics(&self, width: u32, height: u32) -> Intrinsics {
        let fx = self.focal_length_mm / self.sensor_width_mm * width as f32;
        Intrinsics { fx, fy: fx, cx: width as f32 / 2.0, cy: height as f32 / 2.0, width, height }
    }
}

/// Rotation that points an object's -Z axis from `eye` at `target`, keeping +Y up.
pub fn look_rotation(eye: Vec3, target: Vec3) -> Result<Quat> {
    let fwd = (target - eye).normalize_or_zero();
    if fwd == Vec3::ZERO {
        return Err(CoreError::Invalid("look-at target is at the camera position".into()));
    }
    let up = if fwd.cross(Vec3::Y).length_squared() < 1e-8 { Vec3::Z } else { Vec3::Y };
    let right = fwd.cross(up).normalize();
    let up = right.cross(fwd);
    Ok(Quat::from_mat3(&Mat3::from_cols(right, up, -fwd)).normalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fov_of_full_frame_35mm() {
        let c = CameraData::default();
        assert!((c.hfov().to_degrees() - 54.43).abs() < 0.05);
        let k = c.intrinsics(1920, 1080);
        assert!((k.fx - 1866.67).abs() < 0.1);
    }

    #[test]
    fn look_rotation_points_minus_z() {
        let q = look_rotation(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO).unwrap();
        assert!((q * Vec3::NEG_Z - Vec3::NEG_Z).length() < 1e-5);
        let q = look_rotation(Vec3::new(5.0, 0.0, 0.0), Vec3::ZERO).unwrap();
        assert!((q * Vec3::NEG_Z - Vec3::NEG_X).length() < 1e-5);
        assert!(look_rotation(Vec3::ONE, Vec3::ONE).is_err());
    }
}
