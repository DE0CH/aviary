//! The crate's one error type.

/// What can go wrong talking to Postgres. Messages never contain the database URL or its password.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A statement or the connection failed.
    #[error("postgres: {0}")]
    Db(#[from] tokio_postgres::Error),
    /// No pooled connection could be had (the pool's wait or connect timeout, or the pool is closed).
    #[error("postgres pool: {0}")]
    Pool(#[from] deadpool_postgres::PoolError),
    /// The database URL or the pool settings are unusable.
    #[error("postgres config: {0}")]
    Config(String),
    /// The TLS client could not be built.
    #[error("postgres tls: {0}")]
    Tls(#[from] rustls::Error),
    /// A migration recorded as applied differs from the one embedded in this build: someone edited history.
    #[error(
        "migration {version} ({name}) was edited after it was applied: recorded sha256 {recorded}, embedded {embedded}"
    )]
    MigrationEdited {
        /// The migration's version.
        version: i32,
        /// The embedded migration's name.
        name: String,
        /// The sha256 recorded in `_migrations`.
        recorded: String,
        /// The sha256 of the embedded SQL.
        embedded: String,
    },
    /// The embedded migration list is malformed (a bad file name, versions out of order, a no-transaction migration
    /// with more than one statement).
    #[error("bad migration list: {0}")]
    BadMigration(String),
    /// A [`crate::tx!`] closure returned `Ok`, but a statement inside it had failed (its error ignored), which
    /// aborted the transaction: nothing was committed.
    #[error("transaction rolled back: a statement inside it failed and its error was ignored")]
    RolledBack,
    /// A statement that controls the transaction (`begin`, `commit`, …; outside [`crate::tx!`] also savepoints) was
    /// refused: a transaction is only ever [`crate::tx!`] (platform 4.3).
    #[error("transaction control is refused here (use aviary_pg::tx!): {0}")]
    TransactionControl(String),
    /// A no-transaction migration left the index it builds invalid (a failed or interrupted
    /// `create index concurrently`); it was dropped (unless another session may be building it), and the start
    /// fails.
    #[error("migration {version} ({name}) left invalid indexes: {indexes:?}")]
    InvalidIndex {
        /// The migration's version.
        version: i32,
        /// The migration's name.
        name: String,
        /// The invalid index found.
        indexes: Vec<String>,
    },
    /// A dedicated connection ended while it was in use.
    #[error("postgres connection closed")]
    Closed,
}

impl Error {
    /// The SQLSTATE of a statement error ([`Error::Db`] from the server), e.g. `SqlState::UNIQUE_VIOLATION`.
    #[must_use]
    pub fn code(&self) -> Option<&tokio_postgres::error::SqlState> {
        match self {
            Self::Db(e) => e.code(),
            _ => None,
        }
    }
}

impl Error {
    /// The database is away for a moment (a planned switchover, a restart, a primary just demoted to a standby): the
    /// same work can run again shortly on a new connection ([`crate::retry`]).
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Db(e) | Self::Pool(deadpool_postgres::PoolError::Backend(e)) => transient_db(e),
            Self::Pool(deadpool_postgres::PoolError::Timeout(_)) | Self::Closed => true,
            _ => false,
        }
    }
}

/// A lost or refused connection: the server's (SQLSTATE class 08; 57P01 admin shutdown, 57P02 crash shutdown, 57P03
/// cannot connect now; 25006, a write that reached a primary just demoted) or the client's own (tokio-postgres's
/// connection kinds carry no SQLSTATE: their messages are fixed strings).
pub(crate) fn transient_db(e: &tokio_postgres::Error) -> bool {
    if let Some(code) = e.code() {
        let c = code.code();
        return c.starts_with("08") || matches!(c, "57P01" | "57P02" | "57P03" | "25006");
    }
    if e.is_closed() {
        return true;
    }
    let s = e.to_string();
    [
        "error communicating with the server",
        "error connecting to server",
        "timeout waiting for server",
        "error performing TLS handshake",
        "unexpected message from server",
    ]
    .iter()
    .any(|k| s.starts_with(k))
}
