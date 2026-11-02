//! The typed client (hermit spec 13.15): HTTPS to `https://sandboxd:<port>` resolved to the host's overlay address, so
//! the server certificate's name `sandboxd` is checked while connecting by address; TLS 1.3, the fabric CA, a client
//! identity; keep-alive; at most [`MAX_CONNECTIONS`] requests at once per host (sandboxd serves 16).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;

use crate::{
    Deleted, EgressTargets, EnsureImage, ErrorBody, Host, ImageDigest, Listed, Peer, Peers, RECEIVE_WAIT_MAX_MS,
    ReceiveResult, Receiving, Sandboxes, SendRequest, SendResult, SetPeers, Spec, routes,
};

/// Requests in flight per host at most.
pub const MAX_CONNECTIONS: usize = 8;
/// `GET /v1/host` (the heartbeat).
pub const HOST_TIMEOUT: Duration = Duration::from_secs(5);
/// A create (it may pull and unpack the image).
pub const CREATE_TIMEOUT: Duration = Duration::from_mins(15);
/// A send (a whole migration).
pub const SEND_TIMEOUT: Duration = Duration::from_mins(15);
/// Every other call (a receive's `wait` adds to it).
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The PEM files of the client's TLS identity.
#[derive(Clone)]
pub struct Tls {
    /// The fabric CA (or a bundle of CAs during a rotation).
    pub ca_pem: String,
    /// The client certificate (CN [`CLIENT_CN`](crate::CLIENT_CN)), with its chain.
    pub cert_pem: String,
    /// Its private key.
    pub key_pem: String,
}

impl std::fmt::Debug for Tls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tls").finish_non_exhaustive()
    }
}

/// A failed call.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// sandboxd answered with an error status; `body` is its `error` message (or the raw body).
    #[error("sandboxd {status}: {body}")]
    Status {
        /// The HTTP status.
        status: u16,
        /// The message.
        body: String,
    },
    /// No answer (connect, TLS, timeout, reset).
    #[error("sandboxd unreachable: {0}")]
    Transport(#[from] reqwest::Error),
    /// An answer that is not the route's type.
    #[error("sandboxd answered something unexpected: {0}")]
    Decode(#[from] serde_json::Error),
    /// The TLS identity could not be loaded.
    #[error("sandboxd client TLS: {0}")]
    Tls(String),
}

/// A client for one host's sandboxd.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    slots: Arc<Semaphore>,
}

fn tls_config(tls: &Tls) -> Result<rustls::ClientConfig, Error> {
    let bad = |what: &str, e: &dyn std::fmt::Display| Error::Tls(format!("{what}: {e}"));
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(tls.ca_pem.as_bytes()) {
        roots.add(c.map_err(|e| bad("CA", &e))?).map_err(|e| bad("CA", &e))?;
    }
    let chain = CertificateDer::pem_slice_iter(tls.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| bad("certificate", &e))?;
    let key = PrivateKeyDer::from_pem_slice(tls.key_pem.as_bytes()).map_err(|e| bad("key", &e))?;
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| bad("TLS 1.3", &e))?
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .map_err(|e| bad("client identity", &e))
}

impl Client {
    /// The client of the sandboxd at `addr:port` (its overlay address), with mutual TLS.
    ///
    /// # Errors
    /// When the PEM files do not load or the HTTP client cannot be built.
    pub fn new(addr: IpAddr, port: u16, tls: &Tls) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(tls_config(tls)?)
            .resolve(crate::SERVER_NAME, SocketAddr::new(addr, port))
            .pool_max_idle_per_host(MAX_CONNECTIONS)
            .http1_only()
            .user_agent("sandboxd-api")
            .build()?;
        Ok(Self::with(http, format!("https://{}:{port}", crate::SERVER_NAME)))
    }

    /// A client of a plain-HTTP server of this API at `base` (`http://host:port`): tests and harness fakes only.
    ///
    /// # Errors
    /// When the HTTP client cannot be built.
    pub fn plain(base: &str) -> Result<Self, Error> {
        // no TLS is used, but reqwest (rustls without a process-wide provider) needs a configuration to build
        let none = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| Error::Tls(e.to_string()))?
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(none)
            .pool_max_idle_per_host(MAX_CONNECTIONS)
            .http1_only()
            .build()?;
        Ok(Self::with(http, base.trim_end_matches('/').to_owned()))
    }

    fn with(http: reqwest::Client, base: String) -> Self {
        Self {
            http,
            base,
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }

    async fn call<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<T, Error> {
        let _slot = self.slots.acquire().await.map_err(|e| Error::Tls(e.to_string()))?;
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .timeout(timeout);
        if let Some(b) = body {
            req = req
                .header("content-type", "application/json")
                .body(serde_json::to_vec(b)?);
        }
        let res = req.send().await?;
        let status = res.status().as_u16();
        let bytes = res.bytes().await?;
        if status >= 400 {
            let body = serde_json::from_slice::<ErrorBody>(&bytes)
                .map_or_else(|_| String::from_utf8_lossy(&bytes).into_owned(), |e| e.error);
            return Err(Error::Status { status, body });
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// `GET /v1/host`.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn host(&self) -> Result<Host, Error> {
        self.call::<(), _>(Method::GET, routes::HOST, None, HOST_TIMEOUT).await
    }

    /// `PUT /v1/peers`: wg0's peers, exactly these.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn set_peers(&self, peers: Vec<Peer>) -> Result<Peers, Error> {
        self.call(Method::PUT, routes::PEERS, Some(&SetPeers { peers }), TIMEOUT)
            .await
    }

    /// `PUT /v1/egress` (hub only).
    ///
    /// # Errors
    /// [`Error`].
    pub async fn set_egress(&self, targets: Vec<String>) -> Result<EgressTargets, Error> {
        self.call(Method::PUT, routes::EGRESS, Some(&EgressTargets { targets }), TIMEOUT)
            .await
    }

    /// `POST /v1/images`.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn ensure_image(&self, image: &EnsureImage) -> Result<ImageDigest, Error> {
        self.call(Method::POST, routes::IMAGES, Some(image), CREATE_TIMEOUT)
            .await
    }

    /// `GET /v1/sandboxes`.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn list(&self) -> Result<Sandboxes, Error> {
        self.call::<(), _>(Method::GET, routes::SANDBOXES, None, TIMEOUT).await
    }

    /// `POST /v1/sandboxes`.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn create(&self, spec: &Spec) -> Result<Listed, Error> {
        self.call(Method::POST, routes::SANDBOXES, Some(spec), CREATE_TIMEOUT)
            .await
    }

    /// `DELETE /v1/sandboxes/{name}`.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn delete(&self, name: &str) -> Result<Deleted, Error> {
        self.call::<(), _>(Method::DELETE, &routes::sandbox(name), None, TIMEOUT)
            .await
    }

    /// `GET /v1/sandboxes/{name}/spec`.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn spec(&self, name: &str) -> Result<Spec, Error> {
        self.call::<(), _>(Method::GET, &routes::sandbox_part(name, "spec"), None, TIMEOUT)
            .await
    }

    /// `POST /v1/sandboxes/{name}/receive`: prepare as a migration target.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn receive(&self, spec: &Spec) -> Result<Receiving, Error> {
        self.call(
            Method::POST,
            &routes::sandbox_part(&spec.name, "receive"),
            Some(spec),
            CREATE_TIMEOUT,
        )
        .await
    }

    /// `GET /v1/sandboxes/{name}/receive?wait=<ms>`: the receive's result, waiting up to `wait_ms` for it to end.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn receive_result(&self, name: &str, wait_ms: u64) -> Result<ReceiveResult, Error> {
        let wait = wait_ms.min(RECEIVE_WAIT_MAX_MS);
        let path = format!("{}?wait={wait}", routes::sandbox_part(name, "receive"));
        self.call::<(), _>(Method::GET, &path, None, TIMEOUT + Duration::from_millis(wait))
            .await
    }

    /// `POST /v1/sandboxes/{name}/send`: move it to a receive.
    ///
    /// # Errors
    /// [`Error`] (409: not moved, still running here; 502: lost after the switch).
    pub async fn send(&self, name: &str, req: &SendRequest) -> Result<SendResult, Error> {
        self.call(
            Method::POST,
            &routes::sandbox_part(name, "send"),
            Some(req),
            SEND_TIMEOUT,
        )
        .await
    }
}
