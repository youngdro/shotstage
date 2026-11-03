//! Palette thumbnails: every prefab and shape rendered once, a few per frame, into one texture
//! atlas that egui draws from (no readback, so it works the same on the web).

use std::collections::HashMap;

use eframe::egui_wgpu::RenderState;
use serde_json::json;
use shotstage_commands::Session;
use shotstage_core::Scene;
use shotstage_core::eval::{camera_data_at, world_at};
use shotstage_render::{Pass, Renderer, View};

/// Thumbnail size in pixels (4:3).
pub const W: u32 = 128;
pub const H: u32 = 96;
const COLS: u32 = 8;

pub struct Thumbs {
    renderer: Renderer,
    atlas: Option<(wgpu::Texture, egui::TextureId)>,
    /// key -> slot in the atlas, once rendered
    slots: HashMap<String, u32>,
    /// still to render: (key, what to create)
    todo: Vec<(String, serde_json::Value)>,
    next_slot: u32,
}

impl Thumbs {
    /// `items`: (key, a command `{command, params}` that creates the thing with id `thumb`).
    pub fn new(device: &wgpu::Device, items: Vec<(String, serde_json::Value)>) -> Self {
        let mut todo = items;
        todo.reverse();
        Self { renderer: Renderer::new(device), atlas: None, slots: HashMap::new(), todo, next_slot: 0 }
    }

    /// The atlas texture and the UV rectangle of `key`, once it is ready.
    pub fn get(&self, key: &str) -> Option<(egui::TextureId, egui::Rect)> {
        let (_, id) = self.atlas.as_ref()?;
        let slot = *self.slots.get(key)?;
        let rows = self.rows();
        let (c, r) = (slot % COLS, slot / COLS);
        let (u, v) = (1.0 / COLS as f32, 1.0 / rows as f32);
        Some((*id, egui::Rect::from_min_size(egui::pos2(c as f32 * u, r as f32 * v), egui::vec2(u, v))))
    }

    fn rows(&self) -> u32 {
        (self.slots.len() as u32 + self.todo.len() as u32).div_ceil(COLS).max(1)
    }

    /// Render up to `n` thumbnails. Returns true while there is more to do.
    pub fn step(&mut self, rs: &RenderState, n: usize) -> bool {
        if self.todo.is_empty() {
            return false;
        }
        if self.atlas.is_none() {
            let rows = self.rows();
            let tex = rs.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shotstage thumbnails"),
                size: wgpu::Extent3d { width: W * COLS, height: H * rows, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: shotstage_render::COLOR_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let id = rs.renderer.write().register_native_texture(&rs.device, &view, wgpu::FilterMode::Linear);
            self.atlas = Some((tex, id));
        }
        for _ in 0..n {
            let Some((key, create)) = self.todo.pop() else { break };
            let Some(scene) = stage(&create) else { continue };
            let Some(view) = camera_view(&scene) else { continue };
            let mut overlay = shotstage_render::clean_overlay("thumb_cam");
            overlay.studio_light = true;
            self.renderer.render(&rs.device, &rs.queue, &scene, 0.0, view, Pass::Clay, &overlay, (W, H));
            let (Some(src), Some((atlas, _))) = (self.renderer.color_texture(), self.atlas.as_ref()) else { continue };
            let slot = self.next_slot;
            self.next_slot += 1;
            let mut enc = rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("thumb") });
            enc.copy_texture_to_texture(
                src.as_image_copy(),
                wgpu::TexelCopyTextureInfo {
                    texture: atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: (slot % COLS) * W, y: (slot / COLS) * H, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
            );
            rs.queue.submit([enc.finish()]);
            self.slots.insert(key, slot);
        }
        !self.todo.is_empty()
    }
}

/// A tiny scene with the item (id `thumb`) and a camera framing it.
fn stage(create: &serde_json::Value) -> Option<Scene> {
    let mut s = Session::default();
    s.dispatch_json(create.clone()).ok()?;
    s.dispatch(
        "entity.create",
        json!({"type": "camera", "id": "thumb_cam", "camera": {"aspect": W as f32 / H as f32, "focalLengthMm": 50}}),
    )
    .ok()?;
    s.dispatch(
        "camera.frame",
        json!({"camera": "thumb_cam", "subject": "thumb", "size": "full", "angle": "high", "side": "frontRight"}),
    )
    .ok()?;
    Some(s.scene().clone())
}

fn camera_view(scene: &Scene) -> Option<View> {
    let lens = camera_data_at(scene, "thumb_cam", 0.0).ok()?;
    let world = world_at(scene, "thumb_cam", 0.0).ok()?;
    Some(View { view: world.inverse(), proj: lens.projection(lens.aspect), depth_near: 0.0, depth_far: 0.0 })
}
