//! The connection pool and dedicated connections (platform 3.9, 4.3).

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use deadpool_postgres::{Manager, ManagerConfig, Object, RecyclingMethod, Runtime};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::task::JoinHandle;
use tokio_postgres::config::SslMode;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Row, Socket};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::Error;
use crate::stmt::{self, StmtGuard, statement};

/// TCP keepalives, matching the server's (platform 4.3): idle 60 s, interval 10 s, 6 probes, so a dead peer's
/// session (and its advisory locks) is gone within about two minutes.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_RETRIES: u32 = 6;
/// Connect timeout and the pool's wait/create/recycle timeouts (hermit 10.1).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const POOL_TIMEOUT: Duration = Duration::from_secs(5);
/// The role-level backstop (platform 11.1), also set per connection.
const IDLE_IN_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(60);

/// Run on every pooled connection before it is handed out again (deadpool's recycle). First a check that it is
/// outside any transaction: a multi-statement simple query runs in one implicit transaction that starts with the
/// message, so `transaction_timestamp()` equals `statement_timestamp()` unless an explicit transaction was already
/// open; then the check raises and deadpool discards the connection instead of handing out one inside a transaction
/// (a backstop: [`crate::tx!`] itself discards a connection it did not finish). Then deadpool's `Clean` reset, which
/// keeps the prepared statements (no `DISCARD ALL`): session state set through a [`Conn`] or a [`crate::Tx`] (`set`,
/// `set role`, `set_config(…, false)`, temp tables, `listen`, session advisory locks, open cursors) never reaches
/// the connection's next user. `RESET ALL` goes back to the connection's startup values, so the per-pool
/// `statement_timeout` and `application_name` of [`connect`] stay.
pub(crate) const RECYCLE_SQL: &str = "\
    do $aviary_recycle$ begin \
      if transaction_timestamp() <> statement_timestamp() then \
        raise exception 'aviary-pg: pooled connection returned inside a transaction'; \
      end if; \
    end $aviary_recycle$; \
    close all; \
    set session authorization default; \
    reset all; \
    unlisten *; \
    select pg_advisory_unlock_all(); \
    discard temp; \
    discard sequences";

pub(crate) type Connection = tokio_postgres::Connection<Socket, <MakeRustlsConnect as MakeTlsConnect<Socket>>::Stream>;

/// A service's Postgres: the pool of request connections, and the settings to open the dedicated connections the
/// migrations, the [`crate::Leader`] and the [`crate::Listener`] use (each outside the pool, platform 11.4). Cheap to
/// clone.
#[derive(Clone)]
pub struct Pool {
    inner: deadpool_postgres::Pool,
    config: Arc<tokio_postgres::Config>,
    tls: MakeRustlsConnect,
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // never the URL or the password
        f.debug_struct("Pool")
            .field("application_name", &self.config.get_application_name())
            .field("status", &self.inner.status())
            .finish_non_exhaustive()
    }
}

/// Builds the pool for `url` (`postgres://user:password@host:port/db`). Connections are opened lazily.
///
/// Whatever the URL says, TLS is required and the server certificate is not verified (`sslmode=require`; platform
/// 3.9: the `CloudNativePG` CA is short-lived, the path is the cluster's private network). Every connection gets
/// `application_name = app_name`, TCP keepalives 60/10/6, `statement_timeout`, and
/// `idle_in_transaction_session_timeout = 60s`.
///
/// # Errors
/// [`Error::Config`] when the URL cannot be parsed or `pool_size` is 0; [`Error::Tls`] when the TLS client cannot be
/// built.
pub fn connect(url: &str, app_name: &str, pool_size: usize, statement_timeout: Duration) -> Result<Pool, Error> {
    connect_recycling(
        url,
        app_name,
        pool_size,
        statement_timeout,
        RecyclingMethod::Custom(RECYCLE_SQL.to_owned()),
    )
}

/// [`connect`] for a database this process only reads: every connection also starts with
/// `default_transaction_read_only = on`, so Postgres itself refuses a write (`25006 read_only_sql_transaction`),
/// whichever statement sends it.
///
/// # Errors
/// As [`connect`].
pub fn connect_read_only(
    url: &str,
    app_name: &str,
    pool_size: usize,
    statement_timeout: Duration,
) -> Result<Pool, Error> {
    build(
        url,
        app_name,
        pool_size,
        statement_timeout,
        RecyclingMethod::Custom(RECYCLE_SQL.to_owned()),
        true,
    )
}

/// [`connect`] with another recycling method (the tests: `Fast`, to see `tx!`'s own cancellation handling without
/// the recycle backstop).
pub(crate) fn connect_recycling(
    url: &str,
    app_name: &str,
    pool_size: usize,
    statement_timeout: Duration,
    recycling_method: RecyclingMethod,
) -> Result<Pool, Error> {
    build(url, app_name, pool_size, statement_timeout, recycling_method, false)
}

fn build(
    url: &str,
    app_name: &str,
    pool_size: usize,
    statement_timeout: Duration,
    recycling_method: RecyclingMethod,
    read_only: bool,
) -> Result<Pool, Error> {
    if pool_size == 0 {
        return Err(Error::Config("pool size 0".into()));
    }
    // tokio-postgres' parse error names the bad part, never echoes the URL
    let mut config = tokio_postgres::Config::from_str(url).map_err(|e| Error::Config(e.to_string()))?;
    config
        .ssl_mode(SslMode::Require)
        .application_name(app_name)
        .connect_timeout(CONNECT_TIMEOUT)
        .keepalives(true)
        .keepalives_idle(KEEPALIVE_IDLE)
        .keepalives_interval(KEEPALIVE_INTERVAL)
        .keepalives_retries(KEEPALIVE_RETRIES)
        .options(format!(
            "-c statement_timeout={} -c idle_in_transaction_session_timeout={}{}",
            statement_timeout.as_millis(),
            IDLE_IN_TRANSACTION_TIMEOUT.as_millis(),
            if read_only {
                " -c default_transaction_read_only=on"
            } else {
                ""
            }
        ));
    let tls = tls()?;
    let manager = Manager::from_config(config.clone(), tls.clone(), ManagerConfig { recycling_method });
    let pool = deadpool_postgres::Pool::builder(manager)
        .max_size(pool_size)
        .runtime(Runtime::Tokio1)
        .wait_timeout(Some(POOL_TIMEOUT))
        .create_timeout(Some(POOL_TIMEOUT))
        .recycle_timeout(Some(POOL_TIMEOUT))
        .build()
        .map_err(|e| Error::Config(e.to_string()))?;
    Ok(Pool {
        inner: pool,
        config: Arc::new(config),
        tls,
    })
}

impl Pool {
    /// A pooled connection for statements outside a transaction (each its own). Hold it for one short piece of
    /// work; a transaction is [`crate::tx!`].
    ///
    /// # Errors
    /// [`Error::Pool`] when none is free within 5 s, the pool is closed, or the database stays away for
    /// [`crate::retry::RETRY_FOR`].
    pub async fn get(&self) -> Result<Conn, Error> {
        Ok(Conn {
            client: tokio::sync::Mutex::new(crate::retry::get(&self.inner).await?),
            pool: self.inner.clone(),
            doubt: AtomicBool::new(false),
        })
    }

    /// The pool's size, idle connections and waiters (for metrics). The deadpool pool itself is not handed out:
    /// its connections would be a way into a transaction that is not [`crate::tx!`].
    #[must_use]
    pub fn status(&self) -> deadpool_postgres::Status {
        self.inner.status()
    }

    pub(crate) fn deadpool(&self) -> &deadpool_postgres::Pool {
        &self.inner
    }

    /// Closes the pool (the last drain step, platform 3.3): waiting and later `get`s fail, idle connections close.
    pub fn close(&self) {
        self.inner.close();
    }

    /// A new connection outside the pool, with the connection future not yet driven.
    pub(crate) async fn dedicated_raw(&self) -> Result<(Client, Connection), Error> {
        Ok(self.config.connect(self.tls.clone()).await?)
    }

    /// A new connection outside the pool, driven by a spawned task that ends when the connection does.
    pub(crate) async fn dedicated(&self) -> Result<(Client, JoinHandle<()>), Error> {
        let (client, connection) = self.dedicated_raw().await?;
        let task = tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::debug!(error = %e, "dedicated postgres connection ended");
            }
        });
        Ok((client, task))
    }
}

/// A pooled connection outside any transaction: every statement commits on its own. It refuses transaction control
/// (`begin`, `commit`, `savepoint`, …) and has no `transaction()`, so a transaction is only ever [`crate::tx!`]
/// (platform 4.3). Session state it sets (`set`, temp tables, `listen`, …) lasts only until it goes back to the
/// pool, which resets the connection before its next user (`RECYCLE_SQL`). Statements are prepared once per connection and cached; a stale cached statement clears the
/// pool's caches and is retried once (`crate::stmt`).
pub struct Conn {
    /// Replaced by a new pooled connection when the database dropped this one ([`crate::retry`]).
    client: tokio::sync::Mutex<Object>,
    pool: deadpool_postgres::Pool,
    /// Unused here (a statement outside a transaction aborts nothing); [`StmtGuard`] wants one.
    doubt: AtomicBool,
}

impl fmt::Debug for Conn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conn").finish_non_exhaustive()
    }
}

impl Conn {
    #[allow(clippy::unnecessary_wraps)] // the shape `statement!` expects (Tx's can fail)
    async fn stmt_client(&self) -> Result<tokio::sync::MutexGuard<'_, Object>, Error> {
        Ok(self.client.lock().await)
    }

    fn guard(&self, sql: &str) -> Result<StmtGuard<'_>, Error> {
        stmt::refuse_control(sql, false)?;
        Ok(StmtGuard::new(&self.doubt, true))
    }

    /// On a stale cached statement: clear the pool's caches and retry once (the failed statement did nothing). On a
    /// lost connection (the database away for a moment, [`crate::retry`]): a new pooled connection, and the
    /// statement again.
    async fn recover(&self, g: &mut StmtGuard<'_>, e: tokio_postgres::Error) -> Result<(), Error> {
        if crate::error::transient_db(&e) {
            let err = Error::Db(e);
            if !g.patience.again("statement", &err) {
                return Err(err);
            }
            tokio::time::sleep(crate::retry::RETRY_GAP).await;
            let fresh = crate::retry::get(&self.pool).await?;
            let old = std::mem::replace(&mut *self.client.lock().await, fresh);
            drop(Object::take(old)); // the dead connection never goes back to the pool
            return Ok(());
        }
        if !stmt::is_stale(&e) || g.retried {
            return Err(e.into());
        }
        tracing::info!(error = %e, "stale cached statement: caches cleared, retrying");
        stmt::clear_caches(&self.pool);
        g.retried = true;
        Ok(())
    }

    /// Runs one statement; the number of rows it affected.
    ///
    /// # Errors
    /// When the statement fails, or [`Error::TransactionControl`] for `begin`, `commit`, `savepoint` and the like.
    pub async fn execute(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64, Error> {
        let mut g = self.guard(sql)?;
        statement!(self, g, sql, params, execute)
    }

    /// Runs one query; every row.
    ///
    /// # Errors
    /// As [`Conn::execute`].
    pub async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error> {
        let mut g = self.guard(sql)?;
        statement!(self, g, sql, params, query)
    }

    /// Runs one query that returns exactly one row.
    ///
    /// # Errors
    /// As [`Conn::execute`], or when it does not return exactly one row.
    pub async fn query_one(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> {
        let mut g = self.guard(sql)?;
        statement!(self, g, sql, params, query_one)
    }

    /// Runs one query that returns at most one row.
    ///
    /// # Errors
    /// As [`Conn::execute`], or when it returns more than one row.
    pub async fn query_opt(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>, Error> {
        let mut g = self.guard(sql)?;
        statement!(self, g, sql, params, query_opt)
    }

    /// Runs statements without parameters (the simple query protocol), each committing on its own.
    ///
    /// # Errors
    /// When a statement fails, or [`Error::TransactionControl`] when one controls the transaction.
    pub async fn batch_execute(&self, sql: &str) -> Result<(), Error> {
        let mut g = self.guard(sql)?;
        loop {
            let outcome = self.client.lock().await.batch_execute(sql).await;
            match outcome {
                Ok(()) => return g.ok(Ok(())),
                Err(e) => self.recover(&mut g, e).await?,
            }
        }
    }
}

/// The TLS client for `sslmode=require`: encrypted, the server certificate not verified, rustls on ring.
fn tls() -> Result<MakeRustlsConnect, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(EncryptOnly(provider.signature_verification_algorithms)))
        .with_no_client_auth();
    Ok(MakeRustlsConnect::new(config))
}

/// Accepts any server certificate (libpq's `require`) but still checks the handshake signatures.
#[derive(Debug)]
struct EncryptOnly(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for EncryptOnly {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    /// The recycle check is a backstop for a connection handed back inside a transaction (which `tx!` no longer
    /// does): deadpool discards it and opens a new one.
    #[tokio::test]
    async fn a_connection_returned_inside_a_transaction_is_never_handed_out() {
        let pool = crate::testdb::pool("recycle-test", 1);
        let other = crate::testdb::pool("recycle-test-other", 1);
        let table = format!("recycle_{}", uuid::Uuid::new_v4().simple());
        other
            .get()
            .await
            .unwrap()
            .batch_execute(&format!("create table {table} (id int)"))
            .await
            .unwrap();
        for i in 0..5 {
            let raw = pool.deadpool().get().await.unwrap();
            raw.batch_execute("begin").await.unwrap();
            let in_tx_pid: i32 = raw.query_one("select pg_backend_pid()", &[]).await.unwrap().get(0);
            drop(raw); // back to the pool, still inside the transaction
            let conn = pool.get().await.unwrap();
            let pid: i32 = conn.query_one("select pg_backend_pid()", &[]).await.unwrap().get(0);
            assert_ne!(
                pid, in_tx_pid,
                "round {i}: the connection inside a transaction was handed out"
            );
            conn.execute(&format!("insert into {table} values ($1)"), &[&i])
                .await
                .unwrap();
            let seen: i64 = other
                .get()
                .await
                .unwrap()
                .query_one(&format!("select count(*) from {table} where id = $1"), &[&i])
                .await
                .unwrap()
                .get(0);
            assert_eq!(seen, 1, "round {i}: an autocommit insert was not committed");
        }
        other
            .get()
            .await
            .unwrap()
            .batch_execute(&format!("drop table {table}"))
            .await
            .unwrap();
    }
}
