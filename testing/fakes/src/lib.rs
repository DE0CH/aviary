//! In-process fakes of magpie, hermit and the supervisor for the other packages' unit and integration tests
//! (platform 4.1, WP-08). Each implements its contract crate's `Server` trait, so it is served by that crate's own
//! router with the contract's paths, extractors and status codes, and adds:
//!
//! - **scripted answers** on every route ([`script`]), straight from the contract's golden fixtures
//!   ([`fixtures`]) or written by the test, plus a call log;
//! - **a model** where the other packages' tests need behaviour rather than canned answers (magpie's sandbox
//!   surface, hermit's sandboxes, people, feed, tokens and workspaces, the supervisor's control API); a scripted
//!   answer always wins over the model;
//! - **fault injection** ([`fault`]): latency, bursts of 503, holding requests until released, and dropping the
//!   connection without an answer, per route or everywhere.
//!
//! [`serve::serve`] runs a fake on a local port with an accept loop that can drop a connection mid-request.

pub mod answer;
pub mod fault;
pub mod fixtures;
pub mod hermit;
pub mod magpie;
pub mod script;
pub mod serve;
pub mod supervisor;

pub use fault::{FaultId, FaultKind, Faults, Hold};
pub use hermit::FakeHermit;
pub use magpie::FakeMagpie;
pub use script::{Call, Reply, ReplyBody, Script};
pub use serve::{Served, serve};
pub use supervisor::FakeSupervisor;
