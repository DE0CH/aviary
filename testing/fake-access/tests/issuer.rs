//! fake-access: minted tokens verify against the published JWKS (as pigeon's verifier will check them), the
//! identity route answers for good tokens and 401s the rest, both keys work, a key published or retired at run time
//! changes what verifies (rotation), and TLS serves with a harness-CA certificate. Keys and certificates are generated per run with `openssl`, as the harness does.
#![allow(clippy::unwrap_used)] // tests

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use fake_access::{Issuer, Key, MintRequest};
use serde_json::{Value, json};

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("fake-access-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn openssl(args: &[&str]) {
    let out = Command::new("openssl").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "openssl {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rsa_key(dir: &Path, name: &str) -> String {
    let p = dir.join(name);
    openssl(&[
        "genpkey",
        "-algorithm",
        "RSA",
        "-pkeyopt",
        "rsa_keygen_bits:2048",
        "-out",
        p.to_str().unwrap(),
    ]);
    std::fs::read_to_string(p).unwrap()
}

fn issuer(dir: &Path) -> std::sync::Arc<Issuer> {
    let k1 = Key::from_pem("k1", &rsa_key(dir, "k1.pem")).unwrap();
    let k2 = Key::from_pem("k2", &rsa_key(dir, "k2.pem")).unwrap();
    Issuer::new("access.aviary-e2e.svc.cluster.local", vec![k1, k2]).unwrap()
}

fn mint(email: &str, exp: i64, kid: Option<&str>) -> MintRequest {
    MintRequest {
        email: email.into(),
        name: Some("Chan Tai Man".into()),
        aud: "aud-tag".into(),
        exp_seconds: exp,
        kid: kid.map(str::to_owned),
    }
}

/// Verify a token with ring against a JWKS, the way any RS256 verifier does (independently of the issuer).
fn verify_with_jwks(jwks: &Value, token: &str) -> Value {
    try_verify_with_jwks(jwks, token).unwrap_or_else(|| panic!("the token's kid is not in the JWKS"))
}

/// As [`verify_with_jwks`], `None` when the token's kid is not in the JWKS.
fn try_verify_with_jwks(jwks: &Value, token: &str) -> Option<Value> {
    let parts: Vec<&str> = token.split('.').collect();
    let header: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["alg"], "RS256");
    let jwk = jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kid"] == header["kid"])?;
    let n = B64.decode(jwk["n"].as_str().unwrap()).unwrap();
    let e = B64.decode(jwk["e"].as_str().unwrap()).unwrap();
    ring::rsa::PublicKeyComponents { n: &n[..], e: &e[..] }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &B64.decode(parts[2]).unwrap(),
        )
        .unwrap();
    Some(serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap())
}

#[test]
fn tokens_verify_with_the_published_keys() {
    let dir = tmp();
    let i = issuer(&dir);
    let jwks = i.jwks();
    assert_eq!(jwks["keys"].as_array().unwrap().len(), 2);
    for kid in [None, Some("k2")] {
        let t = i.mint(&mint("staff@example.org", 600, kid)).unwrap();
        let claims = verify_with_jwks(&jwks, &t);
        assert_eq!(claims["email"], "staff@example.org");
        assert_eq!(claims["aud"], json!(["aud-tag"]));
        assert_eq!(claims["iss"], "https://access.aviary-e2e.svc.cluster.local");
        assert_eq!(claims["type"], "app");
        assert_eq!(i.verify(&t).unwrap()["email"], "staff@example.org");
    }
    let expired = i.mint(&mint("staff@example.org", -10, None)).unwrap();
    assert_eq!(i.verify(&expired).unwrap_err().0, "expired");
    let good = i.mint(&mint("staff@example.org", 600, None)).unwrap();
    let mut parts: Vec<String> = good.split('.').map(str::to_owned).collect();
    parts[1] = B64.encode(br#"{"email":"admin@example.com"}"#);
    assert_eq!(i.verify(&parts.join(".")).unwrap_err().0, "bad signature");
    assert!(i.mint(&mint("a@b", 1, Some("k9"))).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

async fn get_identity(http: &reqwest::Client, base: &str, token: Option<&str>) -> (u16, Value) {
    let mut rb = http.get(format!("{base}{}", fake_access::GET_IDENTITY));
    if let Some(t) = token {
        rb = rb.header("cookie", format!("theme=dark; {}={t}", fake_access::COOKIE));
    }
    let res = rb.send().await.unwrap();
    (res.status().as_u16(), res.json().await.unwrap())
}

#[tokio::test]
async fn the_routes_over_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tmp();
    let i = issuer(&dir);
    // a harness CA and a server certificate for the issuer's name
    let p = |n: &str| dir.join(n).to_str().unwrap().to_owned();
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        &p("ca.key"),
        "-out",
        &p("ca.crt"),
        "-days",
        "1",
        "-subj",
        "/CN=harness CA",
    ]);
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        &p("srv.key"),
        "-out",
        &p("srv.csr"),
        "-subj",
        "/CN=localhost",
    ]);
    std::fs::write(dir.join("ext"), "subjectAltName=DNS:localhost\n").unwrap();
    openssl(&[
        "x509",
        "-req",
        "-in",
        &p("srv.csr"),
        "-CA",
        &p("ca.crt"),
        "-CAkey",
        &p("ca.key"),
        "-CAcreateserial",
        "-out",
        &p("srv.crt"),
        "-days",
        "1",
        "-extfile",
        &p("ext"),
    ]);
    // openssl writes a PKCS#8 key for -newkey with -nodes
    let cert = std::fs::read_to_string(dir.join("srv.crt")).unwrap();
    let key = std::fs::read_to_string(dir.join("srv.key")).unwrap();
    let ca = std::fs::read(dir.join("ca.crt")).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = i.router();
    tokio::spawn(async move { fake_access::serve_tls(listener, router, &cert, &key).await.unwrap() });
    let http = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(&ca).unwrap()])
        .build()
        .unwrap();
    let base = format!("https://localhost:{port}");

    let jwks: Value = http
        .get(format!("{base}{}", fake_access::CERTS))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let minted: Value = http
        .post(format!("{base}{}", fake_access::MINT))
        .json(&json!({"email": "staff@example.org", "name": "Chan Tai Man", "aud": "aud-tag", "expSeconds": 600, "kid": "k2"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = minted["token"].as_str().unwrap();
    verify_with_jwks(&jwks, token);
    let (status, id) = get_identity(&http, &base, Some(token)).await;
    assert_eq!(status, 200);
    assert_eq!(
        id,
        json!({"email": "staff@example.org", "name": "Chan Tai Man", "given_name": "Chan"})
    );
    assert_eq!(get_identity(&http, &base, None).await.0, 401);
    let expired = i.mint(&mint("staff@example.org", -1, None)).unwrap();
    assert_eq!(get_identity(&http, &base, Some(&expired)).await.0, 401);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A verifier that caches the JWKS and refetches it once on an unknown kid, as pigeon's will.
struct CachingVerifier {
    http: reqwest::Client,
    url: String,
    jwks: Value,
    fetches: u32,
}

impl CachingVerifier {
    async fn new(http: reqwest::Client, base: &str) -> Self {
        let mut v = Self {
            http,
            url: format!("{base}{}", fake_access::CERTS),
            jwks: json!({"keys": []}),
            fetches: 0,
        };
        v.refetch().await;
        v
    }

    async fn refetch(&mut self) {
        self.jwks = self.http.get(&self.url).send().await.unwrap().json().await.unwrap();
        self.fetches += 1;
    }

    async fn verify(&mut self, token: &str) -> Option<Value> {
        if let Some(c) = try_verify_with_jwks(&self.jwks, token) {
            return Some(c);
        }
        self.refetch().await;
        try_verify_with_jwks(&self.jwks, token)
    }
}

async fn set_published(http: &reqwest::Client, base: &str, kid: &str, published: bool) -> Value {
    let r = http
        .post(format!("{base}{}", fake_access::KEYS))
        .json(&json!({"kid": kid, "published": published}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

fn kids(jwks: &Value) -> Vec<String> {
    jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["kid"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn rotation_a_new_kid_verifies_only_after_it_is_published_and_a_retired_kid_is_refused() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tmp();
    let k1 = Key::from_pem("k1", &rsa_key(&dir, "k1.pem")).unwrap();
    let k2 = Key::from_pem("k2", &rsa_key(&dir, "k2.pem")).unwrap().unpublished();
    let i = Issuer::new("access.aviary-e2e.svc.cluster.local", vec![k1, k2]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = i.router();
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let http = reqwest::Client::new();

    let mut verifier = CachingVerifier::new(http.clone(), &base).await;
    assert_eq!(kids(&verifier.jwks), ["k1"]);
    let old = i.mint(&mint("staff@example.org", 600, None)).unwrap();
    let new = i.mint(&mint("staff@example.org", 600, Some("k2"))).unwrap();
    assert!(verifier.verify(&old).await.is_some());
    assert_eq!(verifier.fetches, 1, "a known kid needs no refetch");

    // k2 signs but is not published: the verifier refetches, still does not find it, refuses; so does get-identity
    assert!(verifier.verify(&new).await.is_none());
    assert_eq!(verifier.fetches, 2);
    assert_eq!(get_identity(&http, &base, Some(&new)).await.0, 401);

    // publish k2: the next unknown-kid refetch finds it
    assert_eq!(kids(&set_published(&http, &base, "k2", true).await), ["k1", "k2"]);
    assert_eq!(verifier.verify(&new).await.unwrap()["email"], "staff@example.org");
    assert_eq!(verifier.fetches, 3);
    assert_eq!(get_identity(&http, &base, Some(&new)).await.0, 200);

    // retire k1: gone from the JWKS, and its tokens are refused
    assert_eq!(kids(&set_published(&http, &base, "k1", false).await), ["k2"]);
    assert_eq!(get_identity(&http, &base, Some(&old)).await.0, 401);
    let mut fresh = CachingVerifier::new(http.clone(), &base).await;
    assert!(fresh.verify(&old).await.is_none());
    // the default signer is now the first published key
    let next = i.mint(&mint("staff@example.org", 600, None)).unwrap();
    assert!(fresh.verify(&next).await.is_some());
    let r = http
        .post(format!("{base}{}", fake_access::KEYS))
        .json(&json!({"kid": "k9", "published": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}
