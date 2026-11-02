//! `aviary-conformance gateway` (platform 8.2): a sandbox gateway checked from the outside, through its proxy and
//! its in-cluster API, against the suite's own fakes ([`crate::fakes`]).
//!
//! Phases: a warm-up until a valid token is admitted; everything that needs hermit up, concurrently (the slow
//! `tools` call keeps running); hermit's outage; then the drain, which ends the replica (`--sigterm`).

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gateway_api::credential::proxy_authorization;
use gateway_api::models::Models;
use gateway_api::ops::OpsSummary;
use gateway_api::{Claims, headers};
use hermit_api::token::MasterKey;
use hyper::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::fakes::{CASE, Fakes, Live, Outage, STREAM_MS, Seen, stream_events};
use crate::proxy::{self, Conn, empty, full};
use crate::report::{Outcome, Problem, Report};
use crate::{ensure, fail};

/// The tokens of `--tokens tokens.json`: what the gateway presents to each fake, and what its in-cluster API
/// accepts.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    /// gateway → magpie pair token.
    pub gateway_to_magpie: String,
    /// gateway → hermit pair token.
    pub gateway_to_hermit: String,
    /// gateway → tools pair token.
    pub gateway_to_tools: String,
    /// The model upstream's credential (`x-api-key` or `Authorization: Bearer`).
    pub gateway_to_model: String,
    /// pigeon's bearer on the in-cluster models route.
    pub pigeon_to_gateway: String,
    /// hermit's bearer on the in-cluster ops summary.
    pub hermit_to_gateway: String,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens(..)")
    }
}

impl Tokens {
    #[must_use]
    pub fn expected(&self) -> crate::fakes::Expected {
        crate::fakes::Expected {
            magpie: self.gateway_to_magpie.clone(),
            hermit: self.gateway_to_hermit.clone(),
            tools: self.gateway_to_tools.clone(),
            model: self.gateway_to_model.clone(),
        }
    }
}

/// How long the slow checks take. [`Durations::spec`] is the contract's; a shorter set is for the suite's own
/// tests and proves nothing about an implementation.
#[derive(Clone, Copy, Debug)]
pub struct Durations {
    /// The streamed model reply (8.2: 30 s unbroken).
    pub stream: Duration,
    /// magpie's long-poll (25 s).
    pub long_poll: Duration,
    /// How long the fake tools takes to answer (170 s).
    pub tools_delay: Duration,
    /// hermit's outage, in three equal parts: refused, timing out, 503 (3 min).
    pub outage: Duration,
    /// The model stream open during the drain (60 s).
    pub drain_stream: Duration,
    /// The bound on revocation (30 s), plus the suite's slack.
    pub revocation: Duration,
}

impl Durations {
    #[must_use]
    pub fn spec() -> Self {
        Self {
            stream: Duration::from_secs(30),
            long_poll: Duration::from_secs(25),
            tools_delay: Duration::from_secs(170),
            outage: Duration::from_secs(180),
            drain_stream: Duration::from_secs(60),
            revocation: Duration::from_secs(33),
        }
    }

    /// Short runs for the suite's own tests (the outage still outlasts the 30 s liveness cache).
    #[must_use]
    pub fn quick() -> Self {
        Self {
            stream: Duration::from_secs(4),
            long_poll: Duration::from_secs(3),
            tools_delay: Duration::from_secs(3),
            outage: Duration::from_secs(36),
            drain_stream: Duration::from_secs(6),
            revocation: Duration::from_secs(33),
        }
    }
}

/// Sends SIGTERM to the replica under test.
pub type Sigterm = Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// The suite's inputs.
pub struct Options {
    /// The proxy (`--proxy`).
    pub proxy: SocketAddr,
    /// The in-cluster API's base URL (`--api`).
    pub api: String,
    /// The address the gateway sees the suite's connections come from (`--sandbox-ip`).
    pub sandbox_ip: Ipv4Addr,
    pub tokens: Tokens,
    /// The key hermit signs sandbox tokens with (`--signing-key`, `kid:base64url(32 bytes)`).
    pub signing_key: MasterKey,
    /// `--self-host`: the gateway's own addresses.
    pub self_hosts: Vec<String>,
    pub sigterm: Option<Sigterm>,
    pub durations: Durations,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("proxy", &self.proxy)
            .field("api", &self.api)
            .field("sandbox_ip", &self.sandbox_ip)
            .field("self_hosts", &self.self_hosts)
            .field("sigterm", &self.sigterm.is_some())
            .field("durations", &self.durations)
            .finish_non_exhaustive()
    }
}

/// The public half of the signing key, as the fake hermit publishes it.
///
/// # Errors
/// When the key does not derive.
pub fn public_key(key: &MasterKey) -> Result<gateway_api::TokenKey, String> {
    Ok(key.signing_key().map_err(|e| e.to_string())?.public_key())
}

struct Suite {
    o: Options,
    fakes: Fakes,
    started: Instant,
    http: reqwest::Client,
}

/// One sandbox the suite made up: its claims and its proxy credential.
struct Sbx {
    claims: Claims,
    credential: String,
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Suite {
    fn claims(&self, live: Option<Live>) -> Claims {
        let c = Claims {
            sandbox_id: Uuid::now_v7(),
            conversation_id: Uuid::now_v7(),
            source: self.o.sandbox_ip,
            expires_at: unix_now() + 3600,
            // mixed case: the gateway must send it lowercased
            person: "Staff.Conformance@Example.ORG".into(),
        };
        if let Some(l) = live {
            self.fakes
                .set_live(c.sandbox_id, c.conversation_id, &c.person.to_lowercase(), l);
        }
        c
    }

    fn sign(key: &MasterKey, c: &Claims) -> Result<String, String> {
        key.signing_key()
            .map_err(|e| e.to_string())?
            .sign(c)
            .map_err(|e| e.to_string())
    }

    /// A sandbox hermit says is live.
    fn sandbox(&self) -> Result<Sbx, String> {
        self.sandbox_as(Live::Live)
    }

    fn sandbox_as(&self, live: Live) -> Result<Sbx, String> {
        let claims = self.claims(Some(live));
        let token = Self::sign(&self.o.signing_key, &claims)?;
        Ok(Sbx {
            credential: proxy_authorization(&token),
            claims,
        })
    }

    async fn conn(&self) -> Result<Conn, String> {
        Conn::open(self.o.proxy).await
    }

    /// One request on a new connection: status, headers and the whole body.
    async fn once(
        &self,
        method: &str,
        url: &str,
        credential: Option<&str>,
        hdrs: &[(&str, &str)],
        body: proxy::Body,
        timeout: Duration,
    ) -> Result<(StatusCode, hyper::HeaderMap, Vec<u8>), String> {
        let mut c = self.conn().await?;
        let fut = async {
            let res = c.send(proxy::request(method, url, credential, hdrs, body)).await?;
            let (parts, body) = res.into_parts();
            let read = proxy::read_body(body, timeout).await;
            if let Some(e) = read.error {
                return Err(format!("{method} {url}: body {e}"));
            }
            Ok((parts.status, parts.headers, read.bytes))
        };
        tokio::time::timeout(timeout + Duration::from_secs(5), fut)
            .await
            .map_err(|_| format!("{method} {url}: no answer in {timeout:?}"))?
    }

    async fn get(&self, url: &str, sbx: &Sbx, case: &str) -> Result<(StatusCode, Vec<u8>), String> {
        let (s, _, b) = self
            .once(
                "GET",
                url,
                Some(&sbx.credential),
                &[(CASE, case)],
                empty(),
                Duration::from_secs(30),
            )
            .await?;
        Ok((s, b))
    }

    fn the_one(&self, case: &str) -> Result<Seen, String> {
        let seen = self.fakes.seen(case);
        match seen.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(format!("{case}: the request did not reach the fake")),
            many => Err(format!(
                "{case}: {} requests reached the fakes, expected one",
                many.len()
            )),
        }
    }
}

fn code_of(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("code")?
        .as_str()
        .map(str::to_owned)
}

fn expect_refused(status: StatusCode, body: &[u8], want: StatusCode, what: &str) -> Outcome {
    ensure!(
        status == want,
        "{what}: got {status} {}, want {want}",
        String::from_utf8_lossy(body)
    );
    if want == StatusCode::FORBIDDEN {
        ensure!(
            code_of(body).as_deref() == Some(gateway_api::codes::NOT_ALLOWED),
            "{what}: 403 without code not_allowed: {}",
            String::from_utf8_lossy(body)
        );
    }
    Ok(())
}

/// Runs the gateway suite, recording into `report`.
pub async fn run(o: Options, fakes: Fakes, report: &Report) {
    // boxed: the suite's state machine is large
    Box::pin(run_suite(o, fakes, report)).await;
}

async fn run_suite(o: Options, fakes: Fakes, report: &Report) {
    let s = Arc::new(Suite {
        o,
        fakes,
        started: Instant::now(),
        http: reqwest::Client::builder()
            .tls_backend_preconfigured(aviary_core::http::tls_config())
            .no_proxy()
            .build()
            .unwrap_or_default(),
    });

    if let Err(e) = warm_up(&s).await {
        report.record("warm-up", Err(Problem::Fail(e)), s.started.elapsed());
        return;
    }

    // the slow tools call starts first and ends after the outage
    let tools = {
        let s = s.clone();
        let selected = report.selected("route.tools");
        tokio::spawn(async move {
            let t = Instant::now();
            let r = if selected { route_tools(&s).await } else { Ok(()) };
            (r, t.elapsed())
        })
    };

    // phase A: everything that needs hermit up
    tokio::join!(
        report.check("credential.missing", credential_missing(&s)),
        report.check("credential.not-sbx", credential_not_sbx(&s)),
        report.check("credential.bad-signature", credential_bad_signature(&s)),
        report.check("credential.expired", credential_expired(&s)),
        report.check("credential.wrong-source", credential_wrong_source(&s)),
        report.check("credential.unknown-kid", credential_unknown_kid(&s)),
        report.check("credential.new-kid", credential_new_kid(&s)),
        report.check("liveness.revoked", liveness_revoked(&s)),
        report.check("liveness.unknown-sandbox", liveness_unknown(&s)),
        report.check("route.magpie", route_magpie(&s)),
        report.check("route.magpie-request-id", route_request_id(&s)),
        report.check("route.magpie-not-sandbox", route_magpie_not_sandbox(&s)),
        report.check("route.hermit", route_hermit(&s)),
        report.check("route.model-stream", route_model_stream(&s)),
        report.check("route.model-messages", route_model_messages(&s)),
        report.check("route.models", route_models(&s)),
        report.check("transport.long-poll", long_poll(&s)),
        report.check("limits.64mib", limit_64mib(&s)),
        report.check("limits.64mib-plus-one", limit_over(&s)),
        report.check("limits.upload-cap", upload_cap(&s)),
        report.check("self-host", self_host(&s)),
        report.check("internet.denied", internet(&s)),
        report.check("api.models", api_models(&s)),
        report.check("api.ops-summary", api_summary(&s)),
    );

    // phase B: hermit down
    report.check("liveness.hermit-outage", hermit_outage(&s)).await;

    if report.selected("route.tools") {
        match tools.await {
            Ok((r, t)) => report.record("route.tools", r, t),
            Err(e) => report.record("route.tools", Err(Problem::Fail(e.to_string())), Duration::ZERO),
        }
    }

    // phase C: the drain ends the replica
    report.check("drain", drain(&s)).await;
}

async fn warm_up(s: &Suite) -> Result<(), String> {
    let sbx = s.sandbox()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = String::new();
    while Instant::now() < deadline {
        match s.get("http://magpie/v1/sandbox/warm-up", &sbx, "warm-up").await {
            Ok((StatusCode::OK, _)) => return Ok(()),
            Ok((st, b)) => last = format!("{st} {}", String::from_utf8_lossy(&b)),
            Err(e) => last = e,
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(format!(
        "the gateway did not admit a valid token within 60 s (last: {last})"
    ))
}

async fn credential_missing(s: &Suite) -> Outcome {
    let (st, h, b) = s
        .once(
            "GET",
            "http://magpie/v1/sandbox/x",
            None,
            &[(CASE, "credential.missing")],
            empty(),
            Duration::from_secs(10),
        )
        .await?;
    ensure!(
        st == StatusCode::PROXY_AUTHENTICATION_REQUIRED,
        "no credential: got {st} {}",
        String::from_utf8_lossy(&b)
    );
    let pa = h
        .get(hyper::header::PROXY_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    ensure!(
        pa.to_ascii_lowercase().starts_with("basic"),
        "407 without Proxy-Authenticate: Basic (got {pa:?})"
    );
    ensure!(
        s.fakes.seen("credential.missing").is_empty(),
        "forwarded without a credential"
    );
    Ok(())
}

async fn credential_not_sbx(s: &Suite) -> Outcome {
    // a Basic credential whose user is not `sbx`
    let c = s.claims(Some(Live::Live));
    let token = Suite::sign(&s.o.signing_key, &c)?;
    let cred = format!(
        "Basic {}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("root:{token}"))
    );
    let (st, _, b) = s
        .once(
            "GET",
            "http://magpie/v1/sandbox/x",
            Some(&cred),
            &[(CASE, "credential.not-sbx")],
            empty(),
            Duration::from_secs(10),
        )
        .await?;
    ensure!(
        st == StatusCode::FORBIDDEN || st == StatusCode::PROXY_AUTHENTICATION_REQUIRED,
        "user root: got {st} {}",
        String::from_utf8_lossy(&b)
    );
    ensure!(s.fakes.seen("credential.not-sbx").is_empty(), "forwarded");
    Ok(())
}

async fn refused_with(s: &Suite, case: &str, token: &str) -> Outcome {
    let (st, _, b) = s
        .once(
            "GET",
            "http://magpie/v1/sandbox/x",
            Some(&proxy_authorization(token)),
            &[(CASE, case)],
            empty(),
            Duration::from_secs(20),
        )
        .await?;
    expect_refused(st, &b, StatusCode::FORBIDDEN, case)?;
    ensure!(s.fakes.seen(case).is_empty(), "{case}: forwarded");
    Ok(())
}

async fn credential_bad_signature(s: &Suite) -> Outcome {
    let c = s.claims(Some(Live::Live));
    let token = Suite::sign(&s.o.signing_key, &c)?;
    // flip one character of the signature (base64url: keep it in the alphabet)
    let mut chars: Vec<char> = token.chars().collect();
    let i = chars.len() - 3;
    chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
    let bad: String = chars.into_iter().collect();
    refused_with(s, "credential.bad-signature", &bad).await
}

async fn credential_expired(s: &Suite) -> Outcome {
    let mut c = s.claims(Some(Live::Live));
    c.expires_at = unix_now().saturating_sub(10);
    refused_with(s, "credential.expired", &Suite::sign(&s.o.signing_key, &c)?).await
}

async fn credential_wrong_source(s: &Suite) -> Outcome {
    let mut c = s.claims(Some(Live::Live));
    c.source = if s.o.sandbox_ip == Ipv4Addr::new(192, 0, 2, 99) {
        Ipv4Addr::new(192, 0, 2, 98)
    } else {
        Ipv4Addr::new(192, 0, 2, 99)
    };
    refused_with(s, "credential.wrong-source", &Suite::sign(&s.o.signing_key, &c)?).await
}

fn other_key(kid: u32, fill: u8) -> MasterKey {
    MasterKey::new(kid, [fill; 32])
}

async fn credential_unknown_kid(s: &Suite) -> Outcome {
    let c = s.claims(Some(Live::Live));
    let token = Suite::sign(&other_key(4_000_000, 0x5a), &c)?;
    refused_with(s, "credential.unknown-kid", &token).await
}

async fn credential_new_kid(s: &Suite) -> Outcome {
    // a key published after the gateway's last fetch: the first token naming it makes the gateway refetch (at most
    // every 10 s, so wait that long past the start)
    let wait = Duration::from_secs(12).saturating_sub(s.started.elapsed());
    tokio::time::sleep(wait).await;
    let key = other_key(s.o.signing_key.kid.wrapping_add(1).max(2), 0x3c);
    s.fakes.publish_key(public_key(&key)?);
    let c = s.claims(Some(Live::Live));
    let token = Suite::sign(&key, &c)?;
    let before = s.fakes.key_fetches();
    let (st, _, b) = s
        .once(
            "GET",
            "http://magpie/v1/sandbox/x",
            Some(&proxy_authorization(&token)),
            &[(CASE, "credential.new-kid")],
            empty(),
            Duration::from_secs(20),
        )
        .await?;
    ensure!(
        st == StatusCode::OK,
        "a token of a newly published kid: got {st} {} (key set fetched {} time(s) since)",
        String::from_utf8_lossy(&b),
        s.fakes.key_fetches() - before
    );
    Ok(())
}

async fn liveness_revoked(s: &Suite) -> Outcome {
    let case = "liveness.revoked";
    let sbx = s.sandbox()?;
    let (st, b) = s.get("http://magpie/v1/sandbox/x", &sbx, case).await?;
    ensure!(
        st == StatusCode::OK,
        "live sandbox: got {st} {}",
        String::from_utf8_lossy(&b)
    );
    // let the gateway see hermit's live answer before the change
    let t = Instant::now();
    while s.fakes.liveness_lookups(sbx.claims.sandbox_id) == 0 && t.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    s.fakes.flip(sbx.claims.sandbox_id, Live::NotLive);
    let changed = Instant::now();
    loop {
        let (st, b) = s.get("http://magpie/v1/sandbox/x", &sbx, case).await?;
        if st == StatusCode::FORBIDDEN {
            expect_refused(st, &b, StatusCode::FORBIDDEN, case)?;
            break;
        }
        ensure!(st == StatusCode::OK, "before revocation: got {st}");
        if changed.elapsed() > s.o.durations.revocation {
            fail!(
                "still admitted {:?} after hermit answered live: false",
                changed.elapsed()
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let took = changed.elapsed();
    let (st, _) = s.get("http://magpie/v1/sandbox/x", &sbx, case).await?;
    ensure!(st == StatusCode::FORBIDDEN, "admitted again after the refusal: {st}");
    ensure!(took <= s.o.durations.revocation, "revocation took {took:?}");
    Ok(())
}

async fn liveness_unknown(s: &Suite) -> Outcome {
    let case = "liveness.unknown-sandbox";
    let sbx = s.sandbox_as(Live::Unknown)?;
    let t = Instant::now();
    loop {
        let (st, b) = s.get("http://magpie/v1/sandbox/x", &sbx, case).await?;
        if st == StatusCode::FORBIDDEN {
            return expect_refused(st, &b, StatusCode::FORBIDDEN, case);
        }
        ensure!(
            t.elapsed() < Duration::from_secs(10),
            "a sandbox hermit answers 404 for is still admitted after {:?}",
            t.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn check_identity(seen: &Seen, sbx: &Sbx, bearer: &str) -> Outcome {
    let want = [
        ("aviary-sandbox-id", sbx.claims.sandbox_id.to_string()),
        ("aviary-sandbox-conversation", sbx.claims.conversation_id.to_string()),
        ("aviary-sandbox-person", sbx.claims.person.trim().to_lowercase()),
    ];
    for (h, v) in &want {
        let got: Vec<&str> = seen
            .headers
            .get_all(*h)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        ensure!(got == [v.as_str()], "{h}: got {got:?}, want [{v:?}]");
    }
    let auth: Vec<&str> = seen
        .headers
        .get_all("authorization")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    ensure!(
        auth == [format!("Bearer {bearer}").as_str()],
        "authorization is not the gateway's pair bearer alone ({} value(s))",
        auth.len()
    );
    ensure!(
        seen.header("proxy-authorization").is_none(),
        "proxy-authorization forwarded"
    );
    ensure!(seen.header("x-api-key").is_none(), "the client's x-api-key forwarded");
    Ok(())
}

const FORGED: &[(&str, &str)] = &[
    ("authorization", "Bearer sandbox-sent-authorization"),
    ("x-api-key", "sandbox-sent-key"),
    ("aviary-person", "forged@example.org"),
    ("aviary-admin", "forged@example.org"),
    ("aviary-viewer", "forged@example.org"),
    ("aviary-sandbox-id", "00000000-0000-7000-8000-000000000000"),
    ("aviary-sandbox-person", "forged@example.org"),
    ("aviary-lease", "00000000-0000-4000-8000-000000000000"),
];

async fn route_magpie(s: &Suite) -> Outcome {
    let case = "route.magpie";
    let sbx = s.sandbox()?;
    let rid = Uuid::now_v7().to_string();
    let mut hdrs: Vec<(&str, &str)> = vec![
        (CASE, case),
        ("aviary-request", rid.as_str()),
        ("aviary-protocol", "magpie-sandbox=1"),
    ];
    hdrs.extend_from_slice(FORGED);
    let (st, _, b) = s
        .once(
            "GET",
            "http://magpie/v1/sandbox/x?a=1",
            Some(&sbx.credential),
            &hdrs,
            empty(),
            Duration::from_secs(20),
        )
        .await?;
    ensure!(st == StatusCode::OK, "got {st} {}", String::from_utf8_lossy(&b));
    let seen = s.the_one(case)?;
    ensure!(
        seen.path_and_query == "/v1/sandbox/x?a=1",
        "path {}",
        seen.path_and_query
    );
    check_identity(&seen, &sbx, &s.o.tokens.gateway_to_magpie)?;
    for h in ["aviary-person", "aviary-admin", "aviary-viewer", "aviary-lease"] {
        ensure!(seen.header(h).is_none(), "the client's {h} reached magpie");
    }
    ensure!(
        seen.header("aviary-request") == Some(rid.as_str()),
        "a valid v7 Aviary-Request was not kept: {:?}",
        seen.header("aviary-request")
    );
    ensure!(
        seen.header("aviary-protocol") == Some("magpie-sandbox=1"),
        "the client's Aviary-Protocol did not reach magpie: {:?}",
        seen.header("aviary-protocol")
    );
    Ok(())
}

async fn route_request_id(s: &Suite) -> Outcome {
    let case = "route.magpie-request-id";
    let sbx = s.sandbox()?;
    let (st, _, _) = s
        .once(
            "GET",
            "http://magpie/v1/sandbox/x",
            Some(&sbx.credential),
            &[(CASE, case), ("aviary-request", "not-a-v7-id")],
            empty(),
            Duration::from_secs(20),
        )
        .await?;
    ensure!(st == StatusCode::OK, "got {st}");
    let seen = s.the_one(case)?;
    let got = seen.header("aviary-request").unwrap_or_default();
    ensure!(
        headers::keep_request_id(got),
        "an invalid Aviary-Request must be replaced with a new v7 id; magpie got {got:?}"
    );
    Ok(())
}

async fn route_magpie_not_sandbox(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    for (i, url) in [
        "http://magpie/v1/conversations",
        "http://magpie/v1/sandbox/../conversations",
        "http://magpie/v1/sandbox/%2e%2e/conversations",
    ]
    .iter()
    .enumerate()
    {
        let case = format!("route.magpie-not-sandbox.{i}");
        let (st, b) = s.get(url, &sbx, &case).await?;
        expect_refused(st, &b, StatusCode::FORBIDDEN, url)?;
        ensure!(s.fakes.seen(&case).is_empty(), "{url} was forwarded");
    }
    Ok(())
}

async fn route_hermit(s: &Suite) -> Outcome {
    let case = "route.hermit";
    let sbx = s.sandbox()?;
    let mut hdrs: Vec<(&str, &str)> = vec![(CASE, case), ("aviary-protocol", "hermit-sandbox=1")];
    hdrs.extend_from_slice(FORGED);
    let (st, _, b) = s
        .once(
            "GET",
            "http://hermit/v1/sandbox/self",
            Some(&sbx.credential),
            &hdrs,
            empty(),
            Duration::from_secs(20),
        )
        .await?;
    ensure!(st == StatusCode::OK, "got {st} {}", String::from_utf8_lossy(&b));
    let seen = s.the_one(case)?;
    ensure!(
        seen.path_and_query == "/v1/sandbox/self",
        "path {}",
        seen.path_and_query
    );
    check_identity(&seen, &sbx, &s.o.tokens.gateway_to_hermit)?;
    ensure!(
        seen.header("aviary-person").is_none(),
        "the client's aviary-person reached hermit"
    );
    let case = "route.hermit-not-sandbox";
    let (st, b) = s.get("http://hermit/v1/people", &sbx, case).await?;
    expect_refused(st, &b, StatusCode::FORBIDDEN, "http://hermit/v1/people")?;
    ensure!(s.fakes.seen(case).is_empty(), "http://hermit/v1/people was forwarded");
    Ok(())
}

fn model_credential_ok(seen: &Seen, want: &str) -> Outcome {
    let key = seen.header("x-api-key");
    let auth = seen.header("authorization");
    ensure!(
        key == Some(want) || auth == Some(format!("Bearer {want}").as_str()),
        "the model upstream did not get the gateway's credential"
    );
    for v in [key, auth].into_iter().flatten() {
        ensure!(
            !v.contains("sandbox-sent"),
            "the sandbox's credential reached the model upstream"
        );
    }
    Ok(())
}

/// Opens a model stream on `conn` and reads it to the end; checks the first-byte relay and that every event came.
async fn model_stream(s: &Suite, conn: &mut Conn, sbx: &Sbx, case: &str, length: Duration) -> Outcome {
    let ms = u64::try_from(length.as_millis()).unwrap_or(u64::MAX).to_string();
    let body = json!({"model": "conformance", "max_tokens": 64, "stream": true,
        "messages": [{"role": "user", "content": "conformance"}]});
    let req = proxy::request(
        "POST",
        "http://model/v1/messages",
        Some(&sbx.credential),
        &[
            (CASE, case),
            (STREAM_MS, ms.as_str()),
            ("content-type", "application/json"),
            ("anthropic-version", "2023-06-01"),
            ("x-api-key", "sandbox-sent-key"),
            ("authorization", "Bearer sandbox-sent-authorization"),
        ],
        full(serde_json::to_vec(&body).unwrap_or_default()),
    );
    let res = conn.send(req).await?;
    let status = res.status();
    let ctype = res
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let read = proxy::read_body(res.into_body(), length + Duration::from_secs(30)).await;
    ensure!(
        status == StatusCode::OK,
        "status {status}: {}",
        String::from_utf8_lossy(&read.bytes)
    );
    ensure!(ctype.starts_with("text/event-stream"), "content-type {ctype:?}");
    ensure!(
        read.complete,
        "the stream was cut after {} bytes: {}",
        read.bytes.len(),
        read.error.unwrap_or_default()
    );
    let want: Vec<u8> = stream_events(u64::try_from(length.as_millis()).unwrap_or(0)).concat();
    ensure!(
        read.bytes == want,
        "the stream was not relayed unchanged ({} bytes, want {})",
        read.bytes.len(),
        want.len()
    );
    let (Some(up), Some(down)) = (s.fakes.first_byte(case), read.first_byte) else {
        fail!("no first byte recorded");
    };
    let lag = down.saturating_duration_since(up);
    ensure!(
        lag <= gateway_api::limits::FIRST_BYTE,
        "first byte relayed {lag:?} after the upstream's"
    );
    let seen = s.the_one(case)?;
    model_credential_ok(&seen, &s.o.tokens.gateway_to_model)?;
    Ok(())
}

async fn route_model_stream(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    let mut conn = s.conn().await?;
    model_stream(s, &mut conn, &sbx, "route.model-stream", s.o.durations.stream).await
}

async fn route_model_messages(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    for (path, case) in [
        ("/v1/messages", "route.model-messages"),
        ("/v1/messages/count_tokens", "route.model-count-tokens"),
    ] {
        let body = json!({"model": "conformance", "max_tokens": 16,
            "messages": [{"role": "user", "content": "conformance"}]});
        let (st, _, b) = s
            .once(
                "POST",
                &format!("http://model{path}"),
                Some(&sbx.credential),
                &[
                    (CASE, case),
                    ("content-type", "application/json"),
                    ("x-api-key", "sandbox-sent-key"),
                ],
                full(serde_json::to_vec(&body).unwrap_or_default()),
                Duration::from_secs(20),
            )
            .await?;
        ensure!(st == StatusCode::OK, "{path}: {st} {}", String::from_utf8_lossy(&b));
        let v: Value = serde_json::from_slice(&b).map_err(|e| format!("{path}: {e}"))?;
        if path.ends_with("count_tokens") {
            ensure!(v["input_tokens"] == 7, "{path}: not relayed unchanged: {v}");
        } else {
            ensure!(v["id"] == "msg_conformance", "{path}: not relayed unchanged: {v}");
        }
        model_credential_ok(&s.the_one(case)?, &s.o.tokens.gateway_to_model)?;
    }
    let (st, b) = s.get("http://model/v1/models", &sbx, "route.model-other").await?;
    expect_refused(st, &b, StatusCode::FORBIDDEN, "GET http://model/v1/models")?;
    Ok(())
}

fn models_shape(b: &[u8], what: &str) -> Outcome {
    let m: Models = serde_json::from_slice(b).map_err(|e| format!("{what}: not the R18 shape: {e}"))?;
    ensure!(
        m.models.iter().any(|x| x.id == m.default),
        "{what}: default {:?} is not in models",
        m.default
    );
    let v: Value = serde_json::from_slice(b).map_err(|e| e.to_string())?;
    for k in ["default", "small", "title", "contextTokens", "models"] {
        ensure!(v.get(k).is_some(), "{what}: no {k}");
    }
    Ok(())
}

async fn route_models(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    let (st, b) = s.get("http://model/v1/aviary/models", &sbx, "route.models").await?;
    ensure!(st == StatusCode::OK, "{st} {}", String::from_utf8_lossy(&b));
    models_shape(&b, "http://model/v1/aviary/models")
}

async fn route_tools(s: &Suite) -> Outcome {
    let case = "route.tools";
    let sbx = s.sandbox()?;
    let delay = s.o.durations.tools_delay;
    let ms = delay.as_millis();
    let t = Instant::now();
    let (st, _, b) = s
        .once(
            "POST",
            &format!("http://tools/mcp?delayMs={ms}"),
            Some(&sbx.credential),
            &[
                (CASE, case),
                ("content-type", "application/json"),
                ("accept", "application/json, text/event-stream"),
                ("authorization", "Bearer sandbox-sent-authorization"),
            ],
            full(&br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#[..]),
            delay + Duration::from_secs(30),
        )
        .await?;
    ensure!(
        st == StatusCode::OK,
        "a tools answer after {delay:?}: got {st} {} after {:?}",
        String::from_utf8_lossy(&b),
        t.elapsed()
    );
    let seen = s.the_one(case)?;
    let auth = seen.header("authorization").unwrap_or_default();
    ensure!(
        auth == format!("Bearer {}", s.o.tokens.gateway_to_tools),
        "tools did not get the gateway's pair bearer"
    );
    ensure!(
        seen.header("aviary-sandbox-person") == Some(sbx.claims.person.to_lowercase().as_str()),
        "aviary-sandbox-person: {:?}",
        seen.header("aviary-sandbox-person")
    );
    Ok(())
}

async fn long_poll(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    let ms = s.o.durations.long_poll.as_millis();
    let t = Instant::now();
    let (st, _, b) = s
        .once(
            "GET",
            &format!("http://magpie/v1/sandbox/poll?ms={ms}"),
            Some(&sbx.credential),
            &[(CASE, "transport.long-poll")],
            empty(),
            s.o.durations.long_poll + Duration::from_secs(20),
        )
        .await?;
    ensure!(
        st == StatusCode::OK,
        "a {ms} ms long-poll: {st} {} after {:?}",
        String::from_utf8_lossy(&b),
        t.elapsed()
    );
    Ok(())
}

const MIB: usize = 1024 * 1024;

async fn put_bytes(
    s: &Suite,
    conn: &mut Conn,
    sbx: &Sbx,
    case: &str,
    n: usize,
) -> Result<(StatusCode, Vec<u8>), String> {
    let len = n.to_string();
    let req = proxy::request(
        "PUT",
        "http://magpie/v1/sandbox/blob",
        Some(&sbx.credential),
        &[
            (CASE, case),
            ("content-type", "application/octet-stream"),
            ("content-length", len.as_str()),
        ],
        full(vec![0x61u8; n]),
    );
    let _ = s;
    let res = conn.send(req).await?;
    let st = res.status();
    let read = proxy::read_body(res.into_body(), Duration::from_secs(60)).await;
    Ok((st, read.bytes))
}

async fn limit_64mib(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    let mut conn = s.conn().await?;
    let (st, b) = put_bytes(s, &mut conn, &sbx, "limits.64mib", 64 * MIB).await?;
    ensure!(st == StatusCode::OK, "64 MiB PUT: {st} {}", String::from_utf8_lossy(&b));
    let v: Value = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
    ensure!(v["bytes"] == 64 * MIB, "magpie received {} bytes", v["bytes"]);
    Ok(())
}

async fn limit_over(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    let mut conn = s.conn().await?;
    let (st, b) = put_bytes(s, &mut conn, &sbx, "limits.64mib-plus-one", 64 * MIB + 1).await?;
    ensure!(
        st == StatusCode::PAYLOAD_TOO_LARGE,
        "64 MiB + 1: {st} {}",
        String::from_utf8_lossy(&b)
    );
    ensure!(
        code_of(&b).as_deref() == Some(gateway_api::codes::TOO_LARGE),
        "413 without code too_large"
    );
    Ok(())
}

async fn upload_cap(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    let mut conn = s.conn().await?;
    for i in 0..40 {
        let (st, b) = put_bytes(s, &mut conn, &sbx, "limits.upload-cap", MIB).await?;
        ensure!(
            st == StatusCode::OK,
            "PUT {} of 40 (1 MiB each, one connection): {st} {}",
            i + 1,
            String::from_utf8_lossy(&b)
        );
        ensure!(
            !conn.is_closed() || i == 39,
            "the connection was closed after PUT {}",
            i + 1
        );
    }
    let seen = s.fakes.seen("limits.upload-cap");
    ensure!(seen.len() == 40, "magpie received {} of 40", seen.len());
    ensure!(seen.iter().all(|x| x.body_bytes == MIB as u64), "a body arrived short");
    Ok(())
}

async fn self_host(s: &Suite) -> Outcome {
    if s.o.self_hosts.is_empty() {
        return Err(Problem::Skip("no --self-host given".into()));
    }
    let sbx = s.sandbox()?;
    for h in &s.o.self_hosts {
        let host = if h.contains(':') && !h.starts_with('[') && h.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{h}]")
        } else {
            h.clone()
        };
        let url = format!("http://{host}/conformance/self-host-probe");
        let (st, b) = s.get(&url, &sbx, "self-host").await?;
        ensure!(
            !(st == StatusCode::FORBIDDEN && code_of(&b).as_deref() == Some(gateway_api::codes::NOT_ALLOWED)),
            "{url}: refused like an internet host ({st})"
        );
        ensure!(
            st != StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            "{url}: 407 with a valid credential"
        );
        ensure!(
            s.fakes.seen_path("/conformance/self-host-probe").is_empty(),
            "{url} was forwarded to an upstream"
        );
    }
    Ok(())
}

async fn internet(s: &Suite) -> Outcome {
    let sbx = s.sandbox()?;
    let url = "http://unlisted.invalid/conformance/internet-probe";
    let (st, _) = s.get(url, &sbx, "internet.denied").await?;
    ensure!(!st.is_success(), "{url}: {st}");
    let mut c = s.conn().await?;
    let res = c
        .send(proxy::connect("unlisted.invalid:443", Some(&sbx.credential)))
        .await?;
    ensure!(
        !res.status().is_success(),
        "CONNECT unlisted.invalid:443: {}",
        res.status()
    );
    ensure!(
        s.fakes.seen_path("/conformance/internet-probe").is_empty(),
        "an internet request reached an upstream"
    );
    Ok(())
}

async fn api_get(s: &Suite, path: &str, bearer: Option<&str>) -> Result<(StatusCode, Vec<u8>), String> {
    let mut rb = s
        .http
        .get(format!("{}{path}", s.o.api.trim_end_matches('/')))
        .timeout(Duration::from_secs(15));
    if let Some(b) = bearer {
        rb = rb.bearer_auth(b);
    }
    let res = rb.send().await.map_err(|e| format!("{path}: {e}"))?;
    let st = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let b = res.bytes().await.map_err(|e| e.to_string())?.to_vec();
    Ok((st, b))
}

async fn api_models(s: &Suite) -> Outcome {
    let (st, b) = api_get(s, gateway_api::models::MODELS, Some(&s.o.tokens.pigeon_to_gateway)).await?;
    ensure!(
        st == StatusCode::OK,
        "with pigeon's bearer: {st} {}",
        String::from_utf8_lossy(&b)
    );
    models_shape(&b, "in-cluster /v1/aviary/models")?;
    let (st, _) = api_get(s, gateway_api::models::MODELS, None).await?;
    ensure!(st == StatusCode::UNAUTHORIZED, "without a bearer: {st}");
    Ok(())
}

async fn api_summary(s: &Suite) -> Outcome {
    let path = format!("{}?tz=Asia%2FHong_Kong&recentMinutes=15", gateway_api::ops::OPS_SUMMARY);
    let (st, b) = api_get(s, &path, Some(&s.o.tokens.hermit_to_gateway)).await?;
    ensure!(
        st == StatusCode::OK,
        "with hermit's bearer: {st} {}",
        String::from_utf8_lossy(&b)
    );
    let sum: OpsSummary = serde_json::from_slice(&b).map_err(|e| format!("not platform 15.3's shape: {e}"))?;
    let owners = sum
        .budget
        .by_user
        .iter()
        .map(|u| Some(u.owner.as_str()))
        .chain(sum.egress.recent.iter().map(|r| r.owner.as_deref()))
        .chain(sum.recent.list.iter().map(|r| r.owner.as_deref()))
        .flatten();
    for o in owners {
        ensure!(
            o.contains('@') && o == o.trim().to_lowercase(),
            "an owner is not a trimmed, lowercased email: {o:?}"
        );
    }
    let (st, _) = api_get(s, &path, None).await?;
    ensure!(st == StatusCode::UNAUTHORIZED, "without a bearer: {st}");
    Ok(())
}

async fn hermit_outage(s: &Suite) -> Outcome {
    let case = "liveness.hermit-outage";
    // a sandbox the gateway has a live answer for, and one it has never seen
    let seen_before = s.sandbox()?;
    let (st, _) = s.get("http://magpie/v1/sandbox/x", &seen_before, case).await?;
    ensure!(st == StatusCode::OK, "before the outage: {st}");
    let t = Instant::now();
    while s.fakes.liveness_lookups(seen_before.claims.sandbox_id) == 0 && t.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ensure!(
        s.fakes.liveness_lookups(seen_before.claims.sandbox_id) > 0,
        "the gateway never asked hermit about a sandbox it admitted"
    );
    let never_seen = s.sandbox()?;
    let third = s.o.durations.outage / 3;
    let mut refusals = Vec::new();
    let mut requests = 0u32;
    for phase in [Outage::Refuse, Outage::Hang, Outage::Unavailable] {
        s.fakes.set_outage(phase);
        let end = Instant::now() + third;
        while Instant::now() < end {
            for (which, sbx) in [("seen", &seen_before), ("never seen", &never_seen)] {
                requests += 1;
                match s.get("http://magpie/v1/sandbox/x", sbx, case).await {
                    Ok((StatusCode::OK, _)) => {}
                    Ok((st, b)) => refusals.push(format!(
                        "{phase:?} +{:?}: the {which} token got {st} {}",
                        s.started.elapsed(),
                        String::from_utf8_lossy(&b)
                    )),
                    Err(e) => refusals.push(format!("{phase:?}: the {which} token: {e}")),
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    s.fakes.set_outage(Outage::Up);
    ensure!(
        refusals.is_empty(),
        "{} of {requests} requests failed during hermit's outage, first: {}",
        refusals.len(),
        refusals.first().cloned().unwrap_or_default()
    );
    Ok(())
}

async fn drain(s: &Suite) -> Outcome {
    let Some(sigterm) = &s.o.sigterm else {
        return Err(Problem::Skip("no --sigterm given".into()));
    };
    let sbx = s.sandbox()?;
    // an idle kept-alive connection
    let mut idle = s.conn().await?;
    let res = idle
        .send(proxy::request(
            "GET",
            "http://magpie/v1/sandbox/x",
            Some(&sbx.credential),
            &[(CASE, "drain.idle")],
            empty(),
        ))
        .await?;
    ensure!(
        res.status() == StatusCode::OK,
        "idle connection's request: {}",
        res.status()
    );
    let _ = proxy::read_body(res.into_body(), Duration::from_secs(10)).await;

    // a busy one: a long-poll in flight
    let poll_ms = s.o.durations.long_poll.as_millis();
    let mut busy = s.conn().await?;
    let busy_req = proxy::request(
        "GET",
        &format!("http://magpie/v1/sandbox/poll?ms={poll_ms}"),
        Some(&sbx.credential),
        &[(CASE, "drain.busy")],
        empty(),
    );
    let long_poll = tokio::spawn(async move {
        let res = busy.send(busy_req).await?;
        let close = res
            .headers()
            .get(hyper::header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("close"));
        let st = res.status();
        let read = proxy::read_body(res.into_body(), Duration::from_secs(120)).await;
        Ok::<_, String>((st, close, read.complete))
    });

    // a model stream
    let mut stream_conn = s.conn().await?;
    let stream_sbx = s.sandbox()?;
    let length = s.o.durations.drain_stream;
    let suite_ptr: &Suite = s;
    let stream = model_stream(suite_ptr, &mut stream_conn, &stream_sbx, "drain.stream", length);

    let after = async {
        // both requests are in flight before the signal
        let t = Instant::now();
        while (s.fakes.seen("drain.busy").is_empty() || s.fakes.first_byte("drain.stream").is_none())
            && t.elapsed() < Duration::from_secs(10)
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        sigterm().await.map_err(|e| format!("--sigterm: {e}"))?;
        // the idle connection is closed, and a new connection is refused
        let idle_closed = idle.wait_closed(Duration::from_secs(10)).await;
        let t = Instant::now();
        let mut refused = false;
        while t.elapsed() < Duration::from_secs(10) {
            if tokio::net::TcpStream::connect(s.o.proxy).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Ok::<_, String>((idle_closed, refused))
    };
    let (streamed, after) = tokio::join!(stream, after);
    let (idle_closed, refused) = after?;
    let (poll_status, poll_close, poll_complete) = long_poll.await.map_err(|e| e.to_string())??;
    let mut problems = Vec::new();
    if let Err(Problem::Fail(m)) = streamed {
        problems.push(format!("the model stream: {m}"));
    }
    if poll_status != StatusCode::OK || !poll_complete {
        problems.push(format!("the long-poll: {poll_status}, complete {poll_complete}"));
    }
    if !poll_close {
        problems.push("the long-poll's answer on the busy connection lacked Connection: close".into());
    }
    if !idle_closed {
        problems.push("the idle kept-alive connection was not closed".into());
    }
    if !refused {
        problems.push("a new connection to the draining replica was accepted".into());
    }
    ensure!(problems.is_empty(), "{}", problems.join("; "));
    Ok(())
}
