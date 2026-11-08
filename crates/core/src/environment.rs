//! The world around the scene: a sky (built-in or an HDRI panorama) that lights everything
//! softly and can show behind it, and the shadow settings.
//!
//! The sky is an equirectangular panorama: built-in skies are drawn procedurally, an HDRI is a
//! scene asset. From it come a soft ambient light (9 spherical-harmonic coefficients), its
//! brightest direction (to line up a sun or cast a soft shadow) and the background picture.

use glam::{Quat, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Sky {
    /// The neutral grey studio: no sky light, plain backdrop (the default clay look).
    #[default]
    Studio,
    /// One colour all round (`color`): a seamless paper backdrop.
    Color,
    /// Clear blue sky with a high sun.
    Day,
    /// Low warm sun, orange horizon, blue-violet above.
    Sunset,
    /// Bright, even, cloudy sky: soft light, faint shadows.
    Overcast,
    /// Dark blue night with a faint moon.
    Night,
    /// Dark studio with big softbox panels left, right and above (product light).
    Softbox,
    /// An imported HDRI or panorama picture (`hdri` asset).
    Hdri,
}

pub const SKIES: &[(Sky, &str, &str)] = &[
    (Sky::Studio, "studio", "Neutral grey studio, no sky light (default)"),
    (Sky::Color, "color", "One colour all round: seamless paper backdrop"),
    (Sky::Day, "day", "Clear blue sky, high sun"),
    (Sky::Sunset, "sunset", "Low warm sun, orange horizon"),
    (Sky::Overcast, "overcast", "Bright cloudy sky, soft even light"),
    (Sky::Night, "night", "Dark blue night"),
    (Sky::Softbox, "softbox", "Dark studio with softbox panels"),
    (Sky::Hdri, "hdri", "An imported HDRI / panorama (environment.import)"),
];

/// How shadows look. Lights cast them unless their `castShadows` is off.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ShadowSettings {
    pub enabled: bool,
    /// How dark shadows are: 0 = none, 1 = full.
    pub strength: f32,
    /// Edge blur: 0 = crisp, 1 = very soft. Area lights and big lights add their own softness.
    pub softness: f32,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self { enabled: true, strength: 1.0, softness: 0.3 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct Environment {
    pub sky: Sky,
    /// Colour sky: the colour, linear RGB 0..1. Other skies: a tint (white = none).
    pub color: [f32; 3],
    /// HDRI sky: key of the panorama in the scene's assets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdri: Option<String>,
    /// Brightness of the sky's light and background, 0..=10. 1 = as built (HDRIs are
    /// normalised to a usable exposure first).
    pub intensity: f32,
    /// Turn the sky about the vertical axis, degrees (moves the sun of a sky or HDRI).
    pub rotation: f32,
    /// Show the sky behind the scene (otherwise the plain studio backdrop).
    pub background: bool,
    pub shadows: ShadowSettings,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            sky: Sky::Studio,
            color: [1.0; 3],
            hdri: None,
            intensity: 1.0,
            rotation: 0.0,
            background: true,
            shadows: ShadowSettings::default(),
        }
    }
}

impl Environment {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<()> {
        if !self.color.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)) {
            return Err(CoreError::Invalid("environment color components must be 0..=1".into()));
        }
        if !(self.intensity.is_finite() && (0.0..=10.0).contains(&self.intensity)) {
            return Err(CoreError::Invalid(format!("environment intensity must be 0..=10, got {}", self.intensity)));
        }
        if !self.rotation.is_finite() {
            return Err(CoreError::Invalid("rotation must be finite".into()));
        }
        let s = &self.shadows;
        if !(s.strength.is_finite() && (0.0..=1.0).contains(&s.strength)) {
            return Err(CoreError::Invalid("shadow strength must be 0..=1".into()));
        }
        if !(s.softness.is_finite() && (0.0..=1.0).contains(&s.softness)) {
            return Err(CoreError::Invalid("shadow softness must be 0..=1".into()));
        }
        if self.sky == Sky::Hdri && self.hdri.is_none() {
            return Err(CoreError::Invalid("the hdri sky needs an hdri asset (environment.import)".into()));
        }
        Ok(())
    }

    /// Whether the sky lights the scene (everything but the studio).
    pub fn lights_scene(&self) -> bool {
        self.sky != Sky::Studio
    }

    /// What identifies the panorama (not its rotation or brightness): a cache key.
    pub fn source_key(&self) -> String {
        match self.sky {
            Sky::Hdri => format!("hdri:{}", self.hdri.as_deref().unwrap_or("")),
            s => format!("{s:?}:{:?}", self.color),
        }
    }
}

/// Elevation of the built-in skies' sun, degrees.
pub fn sky_sun_elevation(sky: Sky) -> Option<f32> {
    match sky {
        Sky::Day => Some(55.0),
        Sky::Sunset => Some(7.0),
        Sky::Night => Some(35.0),
        _ => None,
    }
}

/// An equirectangular panorama of linear radiance. See [`dir_of`] for the layout.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvMap {
    pub width: u32,
    pub height: u32,
    /// RGB per pixel, row by row from the top (zenith).
    pub rgb: Vec<[f32; 3]>,
    /// Shown as is (a colour backdrop) rather than tone-mapped from radiance.
    pub flat: Option<[f32; 3]>,
    /// Brightest luminance the ambient light takes from it (an HDRI's sun is clipped to this;
    /// a sun light stands in for it).
    pub clip: Option<f32>,
}

/// Direction of a panorama coordinate (`u`, `v` in 0..1; `v` = 0 is straight up). The middle
/// of the picture (`u` = 0.5) looks along -Z, `u` grows to the right (towards +X).
pub fn dir_of(u: f32, v: f32) -> Vec3 {
    let phi = (u - 0.5) * std::f32::consts::TAU;
    let theta = v * std::f32::consts::PI;
    Vec3::new(theta.sin() * phi.sin(), theta.cos(), -theta.sin() * phi.cos())
}

/// Panorama coordinate of a direction (inverse of [`dir_of`]).
pub fn uv_of(d: Vec3) -> (f32, f32) {
    let d = d.normalize_or_zero();
    let u = d.x.atan2(-d.z) / std::f32::consts::TAU + 0.5;
    let v = d.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
    (u.rem_euclid(1.0), v)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn add(a: [f32; 3], b: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

fn sun_dir(sky: Sky) -> Vec3 {
    let e = sky_sun_elevation(sky).unwrap_or(45.0).to_radians();
    Vec3::new(0.0, e.sin(), -e.cos())
}

/// Radiance of a built-in sky in panorama space (before rotation and tint). The sun's disc is
/// left out of the lighting (`disc` false): a sun light stands in for it.
fn sky_radiance(sky: Sky, color: [f32; 3], d: Vec3, disc: bool) -> [f32; 3] {
    let up = d.y;
    let h = up.max(0.0);
    let ground = |c: [f32; 3]| mix(c, [c[0] * 0.6, c[1] * 0.6, c[2] * 0.6], (-up * 3.0).min(1.0));
    let sun = |c: [f32; 3], s: Vec3, size: f32, power: f32, glow: f32| {
        let cos = d.dot(s);
        let disc = if disc && cos > (size.to_radians()).cos() { power } else { 0.0 };
        let halo = glow * ((cos - 1.0) * 12.0).exp() + glow * 0.3 * ((cos - 1.0) * 3.0).exp();
        [c[0] * (disc + halo), c[1] * (disc + halo), c[2] * (disc + halo)]
    };
    match sky {
        Sky::Studio => mix([0.10, 0.10, 0.11], [0.16, 0.16, 0.17], up * 0.5 + 0.5),
        Sky::Color => [color[0] * 0.25, color[1] * 0.25, color[2] * 0.25],
        Sky::Day => {
            if up < 0.0 {
                return ground([0.16, 0.15, 0.13]);
            }
            let sky = mix([0.42, 0.52, 0.66], [0.10, 0.22, 0.55], h.powf(0.5));
            add(sky, sun([1.0, 0.95, 0.85], sun_dir(Sky::Day), 2.0, 900.0, 0.6), 1.0)
        }
        Sky::Sunset => {
            if up < 0.0 {
                return ground([0.07, 0.05, 0.05]);
            }
            let s = sun_dir(Sky::Sunset);
            let toward =
                (Vec3::new(d.x, 0.0, d.z).normalize_or_zero().dot(Vec3::new(s.x, 0.0, s.z).normalize_or_zero()) * 0.5
                    + 0.5)
                    .powf(2.0);
            let horizon = mix([0.30, 0.16, 0.24], [0.95, 0.42, 0.14], toward);
            let sky = mix(horizon, [0.06, 0.08, 0.20], h.powf(0.45));
            add(sky, sun([1.0, 0.55, 0.22], s, 2.5, 500.0, 1.2), 1.0)
        }
        Sky::Overcast => {
            if up < 0.0 {
                return ground([0.12, 0.12, 0.12]);
            }
            mix([0.30, 0.31, 0.32], [0.42, 0.43, 0.45], h)
        }
        Sky::Night => {
            if up < 0.0 {
                return ground([0.004, 0.005, 0.008]);
            }
            let sky = mix([0.025, 0.035, 0.07], [0.008, 0.012, 0.035], h.powf(0.6));
            add(sky, sun([0.6, 0.7, 0.9], sun_dir(Sky::Night), 1.5, 40.0, 0.04), 1.0)
        }
        Sky::Softbox => {
            let base = mix([0.02, 0.02, 0.022], [0.035, 0.035, 0.04], up * 0.5 + 0.5);
            // panels: (centre direction, half width, half height in radians-ish, brightness)
            let panel = |az: f32, el: f32, hw: f32, hh: f32, b: f32| {
                let c = Quat::from_rotation_y(az.to_radians())
                    * Vec3::new(0.0, el.to_radians().sin(), -el.to_radians().cos());
                let right = Vec3::Y.cross(-c).normalize_or(Vec3::X);
                let upv = right.cross(c).normalize_or(Vec3::Y);
                let cos = d.dot(c);
                if cos <= 0.0 {
                    return 0.0;
                }
                let p = d / cos - c;
                let (x, y) = (p.dot(right).abs(), p.dot(upv).abs());
                let edge = |v: f32, half: f32| (1.0 - ((v - half) / (half * 0.25)).clamp(0.0, 1.0)).powf(1.5);
                b * edge(x, hw) * edge(y, hh)
            };
            let k = panel(70.0, 15.0, 0.35, 0.6, 5.0)
                + panel(-70.0, 15.0, 0.35, 0.6, 5.0)
                + panel(0.0, 88.0, 0.5, 0.5, 3.0);
            add(base, [1.0, 1.0, 1.0], k)
        }
        Sky::Hdri => [0.0; 3],
    }
}

/// A built-in sky drawn into a panorama of `width` × `width/2` pixels (`disc`: with the sun's
/// disc, for the background).
pub fn procedural(sky: Sky, color: [f32; 3], width: u32, disc: bool) -> EnvMap {
    let (w, h) = (width.max(8), (width / 2).max(4));
    let tint = if sky == Sky::Color { [1.0; 3] } else { color };
    let mut rgb = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let d = dir_of((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
            let c = sky_radiance(sky, color, d, disc);
            rgb.push([c[0] * tint[0], c[1] * tint[1], c[2] * tint[2]]);
        }
    }
    EnvMap { width: w, height: h, rgb, flat: (sky == Sky::Color).then_some(color), clip: None }
}

/// An HDRI (`.hdr`) or an ordinary panorama picture (PNG, JPEG, WebP; taken as sRGB), scaled
/// down to at most `max_width` pixels wide.
pub fn decode_panorama(bytes: &[u8], max_width: u32) -> Result<EnvMap> {
    let format = image::guess_format(bytes)
        .map_err(|_| CoreError::Invalid("not an HDRI (.hdr) or a PNG, JPEG or WebP panorama".into()))?;
    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| CoreError::Invalid(format!("cannot read the panorama: {e}")))?;
    let hdr = format == image::ImageFormat::Hdr;
    let img = img.to_rgb32f();
    let (w, h) = img.dimensions();
    if w < 4 || h < 2 {
        return Err(CoreError::Invalid("the panorama is too small".into()));
    }
    let rgb: Vec<[f32; 3]> = img
        .pixels()
        .map(|p| {
            let c = p.0.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
            if hdr { c } else { c.map(|v| v.powf(2.2)) }
        })
        .collect();
    let map = EnvMap { width: w, height: h, rgb, flat: None, clip: None };
    Ok(if w > max_width { map.downsample(max_width) } else { map })
}

impl EnvMap {
    /// Box-filtered copy `width` × `width/2` pixels (never larger than this one).
    pub fn downsample(&self, width: u32) -> EnvMap {
        let w = width.clamp(4, self.width);
        let h = (w / 2).clamp(2, self.height);
        let mut rgb = vec![[0.0f32; 3]; (w * h) as usize];
        let (sx, sy) = (self.width as f32 / w as f32, self.height as f32 / h as f32);
        for y in 0..h {
            let (y0, y1) = ((y as f32 * sy) as u32, (((y + 1) as f32 * sy).ceil() as u32).min(self.height));
            for x in 0..w {
                let (x0, x1) = ((x as f32 * sx) as u32, (((x + 1) as f32 * sx).ceil() as u32).min(self.width));
                let mut acc = [0.0f32; 3];
                let mut n = 0.0;
                for yy in y0..y1.max(y0 + 1) {
                    for xx in x0..x1.max(x0 + 1) {
                        let c = self.rgb[(yy.min(self.height - 1) * self.width + xx.min(self.width - 1)) as usize];
                        acc = add(acc, c, 1.0);
                        n += 1.0;
                    }
                }
                rgb[(y * w + x) as usize] = acc.map(|v| v / n);
            }
        }
        EnvMap { width: w, height: h, rgb, flat: self.flat, clip: self.clip }
    }

    /// Average radiance weighted by solid angle.
    pub fn mean(&self) -> [f32; 3] {
        let mut acc = [0.0f32; 3];
        let mut wsum = 0.0;
        for y in 0..self.height {
            let s = (((y as f32 + 0.5) / self.height as f32) * std::f32::consts::PI).sin();
            for x in 0..self.width {
                acc = add(acc, self.rgb[(y * self.width + x) as usize], s);
                wsum += s;
            }
        }
        acc.map(|v| v / wsum.max(1e-6))
    }

    pub fn scaled(mut self, k: f32) -> EnvMap {
        for c in &mut self.rgb {
            *c = c.map(|v| v * k);
        }
        self
    }
}

/// Luminance of a linear colour.
pub fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Mean irradiance an imported HDRI's sky (its sun clipped) is normalised to.
pub const HDRI_IRRADIANCE: f32 = 0.75;

/// Luminance above which an HDRI's pixels count as the sun: well above the typical sky.
fn sun_clip(map: &EnvMap) -> f32 {
    let small = map.downsample(64);
    let mut l: Vec<f32> = small.rgb.iter().map(|c| luminance(*c)).collect();
    l.sort_by(f32::total_cmp);
    let median = l[l.len() / 2];
    let high = l[l.len() * 9 / 10];
    (median * 30.0).max(high * 3.0).max(1e-4)
}

/// Mean of a panorama with everything above `cap` luminance pulled down to it.
fn clipped_mean(map: &EnvMap, cap: f32) -> f32 {
    let mut m = map.downsample(128);
    for c in &mut m.rgb {
        let l = luminance(*c);
        if l > cap {
            *c = c.map(|v| v * cap / l);
        }
    }
    luminance(m.mean())
}

/// The panorama a scene's environment shows, before rotation and intensity: built-in skies
/// are drawn, an HDRI is decoded from its asset and normalised. `None` for the studio.
pub fn resolve(
    env: &Environment,
    assets: &std::collections::BTreeMap<String, crate::Asset>,
    width: u32,
) -> Option<EnvMap> {
    match env.sky {
        Sky::Studio => None,
        Sky::Hdri => {
            let a = assets.get(env.hdri.as_deref()?)?;
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD.decode(a.data.as_bytes()).ok()?;
            let map = decode_panorama(&bytes, width).ok()?;
            let cap = sun_clip(&map);
            let k = HDRI_IRRADIANCE / (std::f32::consts::PI * clipped_mean(&map, cap).max(1e-6));
            let tint = env.color;
            let map = map.scaled(k);
            Some(EnvMap {
                rgb: map.rgb.iter().map(|c| [c[0] * tint[0], c[1] * tint[1], c[2] * tint[2]]).collect(),
                clip: Some(cap * k),
                ..map
            })
        }
        s => Some(procedural(s, env.color, width, true)),
    }
}

/// The small panorama the sky's light is worked out from: built-in skies without their sun
/// disc, HDRIs with their sun clipped (a sun light stands in for it, see
/// `light.fromEnvironment`), so the ambient light does not wash everything out.
pub fn lighting_map(env: &Environment, full: &EnvMap) -> EnvMap {
    match env.sky {
        Sky::Studio | Sky::Hdri => {
            let mut m = full.downsample(128);
            let cap = full.clip.unwrap_or(f32::MAX);
            for c in &mut m.rgb {
                let l = luminance(*c);
                if l > cap {
                    *c = c.map(|v| v * cap / l);
                }
            }
            m
        }
        s => procedural(s, env.color, 128, false),
    }
}

/// What the renderer needs from the sky: ambient light and its main direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvLighting {
    /// Irradiance as 9 spherical-harmonic coefficients (cosine lobe already applied), in
    /// world space (rotation applied), scaled by the intensity.
    pub sh: [[f32; 3]; 9],
    /// World direction towards the brightest part of the sky.
    pub key_dir: [f32; 3],
    /// Colour of the brightest part, normalised to luminance 1.
    pub key_color: [f32; 3],
    /// 0 = light from all round (no shadow), 1 = one bright spot (a sun).
    pub directionality: f32,
}

/// Turn a panorama direction into the world (the sky is turned `rotation` degrees about +Y).
pub fn env_to_world(rotation: f32) -> Quat {
    Quat::from_rotation_y(rotation.to_radians())
}

fn sh_basis(d: Vec3) -> [f32; 9] {
    let (x, y, z) = (d.x, d.y, d.z);
    [
        0.282_095,
        0.488_603 * y,
        0.488_603 * z,
        0.488_603 * x,
        1.092_548 * x * y,
        1.092_548 * y * z,
        0.315_392 * (3.0 * z * z - 1.0),
        1.092_548 * x * z,
        0.546_274 * (x * x - y * y),
    ]
}

/// Irradiance at normal `n` from coefficients made by [`lighting`].
pub fn irradiance(sh: &[[f32; 3]; 9], n: Vec3) -> [f32; 3] {
    let b = sh_basis(n.normalize_or(Vec3::Y));
    let mut out = [0.0f32; 3];
    for (c, w) in sh.iter().zip(b) {
        out = add(out, *c, w);
    }
    out.map(|v| v.max(0.0))
}

/// Ambient light of a panorama (best given a small one, ~128 px wide).
pub fn lighting(map: &EnvMap, rotation: f32, intensity: f32) -> EnvLighting {
    let rot = env_to_world(rotation);
    let mut sh = [[0.0f32; 3]; 9];
    let (w, h) = (map.width, map.height);
    let dphi = std::f32::consts::TAU / w as f32;
    let dtheta = std::f32::consts::PI / h as f32;
    let mut best = (f32::MIN, Vec3::Y, [1.0f32; 3]);
    for y in 0..h {
        let v = (y as f32 + 0.5) / h as f32;
        let d_omega = dphi * dtheta * (v * std::f32::consts::PI).sin();
        for x in 0..w {
            let c = map.rgb[(y * w + x) as usize];
            let d = rot * dir_of((x as f32 + 0.5) / w as f32, v);
            for (k, b) in sh_basis(d).into_iter().enumerate() {
                sh[k] = add(sh[k], c, b * d_omega);
            }
            let l = luminance(c);
            if l > best.0 {
                best = (l, d, c);
            }
        }
    }
    // cosine-lobe convolution: band 0 × π, band 1 × 2π/3, band 2 × π/4
    let band = [std::f32::consts::PI, 2.0 * std::f32::consts::PI / 3.0, std::f32::consts::PI / 4.0];
    let l1 = Vec3::new(luminance(sh[3]), luminance(sh[1]), luminance(sh[2]));
    let l0 = luminance(sh[0]).max(1e-6);
    // a single spot of light has |L1| / L0 = sqrt(3)
    let directionality = (l1.length() / l0 / 3f32.sqrt()).clamp(0.0, 1.0);
    for (k, c) in sh.iter_mut().enumerate() {
        let a = band[if k == 0 {
            0
        } else if k < 4 {
            1
        } else {
            2
        }] * intensity;
        *c = c.map(|v| v * a);
    }
    let key_color = best.2.map(|v| v / luminance(best.2).max(1e-6));
    EnvLighting { sh, key_dir: best.1.to_array(), key_color, directionality }
}

/// The background picture: tone-mapped RGBA8 of the panorama (rotation is left to the viewer).
/// Its exposure does not follow the sky's intensity, so a dimmed sky keeps its backdrop.
pub fn background_rgba(map: &EnvMap) -> Vec<u8> {
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    if let Some(c) = map.flat {
        let px = [to8(c[0]), to8(c[1]), to8(c[2]), 255];
        return px.repeat((map.width * map.height) as usize);
    }
    let mut out = Vec::with_capacity((map.width * map.height * 4) as usize);
    for c in &map.rgb {
        for v in c {
            // exposure, a soft shoulder, then display gamma
            let x = v * 1.8;
            let x = x / (1.0 + x * 0.35);
            out.push(to8(x.powf(1.0 / 2.2)));
        }
        out.push(255);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panorama_directions_round_trip() {
        for (u, v) in [(0.5, 0.5), (0.25, 0.3), (0.9, 0.7), (0.01, 0.5)] {
            let (u2, v2) = uv_of(dir_of(u, v));
            assert!((u - u2).abs() < 1e-4 && (v - v2).abs() < 1e-4, "{u},{v} -> {u2},{v2}");
        }
        assert!(dir_of(0.5, 0.5).distance(Vec3::NEG_Z) < 1e-5);
        assert!(dir_of(0.75, 0.5).distance(Vec3::X) < 1e-5);
        assert!(dir_of(0.5, 0.0).distance(Vec3::Y) < 1e-5);
    }

    #[test]
    fn uniform_sky_gives_even_irradiance() {
        let map = EnvMap { width: 64, height: 32, rgb: vec![[0.5; 3]; 64 * 32], flat: None, clip: None };
        let l = lighting(&map, 0.0, 1.0);
        for n in [Vec3::Y, Vec3::X, Vec3::NEG_Y, Vec3::new(0.3, -0.5, 0.8)] {
            let e = irradiance(&l.sh, n);
            // irradiance of uniform radiance L is π L
            assert!((e[0] - 0.5 * std::f32::consts::PI).abs() < 0.03, "{e:?}");
        }
        assert!(l.directionality < 0.05);
    }

    #[test]
    fn sun_sky_points_at_its_sun_and_rotates() {
        let map = procedural(Sky::Day, [1.0; 3], 256, false);
        let l = lighting(&map, 0.0, 1.0);
        let sun = sun_dir(Sky::Day);
        assert!(Vec3::from_array(l.key_dir).angle_between(sun).to_degrees() < 3.0);
        assert!(l.directionality > 0.3, "{}", l.directionality);
        // the sky lights the top more than the bottom
        assert!(irradiance(&l.sh, Vec3::Y)[2] > irradiance(&l.sh, Vec3::NEG_Y)[2]);
        let r = lighting(&map, 90.0, 1.0);
        let want = Quat::from_rotation_y(90f32.to_radians()) * sun;
        assert!(Vec3::from_array(r.key_dir).angle_between(want).to_degrees() < 3.0);
    }

    #[test]
    fn hdr_panoramas_decode() {
        let mut img = image::Rgb32FImage::new(32, 16);
        img.put_pixel(8, 4, image::Rgb([50.0, 40.0, 30.0]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb32F(img).write_to(&mut bytes, image::ImageFormat::Hdr).unwrap();
        let map = decode_panorama(bytes.get_ref(), 16).unwrap();
        assert_eq!((map.width, map.height), (16, 8));
        let l = lighting(&map, 0.0, 1.0);
        let want = dir_of(8.5 / 32.0, 4.5 / 16.0);
        assert!(Vec3::from_array(l.key_dir).angle_between(want).to_degrees() < 15.0);
        assert!(decode_panorama(b"nope", 16).is_err());
    }
}
