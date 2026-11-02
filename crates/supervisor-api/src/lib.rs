//! The supervisor's contract (supervisor spec 13, 15; platform R34, R35; DESIGN 12 D5).
//!
//! - **Surface `supervisor-control`**: the API the supervisor serves on `:7070` inside the sandbox, for hermit only,
//!   with the control token ([`control`], [`client::ControlClient`], [`server::ControlServer`]).
//! - **The local socket** `/run/supervisor/agent.sock`: what processes in the sandbox ask the supervisor (the
//!   `SendUserFile` hook, secrets per key) ([`local`]).
//! - **Re-exports** of what the supervisor consumes from hermit (surface `hermit-sandbox`): the descriptor, the
//!   activity report and reply, secrets, release reasons and codes, and the exec body, defined once in `hermit-api`.

pub mod client;
pub mod control;
pub mod local;
pub mod server;

pub use hermit_api::exec::{ExecRequest, ExecResult};
pub use hermit_api::sandbox::{ReleaseCode, ReleaseReason};
pub use hermit_api::sandbox_surface::{
    ActivityEvent, ActivityEventKind, ActivityReply, ActivityReplyState, ActivityReport, ActivityState, MemoryUse,
    PendingRelease, SandboxSelf, SecretKeys, SecretValue,
};
pub use hermit_api::token::{control_token_matches, sha256_hex};
pub use hermit_api::{Surface, Timestamp};

/// `supervisor-control`: the `:7070` API. Fixed for a sandbox's life.
pub const CONTROL: Surface = ("supervisor-control", 1);
/// The surface the supervisor speaks to hermit (`hermit-sandbox`), re-exported for its `Aviary-Protocol` header.
pub const HERMIT_SANDBOX: Surface = hermit_api::SANDBOX;
/// The control port, the same in every implementation (hermit 3.10).
pub const CONTROL_PORT: u16 = 7070;

/// The `:7070` routes (supervisor 15.4).
pub mod routes {
    pub const HEALTH: &str = "/v1/health";
    pub const STATE: &str = "/v1/state";
    pub const PREPARE: &str = "/v1/lifecycle/prepare";
    pub const RESUMED: &str = "/v1/lifecycle/resumed";
    pub const RELEASE: &str = "/v1/lifecycle/release";
    pub const EXEC: &str = "/v1/exec";
}
