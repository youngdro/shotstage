//! wgpu renderer. Draws into its own offscreen colour + depth target, so the same code feeds
//! the editor viewport (shown as an egui texture) and frame exports.
//!
//! Only features available on WebGL2 are used, so the web build falls back cleanly when
//! WebGPU is missing.

pub mod geometry;

use glam::Mat4;
use shotstage_core::Scene;
use wgpu::util::DeviceExt as _;

use geometry::{LineVertex, Vertex};
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
}

const SHADER: &str = r#"
struct U { view_proj: mat4x4<f32>, view: mat4x4<f32>, params: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;

struct MeshIn { @location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec3<f32>, @location(3) id: u32 };
struct MeshOut { @builtin(position) clip: vec4<f32>, @location(0) n: vec3<f32>, @location(1) c: vec3<f32>, @location(2) vz: f32 };

@vertex fn vs_mesh(v: MeshIn) -> MeshOut {
    var o: MeshOut;
    o.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    o.n = v.normal;
    o.c = v.color;
    o.vz = -(u.view * vec4<f32>(v.pos, 1.0)).z;
    return o;
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
    let key = normalize(vec3<f32>(0.45, 0.8, 0.4));
    let fill = normalize(vec3<f32>(-0.6, 0.25, -0.5));
    let light = 0.28 + 0.62 * max(dot(n, key), 0.0) + 0.18 * max(dot(n, fill), 0.0) + 0.08 * max(n.y, 0.0);
    return vec4<f32>(i.c * light, 1.0);
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
    line_pipeline: wgpu::RenderPipeline,
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
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shotstage"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shotstage"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
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
        Self { mesh_pipeline, line_pipeline, uniform, bind_group, target: None, target_generation: 0 }
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
        let Some(target) = &self.target else { return };
        let geo = geometry::build(scene, frame, overlay);
        let (near, far) = if view.depth_far > view.depth_near {
            (view.depth_near, view.depth_far)
        } else {
            depth_range(&geo, view.view)
        };
        let u = Uniforms {
            view_proj: (view.proj * view.view).to_cols_array(),
            view: view.view.to_cols_array(),
            params: [pass as u32 as f32, near, far, 0.0],
        };
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
        let clear = match pass {
            Pass::Clay => wgpu::Color { r: 0.16, g: 0.165, b: 0.18, a: 1.0 },
            Pass::Depth => wgpu::Color::BLACK,
            Pass::Normal => wgpu::Color { r: 0.5, g: 0.5, b: 1.0, a: 1.0 },
        };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shotstage") });
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
            if let Some(b) = &line_buf {
                rp.set_pipeline(&self.line_pipeline);
                rp.set_vertex_buffer(0, b.slice(..));
                rp.draw(0..geo.lines.len() as u32, 0..1);
            }
        }
        queue.submit([enc.finish()]);
    }
}
