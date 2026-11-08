//! `render.snapshot`: the parameters and the camera maths are here (no GPU), so every host
//! agrees on what a snapshot shows. The pixels come from a GPU host: the editor (web and
//! desktop) and the CLI / MCP server render them and return a PNG.

use glam::Mat4;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shotstage_core::Scene;
use shotstage_core::depthfit::DepthMap;
use shotstage_core::eval::{camera_data_at, world_at};

use crate::builtin::{frame_ok, pick_camera};
use crate::{CmdResult, CommandError, parse};

pub const SNAPSHOT_COMMAND: &str = "render.snapshot";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotPass {
    /// Clay shading: the readable reference frame.
    #[default]
    Clay,
    /// Linear depth, near = white (ControlNet / video-model depth convention).
    Depth,
    /// View-space normals as RGB.
    Normal,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SnapshotParams {
    /// Camera to look through. Default: the camera in use at `frame`.
    pub camera: Option<String>,
    /// Default: the editor's current frame in the editor, else the timeline start.
    pub frame: Option<f32>,
    pub pass: Option<SnapshotPass>,
    /// Pixels, 16..=4096. Default 1024 wide; the other side follows the camera's aspect.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Depth pass: `[near, far]` metres mapped linearly to white..black, or
    /// `[near, focusNear, focusFar, far]` to give the focus range most of the greys. Default:
    /// fitted to what is in frame, with the subjects (not the floor, walls or a table top
    /// running out of frame) given most of the greys, so small things stay apart.
    pub depth_range: Option<Vec<f32>>,
}

/// Everything a GPU host needs to draw a snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotPlan {
    pub camera: String,
    pub frame: f32,
    pub width: u32,
    pub height: u32,
    pub pass: SnapshotPass,
    pub view: Mat4,
    pub proj: Mat4,
    /// Depth pass: distance to grey; `None` fits each frame.
    pub depth_range: Option<DepthMap>,
}

fn side(v: u32, what: &str) -> CmdResult<u32> {
    if (16..=4096).contains(&v) { Ok(v) } else { Err(CommandError::invalid(format!("{what} must be 16..=4096"))) }
}

/// Resolve snapshot params against a scene. `default_frame` is the host's playhead, if any.
pub fn plan(scene: &Scene, params: Value, default_frame: Option<f32>) -> CmdResult<SnapshotPlan> {
    let p: SnapshotParams = parse(params)?;
    let frame = frame_ok(p.frame.or(default_frame).unwrap_or(scene.timeline.start as f32))?;
    let camera = pick_camera(scene, p.camera, frame)?;
    let lens = camera_data_at(scene, &camera, frame)?;
    let (width, height) = match (p.width, p.height) {
        (Some(w), Some(h)) => (side(w, "width")?, side(h, "height")?),
        (Some(w), None) => (side(w, "width")?, ((w as f32 / lens.aspect).round() as u32).clamp(16, 4096)),
        (None, Some(h)) => (((h as f32 * lens.aspect).round() as u32).clamp(16, 4096), side(h, "height")?),
        (None, None) => (1024, ((1024.0 / lens.aspect).round() as u32).clamp(16, 4096)),
    };
    let world = world_at(scene, &camera, frame)?;
    Ok(SnapshotPlan {
        camera,
        frame,
        width,
        height,
        pass: p.pass.unwrap_or_default(),
        view: world.inverse(),
        proj: lens.projection(width as f32 / height as f32),
        depth_range: match p.depth_range {
            Some(v) => Some(DepthMap::from_slice(&v).ok_or_else(|| {
                CommandError::invalid(
                    "depthRange must be [near, far] or [near, focusNear, focusFar, far], increasing, from 0",
                )
            })?),
            None => None,
        },
    })
}

/// The JSON a host returns for a rendered snapshot.
pub fn result_json(plan: &SnapshotPlan, png_base64: String) -> Value {
    serde_json::json!({
        "mimeType": "image/png",
        "data": png_base64,
        "width": plan.width,
        "height": plan.height,
        "camera": plan.camera,
        "frame": plan.frame,
        "pass": plan.pass,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use serde_json::json;

    #[test]
    fn plan_follows_the_camera() {
        let mut s = Session::default();
        assert!(plan(s.scene(), json!({}), None).is_err(), "no camera");
        s.dispatch("entity.create", json!({"type": "camera", "id": "cam", "position": [0, 1, 5]})).unwrap();
        let p = plan(s.scene(), json!({"pass": "depth"}), Some(12.0)).unwrap();
        assert_eq!((p.camera.as_str(), p.width, p.height, p.frame), ("cam", 1024, 576, 12.0));
        assert_eq!(p.pass, SnapshotPass::Depth);
        assert!(plan(s.scene(), json!({"width": 5}), None).is_err());
        assert!(plan(s.scene(), json!({"bogus": 1}), None).is_err());
        let e = s.dispatch(SNAPSHOT_COMMAND, json!({})).unwrap_err();
        assert_eq!(e.code, "unsupported");
    }
}
