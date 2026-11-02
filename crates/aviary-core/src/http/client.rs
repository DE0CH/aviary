//! The outbound HTTP client (platform 3.9): `reqwest` on rustls (ring) with the system trust store, which honours
//! `SSL_CERT_FILE`; HTTP/1.1 keep-alive pools; `User-Agent: <svc>/<sha12>`; `Aviary-Protocol` on every call.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue};
use uuid::Uuid;

use super::headers::{AVIARY_PROTOCOL, AVIARY_REQUEST};
use super::middleware::current_request_id;
use crate::version::{Build, Protocol};

/// How long an idle pooled connection is kept.
const POOL_IDLE: Duration = Duration::from_secs(90);
/// Idle connections kept per host.
const POOL_PER_HOST: usize = 32;
/// Connect timeout of every call (each call sets its own overall timeout where its spec names one).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The default bound on a stalled call: how long a read may wait for the next bytes of an answer (the head, or the
/// next chunk of a body). It bounds a call whose site forgot its own `.timeout()`, and is long enough for every
/// held-open answer of the platform (long-polls of 25-30 s, 3.11.5) and for streams that send nothing for a while.
/// A client whose callee stays silent longer overrides it on the builder (`.read_timeout(…)`).
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);
/// TCP keepalive.
const TCP_KEEPALIVE: Duration = Duration::from_secs(60);

fn roots() -> Arc<rustls::RootCertStore> {
    static ROOTS: OnceLock<Arc<rustls::RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let found = rustls_native_certs::load_native_certs();
            for e in &found.errors {
                tracing::warn!(error = %e, "trust store entry not loaded");
            }
            let mut store = rustls::RootCertStore::empty();
            let (added, ignored) = store.add_parsable_certificates(found.certs);
            if ignored > 0 || added == 0 {
                tracing::warn!(added, ignored, "trust store loaded");
            }
            Arc::new(store)
        })
        .clone()
}

/// The TLS configuration of every outbound HTTPS call: rustls on ring, the system trust store (loaded once).
#[must_use]
pub fn tls_config() -> rustls::ClientConfig {
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(rustls::DEFAULT_VERSIONS)
        // ring supports every default version, so this never falls back; the fallback is the same provider
        .unwrap_or_else(|_| rustls::ClientConfig::builder())
        .with_root_certificates(roots())
        .with_no_client_auth()
}

/// A `reqwest` builder for calls from service `build` to one surface: TLS of [`tls_config`], `User-Agent:
/// <svc>/<sha12>`, `Aviary-Protocol: <surface>=<n>` on every request when `protocol` is given (one client per callee
/// surface), HTTP/1.1 with a keep-alive pool, a 5 s connect timeout and a [`DEFAULT_READ_TIMEOUT`] read timeout.
/// Each call sets its own overall timeout (`RequestBuilder::timeout`) where its spec names one; under
/// [`crate::http::retry::send`] that timeout applies to each attempt, so the worst case is
/// [`crate::http::retry::Backoff::worst_case`].
pub fn client_builder(build: &Build, protocol: Option<Protocol>) -> reqwest::ClientBuilder {
    let mut headers = HeaderMap::new();
    if let Some(p) = protocol
        && let Ok(v) = HeaderValue::from_str(&p.to_string())
    {
        headers.insert(AVIARY_PROTOCOL, v);
    }
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls_config())
        .user_agent(build.user_agent())
        .default_headers(headers)
        .http1_only()
        .pool_idle_timeout(POOL_IDLE)
        .pool_max_idle_per_host(POOL_PER_HOST)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(DEFAULT_READ_TIMEOUT)
        .tcp_keepalive(TCP_KEEPALIVE)
}

/// Aviary additions to `reqwest`'s request builder.
pub trait RequestBuilderExt {
    /// Sends `Aviary-Request`: the id of the request being handled ([`current_request_id`]), or a new v7 id for
    /// the service's own work.
    #[must_use]
    fn forward_request_id(self) -> Self;
}

impl RequestBuilderExt for reqwest::RequestBuilder {
    fn forward_request_id(self) -> Self {
        let id = current_request_id().unwrap_or_else(Uuid::now_v7);
        self.header(AVIARY_REQUEST, id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;

    #[tokio::test]
    async fn builder_sets_user_agent_and_protocol() {
        let router = Router::new().route(
            "/h",
            get(|h: HeaderMap| async move {
                format!(
                    "{}|{}",
                    h.get("user-agent").map_or("", |v| v.to_str().unwrap()),
                    h.get("aviary-protocol").map_or("", |v| v.to_str().unwrap())
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let build = Build::new("pigeon");
        let c = client_builder(&build, Some(Protocol::new("magpie-person", 1)))
            .build()
            .unwrap();
        let got = c
            .get(format!("http://{addr}/h"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(got, format!("pigeon/{}|magpie-person=1", crate::version::sha12()));

        let c = client_builder(&build, None).build().unwrap();
        let got = c
            .get(format!("http://{addr}/h"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(got, format!("pigeon/{}|", crate::version::sha12()));
    }

    #[tokio::test]
    async fn a_stalled_callee_does_not_hang_a_call_forever() {
        // a callee that accepts and never answers; the builder's read timeout ends the call
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        let c = client_builder(&Build::new("test"), None)
            .read_timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let started = std::time::Instant::now();
        let e = c.get(format!("http://{addr}/x")).send().await.unwrap_err();
        assert!(e.is_timeout(), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn tls_config_uses_ring() {
        let c = tls_config();
        assert_ne!(c.crypto_provider().cipher_suites.len(), 0);
    }
}
