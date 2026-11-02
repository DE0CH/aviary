//! magpie's one internal error type, and how it is answered (magpie 16: 503 `unavailable` when Postgres cannot be
//! reached, the contract's codes otherwise).

use aviary_core::http::ApiError;
use aviary_pg::reexports::tokio_postgres;
use magpie_api::errors::{ErrorCode, Refusal};
use tokio_postgres::error::SqlState;

use crate::limits;

/// What a magpie operation can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An answer of the contract (`not_found`, `conversation_migrating`, …).
    #[error("{0}")]
    Refused(Refusal),
    /// Postgres.
    #[error("{0}")]
    Db(#[from] aviary_pg::Error),
    /// Anything else that is magpie's own fault (a row that does not decode).
    #[error("internal: {0}")]
    Internal(String),
}

/// Shorthand.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Self {
        Error::Refused(r)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Internal(format!("json: {e}"))
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(e: tokio_postgres::Error) -> Self {
        Error::Db(aviary_pg::Error::Db(e))
    }
}

impl Error {
    /// A contract refusal with `code`.
    #[must_use]
    pub fn code(code: ErrorCode, message: impl Into<String>) -> Self {
        Error::Refused(Refusal::new(code, message))
    }

    /// 404 `not_found`.
    #[must_use]
    pub fn not_found() -> Self {
        Error::Refused(Refusal::not_found())
    }

    /// 400 `bad_request`.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::code(ErrorCode::BadRequest, message)
    }

    /// Whether this is a refusal with `code`.
    #[must_use]
    pub fn is(&self, code: &ErrorCode) -> bool {
        matches!(self, Error::Refused(r) if &r.code == code)
    }
}

/// Whether a Postgres error means the database could not be reached (rather than a statement being wrong): no
/// pooled connection, a connection that ended, or a server-side connection/shutdown/resource condition.
#[must_use]
pub fn is_unreachable(e: &aviary_pg::Error) -> bool {
    match e {
        aviary_pg::Error::Pool(_) | aviary_pg::Error::Closed => true,
        aviary_pg::Error::Db(db) => match db.code() {
            // no SQLSTATE: the connection itself failed (closed, reset, TLS, timeout)
            None => true,
            Some(code) => {
                let c = code.code();
                // 08 connection exception, 57P admin/crash shutdown and cannot-connect-now, 53 insufficient
                // resources, 25006 read-only transaction (a standby during a switchover)
                c.starts_with("08")
                    || c.starts_with("57P")
                    || c.starts_with("53")
                    || code == &SqlState::READ_ONLY_SQL_TRANSACTION
            }
        },
        _ => false,
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        match e {
            Error::Refused(r) => r.into(),
            Error::Db(db) if is_unreachable(&db) => {
                tracing::warn!(error = %db, "postgres unreachable");
                ApiError::unavailable("database unavailable", limits::UNAVAILABLE_RETRY_AFTER_S)
            }
            Error::Db(db) => {
                // the server's message names the constraint or column, never a value (the detail would)
                let (sqlstate, message) = match &db {
                    aviary_pg::Error::Db(e) => (
                        e.code().map(|c| c.code().to_owned()),
                        e.as_db_error().map(|d| d.message().to_owned()),
                    ),
                    _ => (None, None),
                };
                tracing::error!(error = %db, sqlstate = ?sqlstate, message = ?message, "postgres error");
                ApiError::internal("database error")
            }
            Error::Internal(m) => {
                tracing::error!(error = %m, "internal error");
                ApiError::internal("internal error")
            }
        }
    }
}
