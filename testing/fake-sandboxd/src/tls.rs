//! An axum router on a TLS listener, with or without a required client certificate.

use std::sync::Arc;

use anyhow::Context as _;
use axum::Router;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// The server's identity and, for mutual TLS, the CA its clients' certificates must chain to.
#[derive(Clone)]
pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
    pub client_ca_pem: Option<String>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("mutual", &self.client_ca_pem.is_some())
            .finish_non_exhaustive()
    }
}

fn config(id: &Identity) -> anyhow::Result<rustls::ServerConfig> {
    let certs = CertificateDer::pem_slice_iter(id.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .context("certificate")?;
    let key = PrivateKeyDer::from_pem_slice(id.key_pem.as_bytes()).context("key")?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder =
        rustls::ServerConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
    let builder = match &id.client_ca_pem {
        Some(ca) => {
            let mut roots = rustls::RootCertStore::empty();
            for c in CertificateDer::pem_slice_iter(ca.as_bytes()) {
                roots.add(c.context("client CA")?)?;
            }
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .context("client verifier")?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    Ok(builder.with_single_cert(certs, key)?)
}

/// Serves `router` on `listener` over TLS until the process ends.
///
/// # Errors
/// A bad certificate or key.
pub async fn serve(listener: tokio::net::TcpListener, router: Router, id: &Identity) -> anyhow::Result<()> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config(id)?));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                eprintln!("fake-sandboxd: accept: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(stream).await else {
                return;
            };
            let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let mut router = router.clone();
                async move {
                    use tower::Service as _;
                    router.call(req.map(axum::body::Body::new)).await
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                .await;
        });
    }
}
