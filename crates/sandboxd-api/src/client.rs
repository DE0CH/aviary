//! The typed client (hermit spec 13.15): HTTPS to `https://sandboxd:<port>` resolved to the host's overlay address, so
//! the server certificate's name `sandboxd` is checked while connecting by address; TLS 1.3, the fabric CA, a client
//! identity; keep-alive; at most [`MAX_CONNECTIONS`] requests at once per host (sandboxd serves 16).
//!
//! Of the [`MAX_CONNECTIONS`], one is kept for the heartbeat ([`Client::host`]) and the others are shared by every
//! other call, so creates, sends and image pulls (minutes each) never make a heartbeat wait. A call's timeout covers
//! its wait for a slot as well as the request: a call answers or fails within its timeout.
//!
//! Timeouts (hermit spec 13.15): heartbeat 5 s, create 15 min, send 15 min, receive wait 95 s, others 30 s, with two
//! recorded deviations: `POST /v1/images` gets [`IMAGE_TIMEOUT`] (21 min) because sandboxd itself waits up to 20 min
//! for a pull of the same digest (crates/sandboxd/src/images.rs) and a cold pull takes minutes, and
//! `POST …/receive` gets [`CREATE_TIMEOUT`] because it prepares the sandbox's bundle as a create does (including the
//! image pull). `GET …/log` (5 s) comes with that route (hermit spec 13.2 item 10).

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
    Deleted, EgressTargets, EnsureImage, ErrorBody, Host, ImageDigest, Listed, Peer, Peers, ReceiveResult, Receiving,
    Sandboxes, SendRequest, SendResult, SetPeers, Spec, routes,
};

/// Requests in flight per host at most: [`HEARTBEAT_SLOTS`] for [`Client::host`], the rest for every other call.
pub const MAX_CONNECTIONS: usize = 8;
/// The slots kept for the heartbeat (`GET /v1/host`, one at a time per host).
pub const HEARTBEAT_SLOTS: usize = 1;
/// `GET /v1/host` (the heartbeat).
pub const HOST_TIMEOUT: Duration = Duration::from_secs(5);
/// A create (it may pull and unpack the image).
pub const CREATE_TIMEOUT: Duration = Duration::from_mins(15);
/// A send (a whole migration).
pub const SEND_TIMEOUT: Duration = Duration::from_mins(15);
/// `POST /v1/images`: at least sandboxd's own wait for a pull of the same digest (20 min).
pub const IMAGE_TIMEOUT: Duration = Duration::from_mins(21);
/// `GET …/receive?wait=<ms>`; the client asks sandboxd to wait at most [`RECEIVE_WAIT_MS`] of it.
pub const RECEIVE_TIMEOUT: Duration = Duration::from_secs(95);
/// The longest `wait` the client asks for: [`RECEIVE_TIMEOUT`] less 5 s for the answer to arrive.
pub const RECEIVE_WAIT_MS: u64 = 90_000;
/// Every other call.
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
    /// No answer (connect, TLS, reset).
    #[error("sandboxd unreachable: {0}")]
    Transport(#[from] reqwest::Error),
    /// No answer within the call's timeout (the wait for a slot included).
    #[error("sandboxd did not answer within {0:?}")]
    Timeout(Duration),
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
    /// Every call but the heartbeat.
    slots: Arc<Semaphore>,
    /// The heartbeat's own.
    beat: Arc<Semaphore>,
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
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS - HEARTBEAT_SLOTS)),
            beat: Arc::new(Semaphore::new(HEARTBEAT_SLOTS)),
        }
    }

    async fn call<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<T, Error> {
        self.call_in(&self.slots, method, path, body, timeout).await
    }

    async fn call_in<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        slots: &Semaphore,
        method: Method,
        path: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<T, Error> {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(b) = body {
            req = req
                .header("content-type", "application/json")
                .body(serde_json::to_vec(b)?);
        }
        let exchange = async {
            // the semaphore is never closed
            let _slot = slots.acquire().await.map_err(|e| Error::Tls(e.to_string()))?;
            let res = req.send().await?;
            let status = res.status().as_u16();
            let bytes = res.bytes().await?;
            Ok::<_, Error>((status, bytes))
        };
        let (status, bytes) = tokio::time::timeout(timeout, exchange)
            .await
            .map_err(|_| Error::Timeout(timeout))??;
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
        self.call_in::<(), _>(&self.beat, Method::GET, routes::HOST, None, HOST_TIMEOUT)
            .await
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
        self.call(Method::POST, routes::IMAGES, Some(image), IMAGE_TIMEOUT)
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

    /// `GET /v1/sandboxes/{name}/receive?wait=<ms>`: the receive's result, waiting up to `wait_ms` (at most
    /// [`RECEIVE_WAIT_MS`]) for it to end.
    ///
    /// # Errors
    /// [`Error`].
    pub async fn receive_result(&self, name: &str, wait_ms: u64) -> Result<ReceiveResult, Error> {
        let wait = wait_ms.min(RECEIVE_WAIT_MS);
        let path = format!("{}?wait={wait}", routes::sandbox_part(name, "receive"));
        self.call::<(), _>(Method::GET, &path, None, RECEIVE_TIMEOUT).await
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
