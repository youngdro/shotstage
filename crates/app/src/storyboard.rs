//! The storyboard strip: every shot rendered through its camera at its middle frame into one
//! texture atlas, a couple per frame, again only when what a shot shows has changed.

use eframe::egui_wgpu::RenderState;
use shotstage_core::Scene;
use shotstage_core::eval::{camera_data_at, world_at};
use shotstage_render::{Pass, Renderer, View};

/// Cell size in pixels (16:9; other aspects fit inside).
pub const W: u32 = 256;
pub const H: u32 = 144;
const COLS: u32 = 8;

/// One shot's picture: what it was rendered from and where it sits in the atlas.
struct Cell {
    key: String,
    uv: egui::Rect,
    /// Width / height of the picture.
    aspect: f32,
}

pub struct Storyboard {
    renderer: Renderer,
    atlas: Option<(wgpu::Texture, egui::TextureId, u32)>,
    cells: Vec<Option<Cell>>,
}

/// Rendering settings that change every picture.
pub struct Look {
    pub revision: u64,
    pub pass: Pass,
    pub studio_light: bool,
    pub shadows: bool,
}

impl Storyboard {
    pub fn new(device: &wgpu::Device) -> Self {
        Self { renderer: Renderer::new(device), atlas: None, cells: Vec::new() }
    }

    /// Shot `i`'s picture once rendered (it may be from before the latest change): texture,
    /// UV rectangle and aspect.
    pub fn get(&self, i: usize) -> Option<(egui::TextureId, egui::Rect, f32)> {
        let (_, id, _) = self.atlas.as_ref()?;
        self.cells.get(i)?.as_ref().map(|c| (*id, c.uv, c.aspect))
    }

    /// Render up to `budget` out-of-date shots. Returns true while some are still out of date.
    pub fn update(&mut self, rs: &RenderState, scene: &Scene, look: &Look, budget: usize) -> bool {
        let shots = &scene.timeline.shots;
        let rows = (shots.len() as u32).div_ceil(COLS).max(1);
        if self.atlas.as_ref().is_none_or(|(_, _, r)| *r < rows) {
            let tex = rs.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shotstage storyboard"),
                size: wgpu::Extent3d { width: W * COLS, height: H * rows, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: shotstage_render::COLOR_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let mut egui_renderer = rs.renderer.write();
            if let Some((_, old, _)) = self.atlas.take() {
                egui_renderer.free_texture(&old);
            }
            let id = egui_renderer.register_native_texture(&rs.device, &view, wgpu::FilterMode::Linear);
            self.atlas = Some((tex, id, rows));
            self.cells.clear();
        }
        self.cells.resize_with(shots.len(), || None);
        let mut left = budget;
        let mut stale = false;
        for (i, shot) in shots.iter().enumerate() {
            let mid = (shot.start + shot.end) as f32 / 2.0;
            let key = format!(
                "{}|{}|{}|{mid}|{}|{}|{}",
                look.revision, shot.id, shot.camera, look.pass as u32, look.studio_light, look.shadows
            );
            if self.cells[i].as_ref().is_some_and(|c| c.key == key) {
                continue;
            }
            if left == 0 {
                stale = true;
                continue;
            }
            left -= 1;
            let (Ok(lens), Ok(world)) = (camera_data_at(scene, &shot.camera, mid), world_at(scene, &shot.camera, mid))
            else {
                self.cells[i] = None;
                continue;
            };
            // the camera's own aspect, as large as fits the cell
            let a = lens.aspect.clamp(0.2, 5.0);
            let (w, h) = if a >= W as f32 / H as f32 {
                (W, ((W as f32 / a).round() as u32).clamp(1, H))
            } else {
                (((H as f32 * a).round() as u32).clamp(1, W), H)
            };
            let view = View { view: world.inverse(), proj: lens.projection(a), depth: None };
            let mut overlay = shotstage_render::clean_overlay(&shot.camera);
            overlay.studio_light = look.studio_light;
            overlay.no_shadows = !look.shadows;
            self.renderer.render(&rs.device, &rs.queue, scene, mid, view, look.pass, &overlay, (w, h));
            let (Some(src), Some((atlas, _, rows))) = (self.renderer.color_texture(), self.atlas.as_ref()) else {
                continue;
            };
            let (cx, cy) = ((i as u32 % COLS) * W, (i as u32 / COLS) * H);
            let mut enc =
                rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("storyboard") });
            enc.copy_texture_to_texture(
                src.as_image_copy(),
                wgpu::TexelCopyTextureInfo {
                    texture: atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: cx, y: cy, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            rs.queue.submit([enc.finish()]);
            let (aw, ah) = ((W * COLS) as f32, (H * rows) as f32);
            let uv = egui::Rect::from_min_size(
                egui::pos2(cx as f32 / aw, cy as f32 / ah),
                egui::vec2(w as f32 / aw, h as f32 / ah),
            );
            self.cells[i] = Some(Cell { key, uv, aspect: w as f32 / h as f32 });
        }
        stale
    }
}
