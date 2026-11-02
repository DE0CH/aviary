//! Postgres for aviary services (platform 3.9, 4.3, 11): the connection pool, embedded migrations under an advisory
//! lock, the leader lock, the LISTEN helper, and transactions that cannot call out to the network.
//!
//! - [`connect`] builds a [`Pool`]: TLS required without certificate verification (`sslmode=require`), TCP
//!   keepalives 60/10/6, the service's `application_name`, and per-connection `statement_timeout` and
//!   `idle_in_transaction_session_timeout`.
//! - [`migrate`] applies [`Migration`]s (embedded with [`migrations!`]) in order, once, under a session advisory
//!   lock, recorded in `_migrations` with their sha256; a recorded migration whose sha256 differs is a start failure.
//! - [`Leader`] holds `pg_try_advisory_lock(key)` on a dedicated connection and says whether this process leads.
//! - [`Listener`] holds one dedicated LISTEN connection, reconnects with backoff, and wakes every [`Waiter`] after a
//!   reconnect so nothing missed in the gap is lost.
//! - [`tx!`] runs a closure in one transaction. **No transaction is held across a network call**: inside the closure
//!   a `reqwest` call is a clippy error (the `disallowed-methods` entries of `crates/aviary-pg/tx-rule/clippy.toml`,
//!   denied inside the closure by the macro; CI's lint job runs that pass over the workspace).

mod error;
mod leader;
mod listen;
mod lock;
mod migrate;
mod pool;
mod tx;

pub use error::Error;
pub use leader::Leader;
pub use listen::{Listener, Waiter};
pub use lock::LockKey;
pub use migrate::{MIGRATIONS_TABLE, Migration, migrate};
pub use pool::{Pool, connect};
pub use tokio_postgres::types::ToSql;
pub use tokio_postgres::{Row, Statement};
#[doc(hidden)]
pub use tx::{__run_tx, __tx_fn};
pub use tx::{Tx, TxFuture};

/// Re-exports for callers that need the underlying client types (row decoding, `FromSql` derives).
pub mod reexports {
    pub use deadpool_postgres;
    pub use tokio_postgres;
}
