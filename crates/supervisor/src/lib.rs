//! The in-sandbox supervisor and its keeper (supervisor spec; WP-21 builds the process model, the lifecycle and the
//! hermit side, WP-22 the `~/.claude` filesystem, WP-23 the Claude Code driver).
//!
//! Nothing in this crate knows a deployment: the same build runs everywhere, every limit is a constant in
//! [`limits`], and everything a deployment decides comes from hermit's descriptor.

pub mod boot;
pub mod docker;
pub mod fuse;
pub mod hermit;
pub mod keeper;
pub mod lifecycle;
pub mod limits;
pub mod local;
pub mod magpie;
pub mod memguard;
pub mod start;
pub mod state;
