//! Transactions that cannot call out to the network (platform 4.3).
//!
//! [`tx!`](crate::tx!) is the only way to run a transaction through this crate. Its closure gets a [`Tx`], whose API
//! awaits nothing but the database; the macro denies clippy's `disallowed_methods` inside the closure, and the
//! `disallowed-methods` list of `crates/aviary-pg/tx-rule/clippy.toml` names `reqwest`'s calls, so CI's second
//! clippy pass (`CLIPPY_CONF_DIR=crates/aviary-pg/tx-rule`, `-A clippy::disallowed_methods` everywhere else) rejects a
//! `reqwest` call inside a transaction. The role's `idle_in_transaction_session_timeout = 60s` (platform 11.1) is
//! only a backstop.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use deadpool_postgres::Object;

use tokio_postgres::types::ToSql;
use tokio_postgres::{Row, Statement};

use crate::{Error, Pool};

/// The future a [`tx!`](crate::tx!) closure returns.
pub type TxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

/// One open transaction on a pooled connection it owns. Statements are prepared once per connection and cached.
///
/// `'env` is the caller's scope: the closure's future may borrow anything that outlives it. A `Tx` dropped before
/// its commit or rollback (the future was cancelled, or panicked) takes its connection out of the pool and closes it,
/// so the server rolls the transaction back and no pooled connection is ever left inside one.
pub struct Tx<'env> {
    client: Option<Object>,
    _env: PhantomData<&'env ()>,
}

impl std::fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tx").finish_non_exhaustive()
    }
}

impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if let Some(obj) = self.client.take() {
            // still inside the transaction: never hand this connection back
            drop(Object::take(obj));
        }
    }
}

impl Tx<'_> {
    fn client(&self) -> Result<&Object, Error> {
        self.client.as_ref().ok_or(Error::Closed)
    }

    async fn prepare(&self, sql: &str) -> Result<Statement, Error> {
        Ok(self.client()?.prepare_cached(sql).await?)
    }

    /// Runs one statement; the number of rows it affected.
    ///
    /// # Errors
    /// When the statement fails.
    pub async fn execute(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64, Error> {
        let st = self.prepare(sql).await?;
        Ok(self.client()?.execute(&st, params).await?)
    }

    /// Runs one query; every row.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error> {
        let st = self.prepare(sql).await?;
        Ok(self.client()?.query(&st, params).await?)
    }

    /// Runs one query that returns exactly one row.
    ///
    /// # Errors
    /// When the query fails or does not return exactly one row.
    pub async fn query_one(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> {
        let st = self.prepare(sql).await?;
        Ok(self.client()?.query_one(&st, params).await?)
    }

    /// Runs one query that returns at most one row.
    ///
    /// # Errors
    /// When the query fails or returns more than one row.
    pub async fn query_opt(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>, Error> {
        let st = self.prepare(sql).await?;
        Ok(self.client()?.query_opt(&st, params).await?)
    }

    /// Runs statements without parameters (the simple query protocol).
    ///
    /// # Errors
    /// When a statement fails.
    pub async fn batch_execute(&self, sql: &str) -> Result<(), Error> {
        Ok(self.client()?.batch_execute(sql).await?)
    }

    /// `pg_notify(channel, payload)`, delivered to listeners when the transaction commits.
    ///
    /// # Errors
    /// When the statement fails.
    pub async fn notify(&self, channel: &str, payload: &str) -> Result<(), Error> {
        self.execute("select pg_notify($1, $2)", &[&channel, &payload]).await?;
        Ok(())
    }

    /// Ends the transaction (`commit` or `rollback`) and hands the connection back to the pool.
    async fn finish(mut self, sql: &str) -> Result<(), Error> {
        let res = self.client()?.batch_execute(sql).await;
        if res.is_ok() {
            // back to the pool only once the transaction is over; on failure Drop closes it
            drop(self.client.take());
        }
        Ok(res?)
    }
}

/// Makes the closure's type higher-ranked over the borrow of the transaction (closures passed through a `let` are
/// not otherwise inferred that way). Used by [`tx!`](crate::tx!) only.
#[doc(hidden)]
pub fn __tx_fn<'env, T, E, F>(f: F) -> F
where
    F: for<'a> FnOnce(&'a Tx<'env>) -> TxFuture<'a, T, E>,
{
    f
}

/// Runs `f` in one transaction on a pooled connection: commits when it returns `Ok`, rolls back when it returns
/// `Err`; a panic or a dropped future closes the connection (the server rolls back). Used by [`tx!`](crate::tx!)
/// only.
///
/// # Errors
/// `f`'s error, or [`Error`] (converted) when no connection could be had, `BEGIN` failed or `COMMIT` failed.
#[doc(hidden)]
pub async fn __run_tx<'env, T, E, F>(pool: &Pool, f: F) -> Result<T, E>
where
    F: for<'a> FnOnce(&'a Tx<'env>) -> TxFuture<'a, T, E>,
    E: From<Error>,
{
    let client = pool.get().await?;
    client.batch_execute("begin").await.map_err(Error::from)?;
    let tx = Tx {
        client: Some(client),
        _env: PhantomData,
    };
    match f(&tx).await {
        Ok(v) => {
            tx.finish("commit").await?;
            Ok(v)
        }
        Err(e) => {
            if let Err(rb) = tx.finish("rollback").await {
                tracing::warn!(error = %rb, "rollback failed (the connection is discarded)");
            }
            Err(e)
        }
    }
}

/// Runs a closure in one transaction and evaluates to a future of its result: commit on `Ok`, rollback on `Err`.
///
/// ```ignore
/// let n: i64 = aviary_pg::tx!(&pool, |t| {
///     t.execute("insert into things (id) values ($1)", &[&id]).await?;
///     let row = t.query_one("select count(*) from things", &[]).await?;
///     Ok(row.get(0))
/// })
/// .await?;
///
/// // with the caller's own error type (it must implement `From<aviary_pg::Error>`)
/// let v = aviary_pg::tx!(&pool, |t| -> Result<u64, MyError> { Ok(t.execute("…", &[]).await?) }).await?;
/// ```
///
/// Inside the closure a `reqwest` call is a clippy error (platform 4.3): finish the transaction, then call out.
#[macro_export]
macro_rules! tx {
    ($pool:expr, |$t:ident| -> $ret:ty $body:block) => {{
        #[deny(clippy::disallowed_methods)]
        let __aviary_pg_tx = $crate::__tx_fn(|$t| {
            ::std::boxed::Box::pin(async move {
                let __aviary_pg_out: $ret = $body;
                __aviary_pg_out
            })
        });
        $crate::__run_tx($pool, __aviary_pg_tx)
    }};
    ($pool:expr, |$t:ident| $body:expr) => {
        $crate::tx!($pool, |$t| -> ::std::result::Result<_, $crate::Error> { $body })
    };
}
