//! The route handlers, one module per route group (magpie 2): they read the request's identity, call the
//! operation and convert its error into the answer. No SQL here.

// a handler's error is its answer: the route tables (magpie 16) list them
#[allow(clippy::missing_errors_doc)]
pub mod changes;
#[allow(clippy::missing_errors_doc)]
pub mod conversations;
#[allow(clippy::missing_errors_doc)]
pub mod creations;
#[allow(clippy::missing_errors_doc)]
pub mod ops;

use aviary_core::http::ApiError;

use crate::error::Error;

/// A handler's result.
pub type Answer<T> = Result<T, ApiError>;

/// Converts magpie's error into the answer.
pub(crate) fn answer<T>(r: Result<T, Error>) -> Answer<T> {
    r.map_err(ApiError::from)
}
