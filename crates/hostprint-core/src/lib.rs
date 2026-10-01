//! Capture engine and configuration.

pub mod config;
mod engine;

pub use config::Config;
pub use engine::{capture, new_snapshot_id, normalize};
