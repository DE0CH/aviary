//! What [`crate::Tx`] and [`crate::Conn`] share: refusing transaction control, and recovering from a cached
//! statement that the schema made stale.
//!
//! Statements are prepared once per connection and cached (deadpool's statement cache). Migrations are expand-only
//! and a new replica applies them while old ones run (platform 4.3), so a cached statement whose result columns
//! come from `*` (`select *`, `returning *`) meets `cached plan must not change result type` (`0A000`) once its table
//! gains a column; a server that lost its prepared statements answers `prepared statement … does not exist`
//! (`26000`). Either clears every cached statement of the pool (each is prepared again on its next use) and retries
//! the statement once where that is safe: always outside a transaction, and inside one only when it is the
//! transaction's first statement (it restarts the transaction, which has read and written nothing). Elsewhere in a
//! transaction the statement's error is returned (the transaction is aborted); the next transaction works. Name the
//! columns rather than `*` and this never happens.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio_postgres::error::SqlState;

use crate::{Error, sql};

/// Whether `e` says the cached statement is stale (see the module docs).
pub(crate) fn is_stale(e: &tokio_postgres::Error) -> bool {
    let Some(db) = e.as_db_error() else {
        return false;
    };
    (*db.code() == SqlState::FEATURE_NOT_SUPPORTED && db.message().contains("cached plan must not change result type"))
        || *db.code() == SqlState::INVALID_SQL_STATEMENT_NAME
}

/// Clears the cached statements of every connection of `pool`.
pub(crate) fn clear_caches(pool: &deadpool_postgres::Pool) {
    pool.manager().statement_caches.clear();
}

/// [`Error::TransactionControl`] when one of `sql`'s statements controls the transaction (`savepoints`: whether
/// savepoints are allowed, as they are inside [`crate::tx!`]).
pub(crate) fn refuse_control(sql: &str, savepoints: bool) -> Result<(), Error> {
    for s in sql::statements(sql) {
        if sql::controls_transaction(&s, savepoints) {
            let mut head = sql::leading_words(&s, 3).join(" ");
            head.truncate(40);
            return Err(Error::TransactionControl(head));
        }
    }
    Ok(())
}

/// One statement in flight. Unless [`StmtGuard::ok`] is called, dropping it (an error, or a cancelled future)
/// marks the session as in doubt.
pub(crate) struct StmtGuard<'a> {
    doubt: &'a AtomicBool,
    pub(crate) first: bool,
    pub(crate) retried: bool,
    /// The tries left while the database is away ([`crate::retry`]).
    pub(crate) patience: crate::retry::Patience,
    done: bool,
}

impl<'a> StmtGuard<'a> {
    pub(crate) fn new(doubt: &'a AtomicBool, first: bool) -> Self {
        Self {
            doubt,
            first,
            retried: false,
            patience: crate::retry::Patience::default(),
            done: false,
        }
    }

    pub(crate) fn ok<T>(mut self, v: T) -> T {
        self.done = true;
        v
    }
}

impl Drop for StmtGuard<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.doubt.store(true, Ordering::SeqCst);
        }
    }
}

/// The body of one statement method: prepare (cached) and run on `$self.stmt_client()`, and on an error ask
/// `$self.recover` whether to try once more (a stale statement; for a [`crate::Conn`] also a lost connection, which it
/// replaces).
macro_rules! statement {
    ($self:ident, $guard:ident, $sql:ident, $params:ident, $method:ident) => {{
        loop {
            let outcome = {
                let client = $self.stmt_client().await?;
                match client.prepare_cached($sql).await {
                    Ok(st) => client.$method(&st, $params).await,
                    Err(e) => Err(e),
                }
            };
            match outcome {
                Ok(v) => return Ok($guard.ok(v)),
                Err(e) => $self.recover(&mut $guard, e).await?,
            }
        }
    }};
}
pub(crate) use statement;
