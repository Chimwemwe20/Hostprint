//! macOS collectors for system, resources, processes, network and services
//! (launchd). Docker, Git, runtimes, environment, files and Docker/file logs
//! use the same code on every Unix.
//!
//! The parsers in `parse` are plain functions over tool output, tested on
//! every platform; `live` runs the tools and is compiled only on macOS.

#[cfg(target_os = "macos")]
pub(crate) mod live;
pub(crate) mod parse;
