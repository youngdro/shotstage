//! shotstage core: the pure-data scene document.
//!
//! Nothing in this crate touches the GPU, the browser or the filesystem (it only decodes
//! panoramas from bytes), so the same scene
//! logic runs in the web editor (wasm), the CLI and the headless MCP server.

pub mod camera;
pub mod csg;
pub mod depthfit;
pub mod environment;
pub mod eval;
pub mod human;
pub mod image;
pub mod light;
pub mod mesh;
pub mod meshcache;
pub mod meshedit;
pub mod scene;
pub mod spatial;
pub mod timeline;
pub mod uv;
pub mod uvedit;

pub use camera::{CameraData, Intrinsics};
pub use environment::{Environment, ShadowSettings, Sky};
pub use eval::{EvaluatedEntity, EvaluatedScene, evaluate};
pub use image::{Asset, ImageData};
pub use light::{LightData, LightKind};
pub use mesh::{Mesh, MeshSource, Modifier, Primitive, Skin, TriMesh};
pub use scene::{CameraReference, Entity, EntityKind, Scene, Transform};
pub use timeline::{ExpressionMix, Interp, Key, KeyValue, Shot, Timeline, Track, TrackProperty};

/// Errors produced by scene operations. Messages are written for agents as well as people.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CoreError {
    #[error("no entity with id `{0}`")]
    NoEntity(String),
    #[error("an entity with id `{0}` already exists")]
    DuplicateId(String),
    #[error("entity `{0}` is not a {1}")]
    WrongKind(String, &'static str),
    #[error("invalid value: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
