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
    /// A dedicated connection ended while it was in use.
    #[error("postgres connection closed")]
    Closed,
}
