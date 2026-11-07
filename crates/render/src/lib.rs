//! wgpu renderer. Draws into its own offscreen colour + depth target, so the same code feeds
//! the editor viewport (shown as an egui texture) and frame exports.
//!
//! Only features available on WebGL2 are used, so the web build falls back cleanly when
//! WebGPU is missing.

pub mod geometry;

use glam::Mat4;
use shotstage_core::Scene;
use wgpu::util::DeviceExt as _;

use geometry::{ImageVertex, LineVertex, Vertex};
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
    /// x = number of scene lights (0 = studio light), y = ambient
    light_params: [f32; 4],
    /// World to the shadow map of the one shadow-casting light.
    shadow_vp: [f32; 16],
    /// x = shadows on, y = depth bias, z = index of the shadow light (-1 = studio key), w = texel
    shadow_params: [f32; 4],
    lights: [GpuLight; MAX_LIGHTS],
}

/// Shadow map size in texels.
const SHADOW_SIZE: u32 = 2048;

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
}

/// The light that casts shadows and its view of the scene: the brightest sun, spot or area
/// light, else the studio key. Returns (world -> shadow clip, light index or -1, depth bias).
fn shadow_setup(geo: &geometry::Geometry, lights: &[GpuLight]) -> Option<(Mat4, f32, f32)> {
    use glam::Vec3;
    let mut it = geo.triangles.iter().map(|v| Vec3::from_array(v.pos));
    let first = it.next()?;
    let (lo, hi) = it.fold((first, first), |(a, b), p| (a.min(p), b.max(p)));
    let (center, radius) = ((lo + hi) / 2.0, ((hi - lo).length() / 2.0).max(0.5));
    // brightest light that has a direction worth shadowing (point lights are skipped)
    let pick = lights
        .iter()
        .enumerate()
        .filter(|(_, l)| l.pos[3] != 1.0)
        .max_by(|a, b| (a.1.rad[0] + a.1.rad[1] + a.1.rad[2]).total_cmp(&(b.1.rad[0] + b.1.rad[1] + b.1.rad[2])));
    let up_for = |d: Vec3| if d.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let ortho = |dir: Vec3| {
        let eye = center - dir * (radius * 2.0 + 1.0);
        let view = Mat4::look_at_rh(eye, center, up_for(dir));
        let proj = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.01, radius * 4.0 + 2.0);
        proj * view
    };
    match pick {
        None if !lights.is_empty() => None,
        None => Some((ortho(-Vec3::new(0.45, 0.8, 0.4).normalize()), -1.0, 0.0015)),
        Some((i, l)) => {
            let dir = Vec3::new(l.dir[0], l.dir[1], l.dir[2]).normalize_or_zero();
            if l.pos[3] == 0.0 {
                return Some((ortho(dir), i as f32, 0.0015));
            }
            // spot and area lights: a perspective view from the light
            let pos = Vec3::new(l.pos[0], l.pos[1], l.pos[2]);
            let half = l.dir[3].clamp(-1.0, 1.0).acos();
            let fov = (half * 2.0 * 1.1).clamp(0.3, 2.8);
            let far = (pos - center).length() + radius + 1.0;
            let view = Mat4::look_at_rh(pos, pos + dir, up_for(dir));
            Some((Mat4::perspective_rh(fov, 1.0, 0.05, far) * view, i as f32, 0.0005))
        }
    }
}

fn gpu_lights(scene: &Scene, frame: f32) -> (usize, [GpuLight; MAX_LIGHTS]) {
    use shotstage_core::LightKind;
    let mut ls = shotstage_core::eval::lights_at(scene, frame);
    ls.sort_by(|a, b| b.radiance.iter().sum::<f32>().total_cmp(&a.radiance.iter().sum::<f32>()));
    let mut out = [GpuLight::default(); MAX_LIGHTS];
    for (o, l) in out.iter_mut().zip(&ls) {
        let kind = match l.kind {
            LightKind::Sun => 0.0,
            LightKind::Point => 1.0,
            LightKind::Spot => 2.0,
            LightKind::Area => 3.0,
        };
        let [px, py, pz] = l.position;
        let [dx, dy, dz] = l.direction;
        let [r, g, b] = l.radiance;
        *o = GpuLight { pos: [px, py, pz, kind], dir: [dx, dy, dz, l.cos_outer], rad: [r, g, b, l.cos_inner] };
    }
    (ls.len().min(MAX_LIGHTS), out)
}

const SHADER: &str = r#"
struct L { pos: vec4<f32>, dir: vec4<f32>, rad: vec4<f32> };
struct U { view_proj: mat4x4<f32>, view: mat4x4<f32>, params: vec4<f32>, light_params: vec4<f32>, shadow_vp: mat4x4<f32>, shadow_params: vec4<f32>, lights: array<L, 8> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var shadow_tex: texture_depth_2d;
@group(0) @binding(2) var shadow_smp: sampler_comparison;

// How much of the shadow light reaches a point: 1 = lit, 0 = in shadow (3x3 filtered edges).
fn shadow_vis(wp: vec3<f32>) -> f32 {
    let lp = u.shadow_vp * vec4<f32>(wp, 1.0);
    let ndc = lp.xyz / max(lp.w, 1e-5);
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let depth = ndc.z - u.shadow_params.y;
    var sum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let o = vec2<f32>(f32(x), f32(y)) * u.shadow_params.w * 1.5;
            sum = sum + textureSampleCompareLevel(shadow_tex, shadow_smp, uv + o, depth);
        }
    }
    let inside = uv.x > 0.0 && uv.x < 1.0 && uv.y > 0.0 && uv.y < 1.0 && ndc.z < 1.0 && lp.w > 0.0;
    return select(1.0, sum / 9.0, inside && u.shadow_params.x > 0.5);
}

@vertex fn vs_shadow(v: MeshIn) -> @builtin(position) vec4<f32> {
    return u.shadow_vp * vec4<f32>(v.pos, 1.0);
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
    let vis = shadow_vis(i.wp);
    let count = u32(u.light_params.x + 0.5);
    let v = normalize(eye_pos() - i.wp);
    var spec = vec3<f32>(0.0);
    if (count > 0u) {
        // scene lights: soft sky ambient plus each light's Lambert term (no shadows)
        var col = i.c * (u.light_params.y * (0.75 + 0.25 * n.y));
        for (var k = 0u; k < count; k = k + 1u) {
            let l = u.lights[k];
            let kind = u32(l.pos.w + 0.5);
            var ldir = -l.dir.xyz;
            var att = 1.0;
            if (kind != 0u) {
                let d = l.pos.xyz - i.wp;
                let dist2 = max(dot(d, d), 0.01);
                ldir = d * inverseSqrt(dist2);
                // brightness 1 at 2 m, falling off with the square of the distance
                att = 4.0 / dist2;
                if (kind >= 2u) {
                    att = att * smoothstep(l.dir.w, l.rad.w, dot(-ldir, l.dir.xyz));
                }
            }
            var ndl = diffuse(n, ldir, i.mat);
            if (kind == 3u && i.mat != 1u) {
                ndl = vec3<f32>(max((dot(n, ldir) + 0.35) / 1.35, 0.0));
            }
            if (f32(k) == u.shadow_params.z) {
                att = att * vis;
            }
            col = col + i.c * l.rad.rgb * ndl * att;
            col = col + l.rad.rgb * specular(n, ldir, v, i.mat) * att;
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
    let light = vec3<f32>(0.28 + 0.08 * max(n.y, 0.0)) + 0.62 * diffuse(n, key, i.mat) * key_vis + 0.18 * diffuse(n, fill, i.mat);
    spec = vec3<f32>(0.62 * key_vis * specular(n, key, v, i.mat) + 0.18 * specular(n, fill, v, i.mat));
    return vec4<f32>(i.c * light + spec, 1.0);
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
"#;

/// Auto depth range: from the camera (white) to the farthest geometry in front of it (black).
/// Vertex positions are a cheap stand-in for visible surfaces; a vertex behind the camera can
/// belong to a visible face, so the near end stays at the camera.
pub fn depth_range(geo: &geometry::Geometry, view: Mat4) -> (f32, f32) {
    let far =
        geo.triangles.iter().map(|v| -view.transform_point3(glam::Vec3::from_array(v.pos)).z).fold(0.0f32, f32::max);
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
    shadow_bind_group: wgpu::BindGroup,
    shadow_view: wgpu::TextureView,
    line_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    image_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// GPU textures of image assets: key -> (identity of the pixels, bind group).
    textures: std::collections::HashMap<String, (usize, wgpu::BindGroup)>,
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
        // the shadow pass binds only the uniforms (the shadow map is its target)
        let shadow_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shotstage shadow"),
            entries: &[uniform_entry],
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
        let shadow_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shotstage shadow"),
            layout: &shadow_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
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
                module: &module,
                entry_point: Some("vs_shadow"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Uint32],
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
        Self {
            mesh_pipeline,
            shadow_pipeline,
            shadow_bind_group,
            shadow_view,
            line_pipeline,
            image_pipeline,
            image_layout,
            sampler,
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
            if self.textures.get(*key).is_some_and(|(id, _)| *id == identity) {
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
            self.textures.insert(key.to_string(), (identity, bg));
        }
        self.textures.retain(|k, _| scene.assets.contains_key(k));
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
        let used: Vec<&str> = geo.images.iter().map(|q| q.asset.as_str()).collect();
        self.sync_textures(device, queue, scene, &used);
        let Some(target) = &self.target else { return };
        let (near, far) = if view.depth_far > view.depth_near {
            (view.depth_near, view.depth_far)
        } else {
            depth_range(&geo, view.view)
        };
        let u = Uniforms {
            view_proj: (view.proj * view.view).to_cols_array(),
            view: view.view.to_cols_array(),
            params: [pass as u32 as f32, near, far, 0.0],
            light_params: [0.0, 0.0, 0.0, 0.0],
            shadow_vp: Mat4::IDENTITY.to_cols_array(),
            shadow_params: [0.0; 4],
            lights: [GpuLight::default(); MAX_LIGHTS],
        };
        let mut u = u;
        let mut lights_used = vec![];
        if !overlay.studio_light {
            let (n, lights) = gpu_lights(scene, frame);
            u.light_params = [n as f32, 0.12, 0.0, 0.0];
            u.lights = lights;
            lights_used = lights[..n].to_vec();
        }
        let shadow = if pass == Pass::Clay && !overlay.no_shadows { shadow_setup(&geo, &lights_used) } else { None };
        if let Some((vp, index, bias)) = shadow {
            u.shadow_vp = vp.to_cols_array();
            u.shadow_params = [1.0, bias, index, 1.0 / SHADOW_SIZE as f32];
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
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shotstage") });
        if let (Some(_), Some(b)) = (shadow, &tri_buf) {
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
            sp.set_bind_group(0, &self.shadow_bind_group, &[]);
            sp.set_vertex_buffer(0, b.slice(..));
            sp.draw(0..geo.triangles.len() as u32, 0..1);
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
            if let Some(b) = &tri_buf {
                rp.set_pipeline(&self.mesh_pipeline);
                rp.set_vertex_buffer(0, b.slice(..));
                rp.draw(0..geo.triangles.len() as u32, 0..1);
            }
            if let Some(b) = &img_buf {
                rp.set_pipeline(&self.image_pipeline);
                rp.set_vertex_buffer(0, b.slice(..));
                for (i, q) in geo.images.iter().enumerate() {
                    if let Some((_, bg)) = self.textures.get(&q.asset) {
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
