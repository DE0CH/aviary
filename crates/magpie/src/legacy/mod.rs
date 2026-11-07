//! The legacy-path set (DESIGN 9.5, 9.7), until A7: old asset paths in a send's `files[]` keep resolving after the
//! switch window's contraction (A6, cutover 10.5 step 7), which dropped the window's tables and routes.
//!
//! - [`paths`]: an old asset path in a send's `files[]` resolves through `uploads.legacy_path`, then
//!   `import_uploads`, and each send attaches a fresh copy of the upload.
//! - [`metrics`]: `magpie_legacy_path_total` (A7 waits for 30 days without one) and
//!   `magpie_unknown_upload_total{legacy}`.

pub mod metrics;
pub mod paths;

pub use metrics::Metrics;
