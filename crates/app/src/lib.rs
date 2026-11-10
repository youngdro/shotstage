//! shotstage editor (egui + wgpu). Runs natively and in the browser.
//!
//! In the browser the crate exports `mount(container, options)`, which builds the editor
//! inside any DOM element and returns an instance whose methods reach the same command
//! registry as the CLI and MCP server (see `web.rs`).

mod curves;
mod files;
pub mod hub;
pub mod i18n;
mod mp4;
mod storyboard;
mod thumbs;
mod ui;
#[cfg(target_arch = "wasm32")]
mod web;

pub use hub::{Hub, SharedHub, dispatch};
pub use ui::EditorApp;
