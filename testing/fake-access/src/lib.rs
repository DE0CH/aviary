//! fake-access (platform 7.4): a Cloudflare Access issuer for the harness.
//!
//! - `GET /cdn-cgi/access/certs`: the JWKS (RS256), every configured key (two for the rotation test);
//! - `GET /cdn-cgi/access/get-identity`: the `CF_Authorization` cookie's token verified → `{"email", "name",
//!   "given_name"}`, 401 for a missing, tampered, expired or unknown-key token;
//! - `POST /__mint` `{"email", "name", "aud", "expSeconds", "kid"?}` → `{"token"}` for the driver.
//!
//! Tokens carry Cloudflare Access's application-token claims (`aud` as a list, `email`, `exp`, `iat`, `nbf`,
//! `iss` = `https://<team domain>`, `type: "app"`, `identity_nonce`, `sub`, `country`) plus `name`, which this fake
//! answers from on `get-identity` (Cloudflare keeps it on its side).
//!
//! RSA keys are not generated here: ring signs and verifies RSA but cannot generate keys, and the pure-Rust `rsa`
//! crate carries an unfixed advisory (RUSTSEC-2023-0071) that `cargo audit --deny warnings` refuses. The harness
//! generates the per-run key pairs (`openssl genpkey`) and passes PKCS#8 PEM files.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::SystemRandom;
use ring::signature::{self, RsaKeyPair};
use serde::Deserialize;
use serde_json::{Value, json};

/// The JWKS path.
pub const CERTS: &str = "/cdn-cgi/access/certs";
/// The identity path.
pub const GET_IDENTITY: &str = "/cdn-cgi/access/get-identity";
/// The driver's minting path.
pub const MINT: &str = "/__mint";
/// The cookie Cloudflare Access sets.
pub const COOKIE: &str = "CF_Authorization";

/// Why a key or token was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refused(s: impl Into<String>) -> Refused {
    Refused(s.into())
}

/// One signing key.
pub struct Key {
    kid: String,
    pair: RsaKeyPair,
    n: Vec<u8>,
    e: Vec<u8>,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key").field("kid", &self.kid).finish_non_exhaustive()
    }
}

impl Key {
    /// A key from its PKCS#8 DER.
    ///
    /// # Errors
    /// When the bytes are not an RSA PKCS#8 key ring accepts (2048-8192 bits).
    pub fn from_pkcs8(kid: &str, der: &[u8]) -> Result<Self, Refused> {
        let pair = RsaKeyPair::from_pkcs8(der).map_err(|e| refused(format!("key {kid}: {e}")))?;
        let c: ring::rsa::PublicKeyComponents<Vec<u8>> = pair.public().into();
        Ok(Self {
            kid: kid.to_owned(),
            pair,
            n: c.n,
            e: c.e,
        })
    }

    /// A key from a PKCS#8 PEM file's text.
    ///
    /// # Errors
    /// When the text is not one PKCS#8 PEM block of an RSA key.
    pub fn from_pem(kid: &str, pem: &str) -> Result<Self, Refused> {
        use rustls::pki_types::PrivatePkcs8KeyDer;
        use rustls::pki_types::pem::PemObject as _;
        let der = PrivatePkcs8KeyDer::from_pem_slice(pem.as_bytes()).map_err(|e| refused(format!("key {kid}: {e}")))?;
        Self::from_pkcs8(kid, der.secret_pkcs8_der())
    }

    /// The key's id.
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    fn jwk(&self) -> Value {
        json!({"kid": self.kid, "kty": "RSA", "alg": "RS256", "use": "sig", "n": B64.encode(&self.n), "e": B64.encode(&self.e)})
    }
}

/// What `/__mint` takes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MintRequest {
    /// The person.
    pub email: String,
    /// Display name (`get-identity`'s `name`; `given_name` is its first word).
    #[serde(default)]
    pub name: Option<String>,
    /// The Access application audience tag.
    pub aud: String,
    /// Lifetime from now; negative mints an expired token.
    pub exp_seconds: i64,
    /// Which key signs (default: the first).
    #[serde(default)]
    pub kid: Option<String>,
}

/// The issuer.
#[derive(Debug)]
pub struct Issuer {
    keys: Vec<Key>,
    team_domain: String,
    rng: SystemRandom,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl Issuer {
    /// An issuer for `team_domain` (the `iss` is `https://<team_domain>`) signing with `keys`.
    ///
    /// # Errors
    /// With no keys or two keys of one kid.
    pub fn new(team_domain: &str, keys: Vec<Key>) -> Result<Arc<Self>, Refused> {
        if keys.is_empty() {
            return Err(refused("no keys"));
        }
        for (i, k) in keys.iter().enumerate() {
            if keys[..i].iter().any(|o| o.kid == k.kid) {
                return Err(refused(format!("kid {} twice", k.kid)));
            }
        }
        Ok(Arc::new(Self {
            keys,
            team_domain: team_domain.to_owned(),
            rng: SystemRandom::new(),
        }))
    }

    /// The JWKS.
    #[must_use]
    pub fn jwks(&self) -> Value {
        json!({"keys": self.keys.iter().map(Key::jwk).collect::<Vec<_>>()})
    }

    /// A signed application token.
    ///
    /// # Errors
    /// An unknown kid, or a signing failure.
    pub fn mint(&self, req: &MintRequest) -> Result<String, Refused> {
        let key = match &req.kid {
            Some(k) => self
                .keys
                .iter()
                .find(|x| x.kid == *k)
                .ok_or_else(|| refused(format!("no key {k}")))?,
            None => &self.keys[0],
        };
        let iat = now();
        let header = json!({"alg": "RS256", "kid": key.kid, "typ": "JWT"});
        let claims = json!({
            "aud": [req.aud],
            "email": req.email,
            "exp": iat + req.exp_seconds,
            "iat": iat,
            "nbf": iat,
            "iss": format!("https://{}", self.team_domain),
            "type": "app",
            "identity_nonce": uuid::Uuid::new_v4().simple().to_string(),
            "sub": uuid::Uuid::new_v4().to_string(),
            "country": "HK",
            "name": req.name,
        });
        let signing_input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
        let mut sig = vec![0; key.pair.public().modulus_len()];
        key.pair
            .sign(
                &signature::RSA_PKCS1_SHA256,
                &self.rng,
                signing_input.as_bytes(),
                &mut sig,
            )
            .map_err(|_| refused("signing failed"))?;
        Ok(format!("{signing_input}.{}", B64.encode(sig)))
    }

    /// The claims of a token this issuer signed, if it verifies and has not expired.
    ///
    /// # Errors
    /// Malformed, unknown kid, bad signature, wrong issuer, expired.
    pub fn verify(&self, token: &str) -> Result<Value, Refused> {
        let mut parts = token.split('.');
        let (Some(h), Some(c), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(refused("malformed"));
        };
        let decode = |p: &str| B64.decode(p).map_err(|_| refused("malformed"));
        let header: Value = serde_json::from_slice(&decode(h)?).map_err(|_| refused("malformed"))?;
        let kid = header["kid"].as_str().unwrap_or_default();
        let key = self
            .keys
            .iter()
            .find(|k| k.kid == kid)
            .ok_or_else(|| refused(format!("unknown kid {kid}")))?;
        let public = ring::rsa::PublicKeyComponents {
            n: &key.n[..],
            e: &key.e[..],
        };
        public
            .verify(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                format!("{h}.{c}").as_bytes(),
                &decode(s)?,
            )
            .map_err(|_| refused("bad signature"))?;
        let claims: Value = serde_json::from_slice(&decode(c)?).map_err(|_| refused("malformed"))?;
        if claims["iss"] != format!("https://{}", self.team_domain) {
            return Err(refused("wrong issuer"));
        }
        if claims["exp"].as_i64().is_none_or(|e| e <= now()) {
            return Err(refused("expired"));
        }
        Ok(claims)
    }

    /// The router.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route(CERTS, get(certs))
            .route(GET_IDENTITY, get(identity))
            .route(MINT, post(mint))
            .with_state(self.clone())
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

async fn certs(State(i): State<Arc<Issuer>>) -> Response {
    aviary_core::http::json(StatusCode::OK, &i.jwks())
}

async fn identity(State(i): State<Arc<Issuer>>, headers: HeaderMap) -> Response {
    let Some(token) = cookie(&headers, COOKIE) else {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"err": "no CF_Authorization cookie"})),
        )
            .into_response();
    };
    match i.verify(&token) {
        Ok(c) => {
            let name = c["name"].as_str().unwrap_or_default().to_owned();
            let given = name.split_whitespace().next().unwrap_or_default().to_owned();
            aviary_core::http::json(
                StatusCode::OK,
                &json!({"email": c["email"], "name": name, "given_name": given}),
            )
        }
        Err(e) => (StatusCode::UNAUTHORIZED, axum::Json(json!({"err": e.0}))).into_response(),
    }
}

async fn mint(State(i): State<Arc<Issuer>>, body: Bytes) -> Response {
    let req: MintRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, axum::Json(json!({"err": e.to_string()}))).into_response(),
    };
    match i.mint(&req) {
        Ok(token) => aviary_core::http::json(StatusCode::OK, &json!({"token": token})),
        Err(e) => (StatusCode::BAD_REQUEST, axum::Json(json!({"err": e.0}))).into_response(),
    }
}

/// Serve `router` with TLS from a PEM certificate chain and a PKCS#8 PEM key, until the listener fails.
///
/// # Errors
/// A certificate or key that does not load.
pub async fn serve_tls(
    listener: tokio::net::TcpListener,
    router: Router,
    cert_pem: &str,
    key_pem: &str,
) -> anyhow::Result<()> {
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs = CertificateDer::pem_slice_iter(cert_pem.as_bytes()).collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    loop {
        let (stream, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(stream).await else { return };
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
