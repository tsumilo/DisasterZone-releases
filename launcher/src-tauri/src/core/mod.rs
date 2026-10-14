//! Everything the launcher does without a window: verify a release, find WoW, install, update, roll back.
//! The Tauri commands in lib.rs are thin wrappers around this module; `cargo test` exercises it directly.

pub mod bridge;
pub mod error;
pub mod install;
pub mod manifest;
pub mod play;
pub mod source;
pub mod state;
pub mod wow;

pub use error::{Error, Result};
