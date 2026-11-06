//! The route handlers, one module per route group (magpie 2): they read the request's identity, call the
//! operation and convert its error into the answer. No SQL here.

// a handler's error is its answer: the route tables (magpie 16) list them
#[allow(clippy::missing_errors_doc)]
pub mod admin;
#[allow(clippy::missing_errors_doc)]
pub mod blobs;
#[allow(clippy::missing_errors_doc)]
pub mod changes;
#[allow(clippy::missing_errors_doc)]
pub mod conversations;
#[allow(clippy::missing_errors_doc)]
pub mod creations;
#[allow(clippy::missing_errors_doc)]
pub mod files;
#[allow(clippy::missing_errors_doc)]
pub mod hermit;
#[allow(clippy::missing_errors_doc)]
pub mod legacy;
#[allow(clippy::missing_errors_doc)]
pub mod ops;
#[allow(clippy::missing_errors_doc)]
pub mod sandbox_cc;
#[allow(clippy::missing_errors_doc)]
pub mod sandbox_files;
#[allow(clippy::missing_errors_doc)]
pub mod sandbox_live;
#[allow(clippy::missing_errors_doc)]
pub mod sandbox_turns;
#[allow(clippy::missing_errors_doc)]
pub mod schedules;
#[allow(clippy::missing_errors_doc)]
pub mod shares;
#[allow(clippy::missing_errors_doc)]
pub mod timeline;
#[allow(clippy::missing_errors_doc)]
pub mod turns;
#[allow(clippy::missing_errors_doc)]
pub mod uploads;
#[allow(clippy::missing_errors_doc)]
pub mod viewas;

use aviary_core::http::ApiError;

use crate::error::Error;

/// A handler's result.
pub type Answer<T> = Result<T, ApiError>;

/// Converts magpie's error into the answer.
pub(crate) fn answer<T>(r: Result<T, Error>) -> Answer<T> {
    r.map_err(ApiError::from)
}
