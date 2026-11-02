//! A canary for the build path (platform 4.1, 5.1): the workspace's TLS and HTTP clients, built the way the services
//! build them. ring (under rustls) compiles C with cc-rs, so the musl build needs a musl C compiler
//! (`.cargo/config.toml` sets `CC_x86_64_unknown_linux_musl`); CI's build job compiles, links and runs this crate's
//! unit tests for `x86_64-unknown-linux-musl`, and the test job runs `tests/` against the CI Postgres and S3.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};

/// The one crypto provider: ring (no aws-lc anywhere).
#[must_use]
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// An HTTPS client for the AWS SDK on rustls with ring (`aws-smithy-http-client` feature `rustls-ring`). The SDK
/// crates have their default features off, so without this they have no HTTP client at all.
#[must_use]
pub fn aws_http_client() -> aws_sdk_s3::config::SharedHttpClient {
    use aws_smithy_http_client::tls;
    aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
        .build_https()
}

/// A reqwest client on the ring provider (reqwest's `rustls-no-provider` feature needs a preconfigured config).
///
/// # Errors
/// When the TLS config or the client cannot be built.
pub fn reqwest_client() -> Result<reqwest::Client, Box<dyn std::error::Error + Send + Sync>> {
    // An empty root store is enough to prove the build; services load their roots in aviary-core.
    let roots = rustls::RootCertStore::empty();
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(reqwest::Client::builder().tls_backend_preconfigured(config).build()?)
}

/// A Postgres connector for `sslmode=require`: encrypted, the server certificate not verified (the semantics of libpq's
/// `require`; the CI server's certificate is a throwaway self-signed one).
///
/// # Errors
/// When the TLS config cannot be built.
pub fn postgres_tls() -> Result<tokio_postgres_rustls::MakeRustlsConnect, rustls::Error> {
    let provider = provider();
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(EncryptOnly(provider.signature_verification_algorithms)))
        .with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

/// Accepts any server certificate but still checks the handshake signatures.
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
    use super::*;

    #[test]
    fn ring_provider_has_cipher_suites() {
        assert_ne!(provider().cipher_suites.len(), 0);
    }

    #[test]
    fn clients_build() {
        reqwest_client().unwrap();
        postgres_tls().unwrap();
        let _ = aws_http_client();
    }

    #[test]
    fn ring_computes_a_digest() {
        // runs ring's C/assembly code, linked into the (static, for musl) test binary
        let d = ring::digest::digest(&ring::digest::SHA256, b"abc");
        assert_eq!(d.as_ref()[..4], [0xba, 0x78, 0x16, 0xbf], "SHA-256(abc) = ba7816bf...");
    }
}
