//! The switch window (DESIGN 9.2-9.5; cutover 6.4, 7.8; switch-final 4.3, 10.1, 23 C5 and C15): magpie's part of the
//! hand-over of conversations the old stack still owns. Deleted in A6, except the legacy-path set ([`paths`]), which
//! goes in A7 (DESIGN 9.7).
//!
//! - [`tickets`]: pigeon takes a ticket before every write it sends through its legacy upstream; a ticket closes only
//!   on the old stack's outcome, and a fenced conversation's ticket call waits up to 3 s for the release.
//! - [`fence`]: the importer's fence, release and unfence transactions (cutover 7.8), the fence's 3 s lease, and the
//!   fenced wait of every native mutation (cutover 6.4's table: wait for the release, else 409 `route: "legacy"`).
//! - [`mirror`]: the importer's running mirror, a legacy conversation's old running state in `run_state`.
//! - [`paths`]: an old asset path in a send's `files[]` resolves through `uploads.legacy_path`, then
//!   `import_uploads`, then a synchronous ingest by the import Deployment ([`ingest`]), and each send attaches a fresh
//!   copy of the upload.
//! - [`metrics`]: the window's counters of DESIGN 9.9.

pub mod fence;
pub mod ingest;
pub mod metrics;
pub mod mirror;
pub mod paths;
pub mod tickets;

pub use metrics::Metrics;

/// The A6 contraction's schema change for the window's tables (cutover 10.5 step 7), kept here so a test proves the
/// legacy-path set outlives it. It becomes a migration in the A6 contraction release, with the code that reads the
/// dropped columns removed in the same release.
pub const A6_CONTRACTION: &str = include_str!("a6_contraction.sql");
