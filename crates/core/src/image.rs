//! Images placed in the scene: reference cards, billboards that always face the camera,
//! backdrops and floor decals. The pixels live once in the scene's asset table (shared, so undo
//! snapshots do not copy them); an image entity is a textured card that refers to one asset.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;
use crate::{CoreError, Result};

/// An encoded image (PNG, JPEG or WebP) stored in the scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    /// `image/png`, `image/jpeg` or `image/webp`.
    pub mime: String,
    /// Pixel size.
    pub width: u32,
    pub height: u32,
    /// Base64 of the file. Shared between scene copies, so history stays cheap.
    #[schemars(with = "String")]
    pub data: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageData {
    /// Key of the asset in the scene's `assets`.
    pub asset: String,
    /// Card size in metres `[width, height]` (keeps the picture's aspect when imported).
    pub size: [f32; 2],
    /// Always turn to face the viewer about the vertical axis (cut-out people, trees, signs).
    #[serde(default)]
    pub billboard: bool,
}

impl ImageData {
    pub fn validate(&self) -> Result<()> {
        if !self.size.iter().all(|s| s.is_finite() && *s > 0.0 && *s <= 10_000.0) {
            return Err(CoreError::Invalid("image size must be in (0, 10000] metres".into()));
        }
        Ok(())
    }

    /// The card: a quad in the local XY plane facing +Z (both sides drawn), centred.
    pub fn card(&self) -> Mesh {
        let (w, h) = (self.size[0] / 2.0, self.size[1] / 2.0);
        Mesh {
            positions: vec![[-w, -h, 0.0], [w, -h, 0.0], [w, h, 0.0], [-w, h, 0.0]],
            faces: vec![vec![0, 1, 2, 3], vec![3, 2, 1, 0]],
        }
    }
}
