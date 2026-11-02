//! The harness of pigeon's HTTP tests: a real pigeon (its router behind its own listener) in front of fake-hermit,
//! fake-magpie, a model stub and two fake Access teams, with a small app bundle on disk.
#![allow(dead_code, clippy::unwrap_used, clippy::missing_panics_doc, clippy::unused_self)] // tests

pub mod model;
pub mod ws;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use aviary_core::auth::Caller;
use aviary_fakes::hermit::{HermitConfig, Running as HermitRunning};
use aviary_fakes::magpie::Running as MagpieRunning;
use aviary_fakes::{FakeHermit, FakeMagpie};
use axum::Router;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use fake_access::{Issuer, Key, MintRequest};
use hermit_api::deployment::AccessApp;
use pigeon::{App, Config, KeyList, Secrets, Tuning, Upstreams};
use serde_json::{Value, json};

pub const TEAM1: &str = "one.example.org";
pub const TEAM2: &str = "two.example.org";
pub const AUD1: &str = "aud-one";
pub const AUD2: &str = "aud-two";
pub const STAFF: &str = "staff@example.org";
pub const STAFF2: &str = "other@example.org";
pub const ADMIN: &str = "admin@example.com";
pub const ADMIN2: &str = "admin2@example.com";
pub const OUTSIDER: &str = "someone@elsewhere.example";
pub const VIEW_KEY: [u8; 32] = [3; 32];
pub const BLOB_KEY: [u8; 32] = [5; 32];
pub const PRODUCT: &str = "Example AI";
pub const HASHED: &str = "/_expo/static/js/web/entry-0123456789abcdef0123.js";

pub const INDEX: &str = "<!doctype html><html lang=\"zh-HK\"><head><meta charset=\"utf-8\"><title>App</title>\
<meta name=\"apple-mobile-web-app-title\" content=\"App\"><script>window.__early=1</script></head><body>\
<textarea id=\"early-box\"></textarea><script src=\"/_expo/static/js/web/entry-0123456789abcdef0123.js\"></script>\
</body></html>";

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pigeon-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn openssl_key(dir: &Path, name: &str) -> String {
    let p = dir.join(name);
    let out = Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&p)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::read_to_string(p).unwrap()
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel.trim_start_matches('/'));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

/// The test bundle.
pub fn bundle() -> PathBuf {
    let d = tmp("app");
    write(&d, "index.html", INDEX.as_bytes());
    write(&d, HASHED, b"console.log('entry')");
    write(&d, &format!("{HASHED}.br"), b"BROTLI");
    write(&d, &format!("{HASHED}.gz"), b"GZIP");
    write(&d, "sw.js", b"self.registration.unregister()");
    write(&d, "logo.png", b"\x89PNG");
    write(&d, "manifest.json", b"{}");
    d
}

/// A key pair as PEM, plus the issuer key made from it.
pub struct Pem {
    pub kid: String,
    pub pem: String,
}

/// Signs a JWT with a PEM key (for tokens fake-access will not mint: service tokens, foreign issuers).
pub fn sign_jwt(pem: &Pem, claims: &Value) -> String {
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use rustls::pki_types::pem::PemObject as _;
    let der = PrivatePkcs8KeyDer::from_pem_slice(pem.pem.as_bytes()).unwrap();
    let pair = ring::signature::RsaKeyPair::from_pkcs8(der.secret_pkcs8_der()).unwrap();
    let header = json!({"alg": "RS256", "kid": pem.kid, "typ": "JWT"});
    let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
    let mut sig = vec![0; pair.public().modulus_len()];
    pair.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        input.as_bytes(),
        &mut sig,
    )
    .unwrap();
    format!("{input}.{}", B64.encode(sig))
}

pub fn now() -> i64 {
    pigeon::identity::now_secs()
}

pub struct Team {
    pub issuer: Arc<Issuer>,
    pub domain: &'static str,
    pub aud: &'static str,
    pub pems: Vec<Pem>,
    pub jwks_hits: Arc<AtomicUsize>,
    /// The certs endpoint answers (else 503).
    pub certs_up: Arc<AtomicBool>,
    /// The certs endpoint waits this long before answering, ms.
    pub certs_delay_ms: Arc<AtomicU64>,
    pub served: aviary_fakes::Served,
}

impl Team {
    async fn start(dir: &Path, domain: &'static str, aud: &'static str, prefix: &str) -> Self {
        let pems: Vec<Pem> = ["a", "b"]
            .iter()
            .map(|k| {
                let kid = format!("{prefix}-{k}");
                Pem {
                    pem: openssl_key(dir, &format!("{kid}.pem")),
                    kid,
                }
            })
            .collect();
        // the second key is loaded but not published: rotation publishes it
        let keys = vec![
            Key::from_pem(&pems[0].kid, &pems[0].pem).unwrap(),
            Key::from_pem(&pems[1].kid, &pems[1].pem).unwrap().unpublished(),
        ];
        let issuer = Issuer::with_audience(domain, keys, Some(aud)).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let certs_up = Arc::new(AtomicBool::new(true));
        let certs_delay_ms = Arc::new(AtomicU64::new(0));
        let (h, up, delay) = (hits.clone(), certs_up.clone(), certs_delay_ms.clone());
        let router = issuer
            .router()
            .layer(axum::middleware::from_fn(move |req: Request, next: Next| {
                let (h, up, delay) = (h.clone(), up.clone(), delay.clone());
                async move {
                    if req.uri().path() == fake_access::CERTS {
                        h.fetch_add(1, Ordering::SeqCst);
                        let ms = delay.load(Ordering::SeqCst);
                        if ms > 0 {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                        }
                        if !up.load(Ordering::SeqCst) {
                            return Response::builder().status(503).body(axum::body::Body::empty()).unwrap();
                        }
                    }
                    next.run(req).await
                }
            }));
        let served = aviary_fakes::serve(router).await.unwrap();
        Self {
            issuer,
            domain,
            aud,
            pems,
            jwks_hits: hits,
            certs_up,
            certs_delay_ms,
            served,
        }
    }

    /// A token for `email` from this team's first key.
    pub fn token(&self, email: &str) -> String {
        self.mint(email, 3600, None, None)
    }

    pub fn mint(&self, email: &str, exp: i64, nbf: Option<i64>, kid: Option<&str>) -> String {
        self.issuer
            .mint(&MintRequest {
                email: email.into(),
                name: Some("Tai Man Chan".into()),
                aud: self.aud.into(),
                exp_seconds: exp,
                nbf_seconds: nbf,
                kid: kid.map(str::to_owned),
            })
            .unwrap()
    }

    /// Claims as Access writes them, for [`sign_jwt`].
    pub fn claims(&self, email: Option<&str>) -> Value {
        let mut c = json!({
            "aud": [self.aud], "exp": now() + 3600, "iat": now(), "nbf": now(),
            "iss": format!("https://{}", self.domain), "type": "app",
        });
        if let Some(e) = email {
            c["email"] = json!(e);
        }
        c
    }
}

pub struct Harness {
    pub url: String,
    pub http: reqwest::Client,
    pub hermit: HermitRunning,
    pub magpie: MagpieRunning,
    pub teams: Vec<Team>,
    pub app: App,
    pub models_hits: Arc<AtomicUsize>,
    pub models_up: Arc<std::sync::atomic::AtomicBool>,
    /// The model list stub answers only after 30 s.
    pub models_hang: Arc<std::sync::atomic::AtomicBool>,
    pub listener: Option<pigeon::server::Listener>,
    pub readiness: aviary_core::ops::Readiness,
    pub dir: PathBuf,
}

/// How [`Harness::start_opts`] starts.
pub struct Opts {
    pub tuning: Tuning,
    /// Teams (by index) whose certs endpoint fails from the start.
    pub certs_down: Vec<usize>,
    /// How many of the two teams the deployment facts name at first.
    pub teams_in_facts: usize,
    /// Wait until pigeon is ready (else return at once).
    pub wait_ready: bool,
    /// Another magpie (its base URL) instead of fake-magpie: the socket tests' model.
    pub magpie: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            tuning: tuning(),
            certs_down: Vec::new(),
            teams_in_facts: 2,
            wait_ready: true,
            magpie: None,
        }
    }
}

/// Short timings for tests (the specification's are in [`Tuning::default`]).
pub fn tuning() -> Tuning {
    Tuning {
        facts_every: Duration::from_millis(200),
        start_retry: Duration::from_millis(50),
        jwks_cooldown: Duration::from_millis(600),
        people_fresh: Duration::from_millis(400),
        people_stale: Duration::from_millis(1500),
        model_ttl: Duration::from_millis(200),
        warm_every: Duration::from_millis(500),
        transfer_idle: Duration::from_secs(10),
        ..Tuning::default()
    }
}

const MODELS_JSON: &str = r#"{"default":"vendor/model-a","small":"vendor/model-s","title":"vendor/model-s",
"contextTokens":200000,"models":[{"id":"vendor/model-a","label":"Model A","reasoning":null},
{"id":"vendor/model-b","label":"","reasoning":null}]}"#;

fn model_stub(
    hits: Arc<AtomicUsize>,
    up: Arc<std::sync::atomic::AtomicBool>,
    hang: Arc<std::sync::atomic::AtomicBool>,
    token: String,
) -> Router {
    Router::new().route(
        gateway_api::models::MODELS,
        axum::routing::get(move |h: axum::http::HeaderMap| {
            let hits = hits.clone();
            let up = up.clone();
            let hang = hang.clone();
            let token = token.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                if hang.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
                let ok = up.load(Ordering::SeqCst) && auth == format!("Bearer {token}");
                let (status, body) = if ok { (200, MODELS_JSON) } else { (503, "{}") };
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }
        }),
    )
}

impl Harness {
    pub async fn start() -> Self {
        Self::start_with(tuning()).await
    }

    pub async fn start_with(tuning: Tuning) -> Self {
        Self::start_opts(Opts {
            tuning,
            ..Opts::default()
        })
        .await
    }

    pub async fn start_opts(opts: Opts) -> Self {
        let Opts {
            tuning,
            certs_down,
            teams_in_facts,
            wait_ready,
            magpie: magpie_url,
        } = opts;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tmp("keys");
        let teams = vec![
            Team::start(&dir, TEAM1, AUD1, "t1").await,
            Team::start(&dir, TEAM2, AUD2, "t2").await,
        ];
        for i in certs_down {
            teams[i].certs_up.store(false, Ordering::SeqCst);
        }
        let mut config = HermitConfig {
            users: vec!["@example.org".into(), ADMIN.into(), ADMIN2.into()],
            admins: vec![ADMIN.into(), ADMIN2.into()],
            ..HermitConfig::default()
        };
        config.deployment.product.name = PRODUCT.into();
        config.deployment.product.locale = "zh-HK".into();
        config.deployment.sign_in.access = teams
            .iter()
            .take(teams_in_facts)
            .map(|t| AccessApp {
                team_domain: t.domain.into(),
                audience: t.aud.into(),
            })
            .collect();
        config.deployment.sign_in.app_scheme = Some("dev.example.app".into());
        let hermit = FakeHermit::with_config(config).serve().await.unwrap();
        let magpie = FakeMagpie::new().serve().await.unwrap();
        let models_hits = Arc::new(AtomicUsize::new(0));
        let models_up = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let models_hang = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let model_token = "model-token".to_owned();
        let model = aviary_fakes::serve(model_stub(
            models_hits.clone(),
            models_up.clone(),
            models_hang.clone(),
            model_token.clone(),
        ))
        .await
        .unwrap();
        let origins: HashMap<String, String> = teams.iter().map(|t| (t.domain.to_owned(), t.served.url())).collect();
        let upstreams = Upstreams {
            magpie: magpie_url.unwrap_or_else(|| magpie.url()),
            hermit: hermit.url(),
            model: model.url(),
            access: Arc::new(move |team| {
                origins
                    .get(team)
                    .cloned()
                    .unwrap_or_else(|| "http://127.0.0.1:9".into())
            }),
        };
        // the model stub lives as long as the harness
        std::mem::forget(model);
        let secrets = Secrets {
            token_magpie: magpie.fake.token(Caller::Pigeon).unwrap(),
            token_hermit: hermit.fake.token(Caller::Pigeon).unwrap(),
            token_model: model_token,
            view_as_keys: KeyList(vec![VIEW_KEY]),
            blob_keys: KeyList(vec![BLOB_KEY]),
        };
        let app_dir = bundle();
        let mut registry = prometheus_client::registry::Registry::default();
        let app = App::new(
            Config {
                build: aviary_core::version::Build::new("pigeon"),
                upstreams,
                secrets,
                app_dir,
                tuning,
            },
            &mut registry,
        )
        .unwrap();
        let readiness = aviary_core::ops::Readiness::detached();
        app.start(&readiness);
        readiness.started();
        for _ in 0..200 {
            if !wait_ready || app.identities().facts().is_some() && readiness.why().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if wait_ready {
            assert!(readiness.why().is_empty(), "not ready: {:?}", readiness.why());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let served = pigeon::server::serve(listener, app.router());
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self {
            url,
            http,
            hermit,
            magpie,
            teams,
            app,
            models_hits,
            models_up,
            models_hang,
            listener: Some(served),
            readiness,
            dir,
        }
    }

    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(format!("{}{path}", self.url))
    }

    pub fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.post(format!("{}{path}", self.url))
    }

    /// A request carrying `token` as the Access assertion header.
    pub fn as_token(&self, rb: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        rb.header("cf-access-jwt-assertion", token)
    }

    pub fn token(&self, email: &str) -> String {
        self.teams[0].token(email)
    }

    /// GET `path` as `email` (team one).
    pub async fn get_as(&self, path: &str, email: &str) -> reqwest::Response {
        self.as_token(self.get(path), &self.token(email)).send().await.unwrap()
    }

    /// A view-as token signed with the test key.
    pub fn view_token(&self, admin: &str, person: &str, exp: i64) -> String {
        pigeon::viewas::sign(
            &KeyList(vec![VIEW_KEY]),
            &pigeon::viewas::Claims {
                a: admin.into(),
                u: person.into(),
                s: uuid::Uuid::new_v4().to_string(),
                e: exp,
            },
        )
    }

    /// Changes the deployment facts and waits for pigeon to read them.
    pub async fn set_facts(&self, f: impl FnOnce(&mut hermit_api::deployment::Deployment)) {
        let mut want = None;
        self.hermit.fake.configure(|c| {
            f(&mut c.deployment);
            want = Some(c.deployment.clone());
        });
        for _ in 0..200 {
            if self.app.identities().facts().as_deref() == want.as_ref() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("facts not refreshed");
    }
}

/// The JSON body of an answer.
pub async fn body_json(r: reqwest::Response) -> Value {
    let t = r.text().await.unwrap();
    serde_json::from_str(&t).unwrap_or_else(|e| panic!("not JSON ({e}): {t}"))
}

/// The app's error code of an `/api` answer.
pub async fn code(r: reqwest::Response) -> (u16, String) {
    let s = r.status().as_u16();
    let v = body_json(r).await;
    (s, v["error"]["code"].as_str().unwrap_or("").to_owned())
}
