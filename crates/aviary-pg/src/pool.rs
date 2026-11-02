//! The connection pool and dedicated connections (platform 3.9, 4.3).

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use deadpool_postgres::{Manager, ManagerConfig, Object, RecyclingMethod, Runtime};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::task::JoinHandle;
use tokio_postgres::config::SslMode;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{Client, Socket};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::Error;

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
            "-c statement_timeout={} -c idle_in_transaction_session_timeout={}",
            statement_timeout.as_millis(),
            IDLE_IN_TRANSACTION_TIMEOUT.as_millis()
        ));
    let tls = tls()?;
    let manager = Manager::from_config(
        config.clone(),
        tls.clone(),
        ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        },
    );
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
    /// A pooled connection. Hold it for one short piece of work; never across a network call.
    ///
    /// # Errors
    /// [`Error::Pool`] when none is free within 5 s, a new one cannot be opened, or the pool is closed.
    pub async fn get(&self) -> Result<Object, Error> {
        Ok(self.inner.get().await?)
    }

    /// The underlying deadpool pool (status for metrics).
    #[must_use]
    pub fn deadpool(&self) -> &deadpool_postgres::Pool {
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
