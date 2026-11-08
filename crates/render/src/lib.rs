//! wgpu renderer. Draws into its own offscreen colour + depth target, so the same code feeds
//! the editor viewport (shown as an egui texture) and frame exports.
//!
//! Only features available on WebGL2 are used, so the web build falls back cleanly when
//! WebGPU is missing.

pub mod geometry;

use glam::Mat4;
use shotstage_core::Scene;
use wgpu::util::DeviceExt as _;

use geometry::{ImageVertex, LineVertex, TexVertex, Vertex};
pub use geometry::{Overlay, pick};

/// Colour format of the offscreen target.
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// What the meshes are shaded with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pass {
    /// Clay shading (the default look of a previs reference).
    #[default]
    Clay = 0,
    /// Linear depth, near = white, far = black (ControlNet / Wan depth convention).
    Depth = 1,
    /// View-space normals mapped to RGB.
    Normal = 2,
}

/// Camera used for one render.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub view: Mat4,
    pub proj: Mat4,
    /// Depth range mapped to white..black in the depth pass. Set `depth_far <= depth_near`
    /// to fit the range to the visible geometry each frame.
    pub depth_near: f32,
    pub depth_far: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [f32; 16],
    view: [f32; 16],
    /// x = pass, y = depth near, z = depth far, w = 1 when overlays dim with depth
    params: [f32; 4],
    /// x = number of scene lights, y = studio ambient, z = 1 when the sky lights the scene,
    /// w = how much the ambient shadow darkens the sky light
    light_params: [f32; 4],
    /// Clip to world, for the background's view rays.
    inv_view_proj: [f32; 16],
    /// x = 1 when the sky is drawn behind the scene, y = sky rotation (radians), z = shadow
    /// slot that darkens the ambient / studio light (-1 none)
    env_params: [f32; 4],
    /// Sky irradiance, 9 spherical-harmonic coefficients (rgb).
    sh: [[f32; 4]; 9],
    /// x = shadows on, y = strength, z = blur radius in atlas texels, w = atlas texel size
    shadow_params: [f32; 4],
    /// World to shadow clip, per shadow slot.
    shadow_vp: [[f32; 16]; MAX_SHADOWS],
    /// Where each slot sits in the atlas: xy = offset, z = scale, w = depth bias.
    shadow_tile: [[f32; 4]; MAX_SHADOWS],
    /// x = how far to push a point off its surface before the lookup (metres)
    shadow_bias: [[f32; 4]; MAX_SHADOWS],
    lights: [GpuLight; MAX_LIGHTS],
}

/// Shadow atlas size in texels: one light gets all of it, more share it as a 2×2 grid. (2048
/// is the largest texture WebGL2 guarantees.)
const SHADOW_SIZE: u32 = 2048;

/// Most lights that cast shadows at once (the brightest win).
pub const MAX_SHADOWS: usize = 4;

/// Most lights the clay pass shades with (the brightest are kept).
pub const MAX_LIGHTS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLight {
    /// xyz = position, w = kind (0 sun, 1 point, 2 spot, 3 area)
    pos: [f32; 4],
    /// xyz = direction it shines, w = cos of the outer cone half angle
    dir: [f32; 4],
    /// rgb = colour × intensity, w = cos of the inner cone half angle
    rad: [f32; 4],
    /// x = shadow slot (-1 none), y = extra blur in texels (big lights cast softer shadows)
    shadow: [f32; 4],
}

/// The shadow maps to draw this frame.
#[derive(Default)]
struct ShadowPlan {
    /// World -> shadow clip, and the size of one full-atlas texel at the subject (metres).
    slots: Vec<(Mat4, f32)>,
    /// Slot whose shadow darkens the ambient light (studio key or the sky's sun), and how much.
    ambient: Option<(usize, f32)>,
}

/// Choose the shadow casters: the brightest lights that cast shadows (up to four); with none,
/// the studio key or the sky's brightest direction darkens the ambient light instead.
fn shadow_plan(
    geo: &geometry::Geometry,
    lights: &mut [GpuLight],
    casts: &[bool],
    ambient_dir: Option<(glam::Vec3, f32)>,
) -> ShadowPlan {
    use glam::Vec3;
    let mut plan = ShadowPlan::default();
    let mut it = geo.solid_positions().map(Vec3::from_array);
    let Some(first) = it.next() else { return plan };
    let (lo, hi) = it.fold((first, first), |(a, b), p| (a.min(p), b.max(p)));
    let (center, radius) = ((lo + hi) / 2.0, ((hi - lo).length() / 2.0).max(0.5));
    let up_for = |d: Vec3| if d.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let ortho = |dir: Vec3| {
        let eye = center - dir * (radius * 2.0 + 1.0);
        let view = Mat4::look_at_rh(eye, center, up_for(dir));
        let proj = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.01, radius * 4.0 + 2.0);
        proj * view
    };
    for (l, cast) in lights.iter_mut().zip(casts) {
        l.shadow[0] = -1.0;
        if !cast || plan.slots.len() >= MAX_SHADOWS || l.rad[0] + l.rad[1] + l.rad[2] <= 0.0 {
            continue;
        }
        let dir = Vec3::new(l.dir[0], l.dir[1], l.dir[2]).normalize_or(Vec3::NEG_Y);
        let pos = Vec3::new(l.pos[0], l.pos[1], l.pos[2]);
        let texel = |width: f32| width / SHADOW_SIZE as f32;
        let vp = match l.pos[3] as u32 {
            0 => (ortho(dir), texel(radius * 2.0)),
            kind => {
                // look at the middle of the scene, just wide enough to see all of it; a spot
                // whose beam is narrower than that looks along its beam instead
                let to = (center - pos).normalize_or(Vec3::NEG_Y);
                let d = (center - pos).length().max(1e-3);
                let reach = if d > radius { (radius / d).asin() * 1.1 } else { 1.3 };
                let beam = l.dir[3].clamp(-1.0, 1.0).acos();
                let (look, half) = if kind == 2 && beam < reach { (dir, beam) } else { (to, reach.max(0.2)) };
                let fov = (half * 2.0).clamp(0.2, 2.6);
                let dist = (pos - center).length();
                let far = dist + radius + 1.0;
                let near = ((dist - radius) * 0.8).clamp(0.05, far * 0.5);
                let view = Mat4::look_at_rh(pos, pos + look, up_for(look));
                (Mat4::perspective_rh(fov, 1.0, near, far) * view, texel(2.0 * dist.max(0.5) * (fov / 2.0).tan()))
            }
        };
        l.shadow[0] = plan.slots.len() as f32;
        plan.slots.push(vp);
    }
    if plan.slots.is_empty()
        && let Some((dir, amount)) = ambient_dir
    {
        plan.ambient = Some((0, amount));
        plan.slots.push((ortho(dir), radius * 2.0 / SHADOW_SIZE as f32));
    }
    plan
}

/// The scene's lights, brightest first, with whether each casts shadows.
fn gpu_lights(scene: &Scene, frame: f32) -> (usize, [GpuLight; MAX_LIGHTS], [bool; MAX_LIGHTS]) {
    use shotstage_core::LightKind;
    let mut ls = shotstage_core::eval::lights_at(scene, frame);
    ls.sort_by(|a, b| b.radiance.iter().sum::<f32>().total_cmp(&a.radiance.iter().sum::<f32>()));
    let mut out = [GpuLight::default(); MAX_LIGHTS];
    let mut casts = [false; MAX_LIGHTS];
    for ((o, c), l) in out.iter_mut().zip(casts.iter_mut()).zip(&ls) {
        let kind = match l.kind {
            LightKind::Sun => 0.0,
            LightKind::Point => 1.0,
            LightKind::Spot => 2.0,
            LightKind::Area => 3.0,
        };
        let [px, py, pz] = l.position;
        let [dx, dy, dz] = l.direction;
        let [r, g, b] = l.radiance;
        *o = GpuLight {
            pos: [px, py, pz, kind],
            dir: [dx, dy, dz, l.cos_outer],
            rad: [r, g, b, l.cos_inner],
            shadow: [-1.0, (l.size * 4.0).min(12.0), 0.0, 0.0],
        };
        *c = l.cast_shadows;
    }
    (ls.len().min(MAX_LIGHTS), out, casts)
}

/// The sky as the GPU sees it: the background texture, and its lighting at the last rotation.
struct EnvCache {
    key: String,
    /// Small copy for the lighting maths.
    small: shotstage_core::environment::EnvMap,
    background: wgpu::BindGroup,
    lighting: Option<((f32, f32), shotstage_core::environment::EnvLighting)>,
}

const SHADER: &str = r#"
struct L { pos: vec4<f32>, dir: vec4<f32>, rad: vec4<f32>, shadow: vec4<f32> };
struct U {
    view_proj: mat4x4<f32>, view: mat4x4<f32>, params: vec4<f32>, light_params: vec4<f32>,
    inv_view_proj: mat4x4<f32>, env_params: vec4<f32>, sh: array<vec4<f32>, 9>,
    shadow_params: vec4<f32>, shadow_vp: array<mat4x4<f32>, 4>, shadow_tile: array<vec4<f32>, 4>,
    shadow_bias: array<vec4<f32>, 4>,
    lights: array<L, 8>,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var shadow_tex: texture_depth_2d;
@group(0) @binding(2) var shadow_smp: sampler_comparison;

// Angle the shadow blur disc is turned by at this pixel.
var<private> spin: f32 = 0.0;

// How much of a shadow slot's light reaches a point: 1 = lit, 0 = in shadow. Edges are
// blurred over a disc of `extra` + the scene's softness texels.
fn shadow_vis(wp: vec3<f32>, n: vec3<f32>, slot: i32, extra: f32, ndl: f32) -> f32 {
    if (slot < 0 || u.shadow_params.x < 0.5) {
        return 1.0;
    }
    let tile = u.shadow_tile[slot];
    // look up a little off the surface (a few shadow-map texels, more where the light grazes
    // it) so it does not shadow itself
    let push = u.shadow_bias[slot].x * (1.0 + extra * 0.25) * (1.0 + 4.0 * (1.0 - clamp(ndl, 0.0, 1.0)));
    let lp = u.shadow_vp[slot] * vec4<f32>(wp + n * push, 1.0);
    let ndc = lp.xyz / max(lp.w, 1e-5);
    let local = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let inside = local.x > 0.0 && local.x < 1.0 && local.y > 0.0 && local.y < 1.0 && ndc.z < 1.0 && lp.w > 0.0;
    if (!inside) {
        return 1.0;
    }
    let texel = u.shadow_params.w;
    let uv = tile.xy + local * tile.z;
    let lo = tile.xy + vec2<f32>(texel);
    let hi = tile.xy + vec2<f32>(tile.z - texel);
    let r = (u.shadow_params.z + extra) * texel;
    let depth = ndc.z - tile.w;
    var disc = array<vec2<f32>, 12>(
        vec2<f32>(-0.326, -0.406), vec2<f32>(-0.840, -0.074), vec2<f32>(-0.696, 0.457),
        vec2<f32>(-0.203, 0.621), vec2<f32>(0.962, -0.195), vec2<f32>(0.473, -0.480),
        vec2<f32>(0.519, 0.767), vec2<f32>(0.185, -0.893), vec2<f32>(0.507, 0.064),
        vec2<f32>(0.896, 0.412), vec2<f32>(-0.322, -0.933), vec2<f32>(-0.792, -0.598),
    );
    let rot = mat2x2<f32>(cos(spin), sin(spin), -sin(spin), cos(spin));
    var sum = 0.0;
    for (var k = 0; k < 12; k = k + 1) {
        sum = sum + textureSampleCompareLevel(shadow_tex, shadow_smp, clamp(uv + rot * disc[k] * r, lo, hi), depth);
    }
    return mix(1.0, sum / 12.0, u.shadow_params.y);
}

// Light from the sky (spherical harmonics), or the plain studio ambient.
fn ambient(n: vec3<f32>) -> vec3<f32> {
    if (u.light_params.z < 0.5) {
        return vec3<f32>(u.light_params.y * (0.75 + 0.25 * n.y));
    }
    let x = n.x;
    let y = n.y;
    let z = n.z;
    var e = u.sh[0].rgb * 0.282095;
    e = e + u.sh[1].rgb * (0.488603 * y) + u.sh[2].rgb * (0.488603 * z) + u.sh[3].rgb * (0.488603 * x);
    e = e + u.sh[4].rgb * (1.092548 * x * y) + u.sh[5].rgb * (1.092548 * y * z);
    e = e + u.sh[6].rgb * (0.315392 * (3.0 * z * z - 1.0)) + u.sh[7].rgb * (1.092548 * x * z);
    e = e + u.sh[8].rgb * (0.546274 * (x * x - y * y));
    return max(e, vec3<f32>(0.0));
}

struct MeshIn { @location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec3<f32>, @location(3) id: u32 };
struct MeshOut { @builtin(position) clip: vec4<f32>, @location(0) n: vec3<f32>, @location(1) c: vec3<f32>, @location(2) vz: f32, @location(3) wp: vec3<f32>, @location(4) @interpolate(flat) mat: u32 };

@vertex fn vs_mesh(v: MeshIn) -> MeshOut {
    var o: MeshOut;
    o.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    o.n = v.normal;
    o.c = v.color;
    o.vz = -(u.view * vec4<f32>(v.pos, 1.0)).z;
    o.wp = v.pos;
    o.mat = v.id >> 28u;
    return o;
}

// Surface looks (top bits of the vertex id): 0 matte, 1 skin, 2 hair, 3 eyes.
// Skin lets light wrap past the terminator, warmer in red (a cheap subsurface look).
fn diffuse(n: vec3<f32>, l: vec3<f32>, mat: u32) -> vec3<f32> {
    let d = dot(n, l);
    let lam = max(d, 0.0);
    if (mat == 1u) {
        let wrap = max((d + 0.4) / 1.4, 0.0);
        return mix(vec3<f32>(lam), vec3<f32>(wrap), vec3<f32>(0.6, 0.38, 0.32));
    }
    return vec3<f32>(lam);
}

// A soft sheen on skin, a tighter highlight on hair, a wet glint on eyes.
fn specular(n: vec3<f32>, l: vec3<f32>, v: vec3<f32>, mat: u32) -> f32 {
    if (mat == 0u || dot(n, l) <= 0.0) {
        return 0.0;
    }
    let nh = max(dot(n, normalize(l + v)), 0.0);
    if (mat == 1u) {
        return 0.045 * pow(nh, 18.0);
    }
    if (mat == 2u) {
        return 0.09 * pow(nh, 40.0) + 0.03 * pow(nh, 8.0);
    }
    return 0.5 * pow(nh, 140.0);
}

fn eye_pos() -> vec3<f32> {
    let r = mat3x3<f32>(u.view[0].xyz, u.view[1].xyz, u.view[2].xyz);
    return -(transpose(r) * u.view[3].xyz);
}

fn lit(c: vec3<f32>, n: vec3<f32>, wp: vec3<f32>, mat: u32, px: vec2<f32>) -> vec4<f32> {
    // turn the blur disc per pixel, so soft edges dither instead of showing copies
    spin = 6.2831853 * fract(52.9829189 * fract(dot(floor(px), vec2<f32>(0.06711056, 0.00583715))));
    // the shadow of the studio key, or of the sky's sun when no light casts one
    let vis = shadow_vis(wp, n, i32(u.env_params.z), 0.0, 0.5);
    let count = u32(u.light_params.x + 0.5);
    let v = normalize(eye_pos() - wp);
    var spec = vec3<f32>(0.0);
    if (count > 0u || u.light_params.z > 0.5) {
        // sky (or soft studio) ambient plus each light's term, shadowed where it casts
        var col = c * ambient(n) * mix(1.0, vis, u.light_params.w);
        for (var k = 0u; k < count; k = k + 1u) {
            let l = u.lights[k];
            let kind = u32(l.pos.w + 0.5);
            var ldir = -l.dir.xyz;
            var att = 1.0;
            if (kind != 0u) {
                let d = l.pos.xyz - wp;
                let dist2 = max(dot(d, d), 0.01);
                ldir = d * inverseSqrt(dist2);
                // brightness 1 at 2 m, falling off with the square of the distance
                att = 4.0 / dist2;
                if (kind >= 2u) {
                    att = att * smoothstep(l.dir.w, l.rad.w, dot(-ldir, l.dir.xyz));
                }
            }
            var ndl = diffuse(n, ldir, mat);
            if (kind == 3u && mat != 1u) {
                ndl = vec3<f32>(max((dot(n, ldir) + 0.35) / 1.35, 0.0));
            }
            if (att > 0.0) {
                att = att * shadow_vis(wp, n, i32(l.shadow.x), l.shadow.y, dot(n, ldir));
            }
            col = col + c * l.rad.rgb * ndl * att;
            col = col + l.rad.rgb * specular(n, ldir, v, mat) * att;
        }
        // roll highlights off softly instead of clipping to white
        let knee = vec3<f32>(0.75);
        let over = max(col - knee, vec3<f32>(0.0));
        col = min(col, knee) + over / (vec3<f32>(1.0) + over * 4.0);
        return vec4<f32>(col, 1.0);
    }
    let key = normalize(vec3<f32>(0.45, 0.8, 0.4));
    let fill = normalize(vec3<f32>(-0.6, 0.25, -0.5));
    // the studio key casts a soft shadow (so things sit on the ground)
    let key_vis = mix(1.0, vis, 0.7);
    let light = vec3<f32>(0.28 + 0.08 * max(n.y, 0.0)) + 0.62 * diffuse(n, key, mat) * key_vis + 0.18 * diffuse(n, fill, mat);
    spec = vec3<f32>(0.62 * key_vis * specular(n, key, v, mat) + 0.18 * specular(n, fill, v, mat));
    return vec4<f32>(c * light + spec, 1.0);
}

@fragment fn fs_mesh(i: MeshOut) -> @location(0) vec4<f32> {
    let n = normalize(i.n);
    let pass_id = u32(u.params.x + 0.5);
    if (pass_id == 1u) {
        let d = clamp((i.vz - u.params.y) / max(u.params.z - u.params.y, 1e-4), 0.0, 1.0);
        let g = 1.0 - d;
        return vec4<f32>(g, g, g, 1.0);
    }
    if (pass_id == 2u) {
        let vn = normalize((u.view * vec4<f32>(n, 0.0)).xyz);
        return vec4<f32>(vn * 0.5 + vec3<f32>(0.5), 1.0);
    }
    return lit(i.c, n, i.wp, i.mat, i.clip.xy);
}

@group(1) @binding(0) var tex_tex: texture_2d<f32>;
@group(1) @binding(1) var tex_smp: sampler;

struct TexIn { @location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) uv: vec2<f32>, @location(3) color: vec3<f32> };
struct TexOut { @builtin(position) clip: vec4<f32>, @location(0) n: vec3<f32>, @location(1) c: vec3<f32>, @location(2) vz: f32, @location(3) wp: vec3<f32>, @location(4) uv: vec2<f32> };

@vertex fn vs_tex(v: TexIn) -> TexOut {
    var o: TexOut;
    o.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    o.n = v.normal;
    o.c = v.color;
    o.vz = -(u.view * vec4<f32>(v.pos, 1.0)).z;
    o.wp = v.pos;
    o.uv = v.uv;
    return o;
}

@fragment fn fs_tex(i: TexOut) -> @location(0) vec4<f32> {
    let t = textureSample(tex_tex, tex_smp, i.uv);
    // see-through pixels of a cut-out texture are holes
    if (t.a < 0.5) {
        discard;
    }
    let n = normalize(i.n);
    let pass_id = u32(u.params.x + 0.5);
    if (pass_id == 1u) {
        let d = clamp((i.vz - u.params.y) / max(u.params.z - u.params.y, 1e-4), 0.0, 1.0);
        let g = 1.0 - d;
        return vec4<f32>(g, g, g, 1.0);
    }
    if (pass_id == 2u) {
        let vn = normalize((u.view * vec4<f32>(n, 0.0)).xyz);
        return vec4<f32>(vn * 0.5 + vec3<f32>(0.5), 1.0);
    }
    return lit(t.rgb * i.c, n, i.wp, 0u, i.clip.xy);
}

@group(1) @binding(0) var img_tex: texture_2d<f32>;
@group(1) @binding(1) var img_smp: sampler;

struct ImgIn { @location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) uv: vec2<f32>, @location(3) tint: vec3<f32> };
struct ImgOut { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) tint: vec3<f32>, @location(2) vz: f32, @location(3) n: vec3<f32> };

@vertex fn vs_img(v: ImgIn) -> ImgOut {
    var o: ImgOut;
    o.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    o.uv = v.uv;
    o.tint = v.tint;
    o.vz = -(u.view * vec4<f32>(v.pos, 1.0)).z;
    o.n = v.normal;
    return o;
}

@fragment fn fs_img(i: ImgOut) -> @location(0) vec4<f32> {
    let c = textureSample(img_tex, img_smp, i.uv);
    // transparent pixels of a cut-out are holes
    if (c.a < 0.5) {
        discard;
    }
    let pass_id = u32(u.params.x + 0.5);
    if (pass_id == 1u) {
        let d = clamp((i.vz - u.params.y) / max(u.params.z - u.params.y, 1e-4), 0.0, 1.0);
        return vec4<f32>(vec3<f32>(1.0 - d), 1.0);
    }
    if (pass_id == 2u) {
        let vn = normalize((u.view * vec4<f32>(normalize(i.n), 0.0)).xyz);
        return vec4<f32>(vn * 0.5 + vec3<f32>(0.5), 1.0);
    }
    // pictures are shown as they are, not shaded
    return vec4<f32>(min(c.rgb * i.tint, vec3<f32>(1.0)), 1.0);
}

struct LineIn { @location(0) pos: vec3<f32>, @location(1) color: vec3<f32> };
struct LineOut { @builtin(position) clip: vec4<f32>, @location(0) c: vec3<f32> };

@vertex fn vs_line(v: LineIn) -> LineOut {
    var o: LineOut;
    o.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    o.c = v.color;
    return o;
}

@fragment fn fs_line(i: LineOut) -> @location(0) vec4<f32> {
    return vec4<f32>(i.c, 1.0);
}

// The sky behind everything: a full-screen triangle looking up the panorama along view rays.
struct BgOut { @builtin(position) clip: vec4<f32>, @location(0) ndc: vec2<f32> };

@vertex fn vs_bg(@builtin(vertex_index) i: u32) -> BgOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - vec2<f32>(1.0);
    var o: BgOut;
    o.clip = vec4<f32>(p, 1.0, 1.0);
    o.ndc = p;
    return o;
}

@fragment fn fs_bg(i: BgOut) -> @location(0) vec4<f32> {
    let a = u.inv_view_proj * vec4<f32>(i.ndc, 0.0, 1.0);
    let b = u.inv_view_proj * vec4<f32>(i.ndc, 1.0, 1.0);
    let d = normalize(b.xyz / b.w - a.xyz / a.w);
    // into panorama space (the sky is turned by env_params.y about +Y)
    let r = -u.env_params.y;
    let e = vec3<f32>(d.x * cos(r) + d.z * sin(r), d.y, -d.x * sin(r) + d.z * cos(r));
    let uv = vec2<f32>(atan2(e.x, -e.z) / 6.2831853 + 0.5, acos(clamp(e.y, -1.0, 1.0)) / 3.1415927);
    return vec4<f32>(textureSampleLevel(img_tex, img_smp, uv, 0.0).rgb, 1.0);
}
"#;

/// Depth-only pass for one shadow slot.
const SHADOW_SHADER: &str = r#"
@group(0) @binding(0) var<uniform> m: mat4x4<f32>;
@vertex fn vs_shadow(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    return m * vec4<f32>(pos, 1.0);
}
"#;

/// Auto depth range: from the camera (white) to the farthest geometry in front of it (black).
/// Vertex positions are a cheap stand-in for visible surfaces; a vertex behind the camera can
/// belong to a visible face, so the near end stays at the camera.
pub fn depth_range(geo: &geometry::Geometry, view: Mat4) -> (f32, f32) {
    let far = geo.solid_positions().map(|p| -view.transform_point3(glam::Vec3::from_array(p)).z).fold(0.0f32, f32::max);
    (0.0, if far > 0.01 { far } else { 10.0 })
}

struct Target {
    size: (u32, u32),
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
}

pub struct Renderer {
    mesh_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    /// One small uniform (the light's matrix) per shadow slot.
    shadow_slots: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    shadow_view: wgpu::TextureView,
    bg_pipeline: wgpu::RenderPipeline,
    env: Option<EnvCache>,
    line_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    /// Textured meshes: lit like plain meshes, coloured by their texture.
    tex_pipeline: wgpu::RenderPipeline,
    repeat_sampler: wgpu::Sampler,
    image_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// GPU textures of image assets: key -> (identity of the pixels, bind group).
    textures: std::collections::HashMap<String, (usize, wgpu::BindGroup, wgpu::BindGroup)>,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: Option<Target>,
    /// Bumps when the target texture is recreated (callers re-register it).
    pub target_generation: u64,
}

impl Renderer {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shotstage"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shotstage uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shotstage"),
            entries: &[
                uniform_entry,
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        // the shadow pass binds only the light's matrix (the shadow map is its target)
        let shadow_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shotstage shadow"),
            entries: &[wgpu::BindGroupLayoutEntry { visibility: wgpu::ShaderStages::VERTEX, ..uniform_entry }],
        });
        let shadow_map = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shotstage shadow map"),
            size: wgpu::Extent3d { width: SHADOW_SIZE, height: SHADOW_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow_map.create_view(&Default::default());
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shotstage shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shotstage"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&shadow_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&shadow_sampler) },
            ],
        });
        let shadow_slots = (0..MAX_SHADOWS)
            .map(|_| {
                let buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("shotstage shadow slot"),
                    size: 64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shotstage shadow"),
                    layout: &shadow_bgl,
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
                });
                (buf, bg)
            })
            .collect();
        let shadow_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shotstage shadow"),
            source: wgpu::ShaderSource::Wgsl(SHADOW_SHADER.into()),
        });
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shotstage shadow"),
            bind_group_layouts: &[Some(&shadow_bgl)],
            immediate_size: 0,
        });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vs_shadow"),
            layout: Some(&shadow_layout),
            vertex: wgpu::VertexState {
                module: &shadow_module,
                entry_point: Some("vs_shadow"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                })],
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.0, clamp: 0.0 },
            }),
            multisample: Default::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shotstage"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let mesh_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Uint32];
        let line_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
        let pipeline = |vs: &str, fs: &str, topology, layout_desc: wgpu::VertexBufferLayout<'_>, cull| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(layout_desc)],
                },
                primitive: wgpu::PrimitiveState { topology, cull_mode: cull, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let mesh_pipeline = pipeline(
            "vs_mesh",
            "fs_mesh",
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &mesh_attrs,
            },
            Some(wgpu::Face::Back),
        );
        let line_pipeline = pipeline(
            "vs_line",
            "fs_line",
            wgpu::PrimitiveTopology::LineList,
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<LineVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &line_attrs,
            },
            None,
        );
        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shotstage image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let image_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shotstage image"),
            bind_group_layouts: &[Some(&bgl), Some(&image_layout)],
            immediate_size: 0,
        });
        let img_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x3];
        let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vs_img"),
            layout: Some(&image_pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_img"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ImageVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &img_attrs,
                })],
            },
            // cards are seen from both sides
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_img"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shotstage image"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let repeat_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shotstage texture"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let tex_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x3];
        let tex_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vs_tex"),
            layout: Some(&image_pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_tex"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TexVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &tex_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState { cull_mode: Some(wgpu::Face::Back), ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_tex"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vs_bg"),
            layout: Some(&image_pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_bg"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_bg"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            mesh_pipeline,
            shadow_pipeline,
            shadow_slots,
            shadow_view,
            bg_pipeline,
            env: None,
            line_pipeline,
            image_pipeline,
            tex_pipeline,
            image_layout,
            sampler,
            repeat_sampler,
            textures: Default::default(),
            uniform,
            bind_group,
            target: None,
            target_generation: 0,
        }
    }

    /// Upload the textures of image assets that are new or changed.
    fn sync_textures(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene, used: &[&str]) {
        use base64::Engine as _;
        for key in used {
            let Some(asset) = scene.assets.get(*key) else { continue };
            // the pixels are shared (`Arc`), so their address identifies them
            let identity = asset.data.as_ptr() as usize ^ asset.data.len();
            if self.textures.get(*key).is_some_and(|(id, _, _)| *id == identity) {
                continue;
            }
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(asset.data.as_bytes()) else { continue };
            let Ok(img) = image::load_from_memory(&bytes) else { continue };
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width().clamp(1, 4096), rgba.height().clamp(1, 4096));
            let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shotstage image"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * rgba.width()), rows_per_image: None },
                wgpu::Extent3d { width: rgba.width(), height: rgba.height(), depth_or_array_layers: 1 },
            );
            let view = tex.create_view(&Default::default());
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shotstage image"),
                layout: &self.image_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            // textured meshes tile their picture: a second binding that repeats
            let bg_repeat = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shotstage texture"),
                layout: &self.image_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.repeat_sampler) },
                ],
            });
            self.textures.insert(key.to_string(), (identity, bg, bg_repeat));
        }
        self.textures.retain(|k, _| scene.assets.contains_key(k));
    }

    /// Make the sky's panorama and background texture current; returns its lighting.
    fn sync_env(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
    ) -> Option<shotstage_core::environment::EnvLighting> {
        use shotstage_core::environment as env;
        let e = &scene.environment;
        if !e.lights_scene() {
            return None;
        }
        // an HDRI is known by its pixels too, so replacing the asset re-reads it
        let ident = e.hdri.as_ref().and_then(|k| scene.assets.get(k)).map_or(0, |a| a.data.as_ptr() as usize);
        let key = format!("{}|{ident}", e.source_key());
        if self.env.as_ref().is_none_or(|c| c.key != key) {
            let width = if e.sky == shotstage_core::Sky::Hdri { 2048 } else { 1024 };
            let Some(map) = env::resolve(e, &scene.assets, width) else {
                self.env = None;
                return None;
            };
            let rgba = env::background_rgba(&map);
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shotstage sky"),
                size: wgpu::Extent3d { width: map.width, height: map.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * map.width), rows_per_image: None },
                wgpu::Extent3d { width: map.width, height: map.height, depth_or_array_layers: 1 },
            );
            let view = tex.create_view(&Default::default());
            let background = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shotstage sky"),
                layout: &self.image_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.repeat_sampler) },
                ],
            });
            self.env = Some(EnvCache { key, small: env::lighting_map(e, &map), background, lighting: None });
        }
        let cache = self.env.as_mut()?;
        let want = (e.rotation, e.intensity);
        if cache.lighting.is_none_or(|(k, _)| k != want) {
            cache.lighting = Some((want, env::lighting(&cache.small, e.rotation, e.intensity)));
        }
        cache.lighting.map(|(_, l)| l)
    }

    fn ensure_target(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        let size = (size.0.clamp(1, 8192), size.1.clamp(1, 8192));
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        let tex = |format, usage, label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = tex(
            COLOR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            "shotstage color",
        );
        let depth = tex(DEPTH_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT, "shotstage depth");
        self.target = Some(Target {
            size,
            color_view: color.create_view(&Default::default()),
            depth_view: depth.create_view(&Default::default()),
            color,
        });
        self.target_generation += 1;
    }

    /// The colour target (valid after the first `render`).
    pub fn color_view(&self) -> Option<&wgpu::TextureView> {
        self.target.as_ref().map(|t| &t.color_view)
    }

    pub fn color_texture(&self) -> Option<&wgpu::Texture> {
        self.target.as_ref().map(|t| &t.color)
    }

    /// Render `scene` at `frame` into the offscreen target of `size` pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        frame: f32,
        view: View,
        pass: Pass,
        overlay: &Overlay,
        size: (u32, u32),
    ) {
        self.ensure_target(device, size);
        let mut overlay = overlay.clone();
        overlay.eye = Some(view.view.inverse().w_axis.truncate().to_array());
        let overlay = &overlay;
        let geo = geometry::build(scene, frame, overlay);
        let used: Vec<&str> =
            geo.images.iter().map(|q| q.asset.as_str()).chain(geo.textured.iter().map(|b| b.asset.as_str())).collect();
        self.sync_textures(device, queue, scene, &used);
        let (near, far) = if view.depth_far > view.depth_near {
            (view.depth_near, view.depth_far)
        } else {
            depth_range(&geo, view.view)
        };
        let mut u = Uniforms {
            view_proj: (view.proj * view.view).to_cols_array(),
            view: view.view.to_cols_array(),
            params: [pass as u32 as f32, near, far, 0.0],
            light_params: [0.0, 0.12, 0.0, 0.0],
            inv_view_proj: (view.proj * view.view).inverse().to_cols_array(),
            env_params: [0.0, 0.0, -1.0, 0.0],
            sh: [[0.0; 4]; 9],
            shadow_params: [0.0; 4],
            shadow_vp: [Mat4::IDENTITY.to_cols_array(); MAX_SHADOWS],
            shadow_tile: [[0.0; 4]; MAX_SHADOWS],
            shadow_bias: [[0.0; 4]; MAX_SHADOWS],
            lights: [GpuLight::default(); MAX_LIGHTS],
        };
        let settings = scene.environment.shadows;
        // the studio light toggle shows plain clay: no scene lights, no sky
        let sky = if overlay.studio_light || pass != Pass::Clay { None } else { self.sync_env(device, queue, scene) };
        let mut casts = [false; MAX_LIGHTS];
        let mut count = 0;
        if !overlay.studio_light {
            let (n, lights, c) = gpu_lights(scene, frame);
            (count, u.lights, casts) = (n, lights, c);
            u.light_params[0] = n as f32;
        }
        if let Some(l) = &sky {
            u.light_params[2] = 1.0;
            for (o, c) in u.sh.iter_mut().zip(l.sh) {
                *o = [c[0], c[1], c[2], 0.0];
            }
            u.env_params[0] = if scene.environment.background { 1.0 } else { 0.0 };
            u.env_params[1] = scene.environment.rotation.to_radians();
        }
        // with no lights at all, the studio key (or the sky's sun) shades the ambient light
        let ambient_dir = match &sky {
            _ if count > 0 && casts[..count].iter().any(|c| *c) => None,
            Some(l) if l.directionality > 0.08 => {
                Some((-glam::Vec3::from_array(l.key_dir), (l.directionality * 1.2).min(0.85)))
            }
            Some(_) => None,
            None if count == 0 => Some((-glam::Vec3::new(0.45, 0.8, 0.4).normalize(), 0.7)),
            None => None,
        };
        let plan = if pass == Pass::Clay && !overlay.no_shadows && settings.enabled {
            shadow_plan(&geo, &mut u.lights[..count], &casts[..count], ambient_dir)
        } else {
            for l in &mut u.lights {
                l.shadow[0] = -1.0;
            }
            ShadowPlan::default()
        };
        let tiles = if plan.slots.len() > 1 { 2 } else { 1 };
        let scale = 1.0 / tiles as f32;
        for (i, (vp, texel_m)) in plan.slots.iter().enumerate() {
            u.shadow_vp[i] = vp.to_cols_array();
            let (tx, ty) = ((i % tiles) as f32 * scale, (i / tiles) as f32 * scale);
            // a shared atlas has coarser texels, so push further off the surface
            let off = texel_m * tiles as f32 * (1.5 + settings.softness * 3.0);
            u.shadow_tile[i] = [tx, ty, scale, 0.0];
            u.shadow_bias[i] = [off, 0.0, 0.0, 0.0];
            queue.write_buffer(&self.shadow_slots[i].0, 0, bytemuck::bytes_of(&vp.to_cols_array()));
        }
        if !plan.slots.is_empty() {
            u.shadow_params = [1.0, settings.strength, 0.6 + settings.softness * 7.0, 1.0 / SHADOW_SIZE as f32];
        }
        if let Some((slot, amount)) = plan.ambient {
            u.env_params[2] = slot as f32;
            u.light_params[3] = amount;
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
        let tri_buf = (!geo.triangles.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("shotstage triangles"),
                contents: bytemuck::cast_slice(&geo.triangles),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let line_buf = (!geo.lines.is_empty() && pass == Pass::Clay).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("shotstage lines"),
                contents: bytemuck::cast_slice(&geo.lines),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let tex_verts: Vec<TexVertex> = geo.textured.iter().flat_map(|b| b.vertices.iter().copied()).collect();
        let tex_buf = (!tex_verts.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("shotstage textured"),
                contents: bytemuck::cast_slice(&tex_verts),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        // shadows come from every solid triangle, textured or not
        let shadow_tris: Vec<Vertex> =
            geo.solid_positions().map(|pos| Vertex { pos, normal: [0.0; 3], color: [0.0; 3], id: 0 }).collect();
        let shadow_buf = (!shadow_tris.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("shotstage shadow casters"),
                contents: bytemuck::cast_slice(&shadow_tris),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let img_verts: Vec<ImageVertex> = geo.images.iter().flat_map(|q| q.vertices).collect();
        let img_buf = (!img_verts.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("shotstage images"),
                contents: bytemuck::cast_slice(&img_verts),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let clear = match pass {
            Pass::Clay => wgpu::Color { r: 0.16, g: 0.165, b: 0.18, a: 1.0 },
            Pass::Depth => wgpu::Color::BLACK,
            Pass::Normal => wgpu::Color { r: 0.5, g: 0.5, b: 1.0, a: 1.0 },
        };
        let Some(target) = &self.target else { return };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shotstage") });
        if let (false, Some(b)) = (plan.slots.is_empty(), &shadow_buf) {
            let mut sp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shotstage shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            sp.set_pipeline(&self.shadow_pipeline);
            sp.set_vertex_buffer(0, b.slice(..));
            let side = SHADOW_SIZE as f32 / tiles as f32;
            for i in 0..plan.slots.len() {
                sp.set_viewport((i % tiles) as f32 * side, (i / tiles) as f32 * side, side, side, 0.0, 1.0);
                sp.set_bind_group(0, &self.shadow_slots[i].1, &[]);
                sp.draw(0..shadow_tris.len() as u32, 0..1);
            }
        }
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shotstage"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_bind_group(0, &self.bind_group, &[]);
            if u.env_params[0] > 0.5
                && let Some(env) = &self.env
            {
                rp.set_pipeline(&self.bg_pipeline);
                rp.set_bind_group(1, &env.background, &[]);
                rp.draw(0..3, 0..1);
            }
            if let Some(b) = &tri_buf {
                rp.set_pipeline(&self.mesh_pipeline);
                rp.set_vertex_buffer(0, b.slice(..));
                rp.draw(0..geo.triangles.len() as u32, 0..1);
            }
            if let Some(b) = &tex_buf {
                rp.set_pipeline(&self.tex_pipeline);
                rp.set_vertex_buffer(0, b.slice(..));
                let mut start = 0u32;
                for batch in &geo.textured {
                    let n = batch.vertices.len() as u32;
                    if let Some((_, _, bg)) = self.textures.get(&batch.asset) {
                        rp.set_bind_group(1, bg, &[]);
                        rp.draw(start..start + n, 0..1);
                    }
                    start += n;
                }
            }
            if let Some(b) = &img_buf {
                rp.set_pipeline(&self.image_pipeline);
                rp.set_vertex_buffer(0, b.slice(..));
                for (i, q) in geo.images.iter().enumerate() {
                    if let Some((_, bg, _)) = self.textures.get(&q.asset) {
                        rp.set_bind_group(1, bg, &[]);
                        rp.draw(i as u32 * 6..i as u32 * 6 + 6, 0..1);
                    }
                }
            }
            if let Some(b) = &line_buf {
                rp.set_pipeline(&self.line_pipeline);
                rp.set_vertex_buffer(0, b.slice(..));
                rp.draw(0..geo.lines.len() as u32, 0..1);
            }
        }
        queue.submit([enc.finish()]);
    }
}

/// A copy of the colour target in a mappable buffer (rows padded to 256 bytes).
pub struct Readback {
    pub buffer: wgpu::Buffer,
    pub width: u32,
    pub height: u32,
    padded_row: u32,
}

impl Renderer {
    /// Queue a copy of the last render into a buffer. Map it with `buffer.map_async`, then
    /// call [`Readback::rgba`] once mapped.
    pub fn readback(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Readback> {
        let target = self.target.as_ref()?;
        let (width, height) = target.size;
        let padded_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shotstage readback"),
            size: padded_row as u64 * height as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        Some(Readback { buffer, width, height, padded_row })
    }
}

impl Readback {
    /// Tightly packed RGBA8 rows (the buffer must be mapped); unmaps the buffer.
    pub fn rgba(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity((self.width * self.height * 4) as usize);
        {
            let data = self.buffer.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
            for row in data.chunks(self.padded_row as usize).take(self.height as usize) {
                out.extend_from_slice(&row[..(self.width * 4) as usize]);
            }
        }
        self.buffer.unmap();
        Ok(out)
    }
}

/// Encode RGBA8 pixels as PNG.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

pub fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Overlay for exported frames: the clean camera view, nothing drawn on top.
pub fn clean_overlay(camera: &str) -> Overlay {
    Overlay { looking_through: Some(camera.to_string()), ..Default::default() }
}

/// Render to PNG on the calling thread (native only: blocks until the GPU is done).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub fn render_png_blocking(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &Scene,
    frame: f32,
    view: View,
    pass: Pass,
    overlay: &Overlay,
    size: (u32, u32),
) -> Result<Vec<u8>, String> {
    renderer.render(device, queue, scene, frame, view, pass, overlay, size);
    let rb = renderer.readback(device, queue).ok_or("nothing rendered")?;
    let (tx, rx) = std::sync::mpsc::channel();
    rb.buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    encode_png(&rb.rgba()?, rb.width, rb.height)
}

/// A GPU device without a window, for the CLI and MCP server.
#[cfg(not(target_arch = "wasm32"))]
pub struct Headless {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub renderer: Renderer,
    pub adapter: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl Headless {
    pub fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|e| format!("GPU device: {e}"))?;
        let renderer = Renderer::new(&device);
        Ok(Self { device, queue, renderer, adapter: format!("{} ({:?})", info.name, info.backend) })
    }

    pub fn png(
        &mut self,
        scene: &Scene,
        frame: f32,
        view: View,
        pass: Pass,
        camera: &str,
        size: (u32, u32),
    ) -> Result<Vec<u8>, String> {
        render_png_blocking(
            &mut self.renderer,
            &self.device,
            &self.queue,
            scene,
            frame,
            view,
            pass,
            &clean_overlay(camera),
            size,
        )
    }
}
