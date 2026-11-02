//! `aviary-conformance hermit` (hermit spec 24.1, platform 8.1): a live hermit, any implementation, with a fake
//! fabric, checked through its routes with the callers' pair tokens.
//!
//! The suite needs from the implementation only its routes and, through [`Hooks`], what no route gives: a sandbox
//! token (`--token-command`), and in the suite's own tests a pending release and a feed retention cut. A check
//! that needs a hook it does not have is skipped and says so.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hermit_api::people::{People, Person, Role};
use hermit_api::sandbox::{Sandbox, SandboxEvents, SandboxState};
use hermit_api::sandbox_surface::{ActivityReply, ActivityReplyState, SandboxSelf, SecretKeys};
use hermit_api::token::{KeySet, TokenKeys, TokenLiveness, VerifyError};
use hermit_api::workspaces::{Entries, Workspace};
use hermit_api::{CORE, SANDBOX, routes};
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::report::{Outcome, Problem, Report};
use crate::{ensure, fail};

/// The pair tokens of `--tokens tokens.json`, one per caller hermit knows (each optional; a check needing a missing
/// one is skipped).
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub pigeon: Option<String>,
    pub magpie: Option<String>,
    pub gateway: Option<String>,
    pub tools: Option<String>,
    pub monitor: Option<String>,
    pub admin: Option<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens(..)")
    }
}

/// The token hook: (sandbox id, conversation id) to the sandbox's token.
pub type TokenHook = Box<dyn Fn(Uuid, Uuid) -> Option<String> + Send + Sync>;

/// What a check may ask of the implementation beyond its routes.
#[derive(Default)]
pub struct Hooks {
    /// The sandbox token of a sandbox (`sandbox id`, `conversation id`).
    pub token: Option<TokenHook>,
    /// Make hermit want a soft release of this conversation's sandbox, so the next activity reply carries it.
    pub pending_release: Option<Box<dyn Fn(Uuid) + Send + Sync>>,
    /// Drop every feed event older than the given seq (retention), so an older cursor answers 410.
    pub expire_feed: Option<Box<dyn Fn(u64) + Send + Sync>>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field("token", &self.token.is_some())
            .field("pending_release", &self.pending_release.is_some())
            .field("expire_feed", &self.expire_feed.is_some())
            .finish()
    }
}

/// What the implementation claims (`--capabilities`); it publishes no flags itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    pub pause: bool,
    pub snapshot: bool,
}

/// The suite's inputs.
#[derive(Debug)]
pub struct Options {
    pub target: String,
    pub tokens: Tokens,
    pub capabilities: Capabilities,
    pub hooks: Hooks,
    /// How long a sandbox may take to become ready.
    pub ready_within: Duration,
}

/// Which caller a request speaks as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum As {
    Pigeon,
    Magpie,
    Gateway,
    Tools,
    Nobody,
}

struct Answer {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn code(&self) -> Option<String> {
        self.json().get("code").and_then(Value::as_str).map(str::to_owned)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).chars().take(300).collect()
    }

    fn typed<T: DeserializeOwned>(&self, what: &str) -> Result<T, Problem> {
        serde_json::from_slice(&self.body)
            .map_err(|e| Problem::Fail(format!("{what}: not the contract's shape: {e}: {}", self.text())))
    }

    fn expect(&self, status: u16, code: Option<&str>, what: &str) -> Outcome {
        ensure!(
            self.status.as_u16() == status,
            "{what}: {} {}, want {status}",
            self.status,
            self.text()
        );
        if let Some(c) = code {
            ensure!(
                self.code().as_deref() == Some(c),
                "{what}: code {:?}, want {c}",
                self.code()
            );
        }
        Ok(())
    }
}

struct Suite {
    o: Options,
    http: reqwest::Client,
    second: reqwest::Client,
    /// Keys with `_` seen in JSON answers (platform 3.6), with the route.
    snake: Mutex<Vec<String>>,
}

/// One `Aviary-Sandbox-*` triple.
#[derive(Clone)]
struct Identity {
    id: String,
    conversation: String,
    person: String,
}

impl From<&Sandbox> for Identity {
    fn from(s: &Sandbox) -> Self {
        Self {
            id: s.id.to_string(),
            conversation: s.conversation_id.to_string(),
            person: s.person.clone(),
        }
    }
}

fn snake_keys(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                // the descriptor's `env` is a map of environment variable names (data, not field names)
                if path.rsplit('.').next() == Some("env") {
                    continue;
                }
                if k.contains('_') {
                    out.push(format!("{path}.{k}"));
                }
                snake_keys(v, &format!("{path}.{k}"), out);
            }
        }
        Value::Array(a) => {
            for v in a {
                snake_keys(v, path, out);
            }
        }
        _ => {}
    }
}

impl Suite {
    fn token(&self, who: As) -> Result<Option<&str>, Problem> {
        let t = match who {
            As::Pigeon => &self.o.tokens.pigeon,
            As::Magpie => &self.o.tokens.magpie,
            As::Gateway => &self.o.tokens.gateway,
            As::Tools => &self.o.tokens.tools,
            As::Nobody => return Ok(None),
        };
        t.as_deref()
            .map(Some)
            .ok_or_else(|| Problem::Skip(format!("no {who:?} token in --tokens")))
    }

    #[allow(clippy::too_many_arguments)]
    async fn call_on(
        &self,
        http: &reqwest::Client,
        who: As,
        method: &str,
        path: &str,
        body: Option<&Value>,
        identity: Option<&Identity>,
        extra: &[(&str, &str)],
    ) -> Result<Answer, Problem> {
        let token = self.token(who)?;
        let url = format!("{}{path}", self.o.target.trim_end_matches('/'));
        let m: reqwest::Method = method.parse().map_err(|_| Problem::Fail(format!("method {method}")))?;
        let surface = if path.starts_with(routes::SANDBOX_SURFACE_PREFIX) {
            SANDBOX
        } else {
            CORE
        };
        let mut rb = http
            .request(m, &url)
            .timeout(Duration::from_secs(40))
            .header(hermit_api::headers::PROTOCOL, format!("{}={}", surface.0, surface.1));
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        if let Some(i) = identity {
            rb = rb
                .header(hermit_api::headers::SANDBOX_ID, &i.id)
                .header(hermit_api::headers::SANDBOX_CONVERSATION, &i.conversation)
                .header(hermit_api::headers::SANDBOX_PERSON, &i.person);
        }
        for (k, v) in extra {
            rb = rb.header(*k, *v);
        }
        if let Some(b) = body {
            rb = rb.json(b);
        }
        let res = rb
            .send()
            .await
            .map_err(|e| Problem::Fail(format!("{method} {path}: {e}")))?;
        let status = res.status();
        let headers = res.headers().clone();
        let body = res
            .bytes()
            .await
            .map_err(|e| Problem::Fail(format!("{method} {path}: {e}")))?
            .to_vec();
        if status.is_success()
            && headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("application/json"))
            && let Ok(v) = serde_json::from_slice::<Value>(&body)
        {
            let mut found = Vec::new();
            snake_keys(&v, "", &mut found);
            if !found.is_empty() {
                self.snake
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend(found.into_iter().map(|k| format!("{method} {path}: {k}")));
            }
        }
        Ok(Answer { status, headers, body })
    }

    async fn call(&self, who: As, method: &str, path: &str, body: Option<&Value>) -> Result<Answer, Problem> {
        self.call_on(&self.http, who, method, path, body, None, &[]).await
    }

    async fn catalogue(&self) -> Result<People, Problem> {
        let a = self.call(As::Magpie, "GET", routes::PEOPLE, None).await?;
        a.expect(200, None, "GET /v1/people")?;
        a.typed("GET /v1/people")
    }

    /// An address the catalogue allows: an explicit staff entry, else one at a listed domain, else an admin.
    async fn allowed_people(&self) -> Result<Vec<String>, Problem> {
        let c = self.catalogue().await?;
        let mut out: Vec<String> = c
            .explicit
            .iter()
            .filter(|p| p.role == Role::Staff)
            .map(|p| p.email.clone())
            .collect();
        out.extend(
            c.domains
                .iter()
                .map(|d| format!("conformance.{}@{d}", Uuid::new_v4().simple())),
        );
        out.extend(
            c.explicit
                .iter()
                .filter(|p| p.role == Role::Admin)
                .map(|p| p.email.clone()),
        );
        if out.is_empty() {
            return Err(Problem::Skip("the catalogue allows nobody".into()));
        }
        Ok(out)
    }

    async fn person(&self) -> Result<String, Problem> {
        Ok(self.allowed_people().await?.remove(0))
    }

    async fn put(&self, cid: Uuid, person: &str) -> Result<Answer, Problem> {
        self.call(
            As::Magpie,
            "PUT",
            &routes::fill(routes::SANDBOX, &[&cid.to_string()]),
            Some(&json!({"person": person})),
        )
        .await
    }

    /// Starts a sandbox for a new conversation and waits until it is ready.
    async fn ready_sandbox(&self) -> Result<Sandbox, Problem> {
        let cid = Uuid::now_v7();
        let person = self.person().await?;
        let a = self.put(cid, &person).await?;
        ensure!(
            matches!(a.status.as_u16(), 200 | 202),
            "PUT sandbox: {} {}",
            a.status,
            a.text()
        );
        let s: Sandbox = a.typed("PUT sandbox")?;
        self.wait_state(cid, s.version, |st| st == SandboxState::Ready).await
    }

    /// Long-polls the sandbox of `cid` until `done(state)`.
    async fn wait_state(
        &self,
        cid: Uuid,
        mut version: u64,
        done: impl Fn(SandboxState) -> bool,
    ) -> Result<Sandbox, Problem> {
        let path = routes::fill(routes::SANDBOX, &[&cid.to_string()]);
        let deadline = Instant::now() + self.o.ready_within;
        loop {
            let a = self
                .call(
                    As::Magpie,
                    "GET",
                    &format!("{path}?waitMs=25000&afterVersion={version}"),
                    None,
                )
                .await?;
            a.expect(200, None, "GET sandbox (long-poll)")?;
            let s: Sandbox = a.typed("GET sandbox")?;
            if done(s.state) {
                return Ok(s);
            }
            ensure!(
                Instant::now() < deadline,
                "the sandbox of {cid} is still {:?} after {:?}",
                s.state,
                self.o.ready_within
            );
            if s.version == version {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            version = s.version;
        }
    }

    async fn head_seq(&self) -> Result<u64, Problem> {
        let a = self
            .call(
                As::Magpie,
                "GET",
                &format!("{}?after=latest", routes::SANDBOX_EVENTS),
                None,
            )
            .await?;
        a.expect(200, None, "GET sandbox-events?after=latest")?;
        let f: SandboxEvents = a.typed("sandbox-events")?;
        ensure!(f.events.is_empty(), "after=latest returned events");
        Ok(f.head_seq)
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Runs the hermit suite, recording into `report`.
pub async fn run(o: Options, report: &Report) {
    // boxed: the suite's state machine is large
    Box::pin(run_suite(o, report)).await;
}

async fn run_suite(o: Options, report: &Report) {
    let build = || {
        reqwest::Client::builder()
            .tls_backend_preconfigured(aviary_core::http::tls_config())
            .no_proxy()
            .http1_only()
            .build()
            .unwrap_or_default()
    };
    let s = Arc::new(Suite {
        o,
        http: build(),
        second: build(),
        snake: Mutex::new(Vec::new()),
    });
    let head = s.head_seq().await.ok();
    tokio::join!(
        report.check("auth.missing-token", auth_missing(&s)),
        report.check("auth.wrong-caller", auth_wrong_caller(&s)),
        report.check("auth.protocol", auth_protocol(&s)),
        report.check("deployment", deployment(&s)),
        report.check("people.catalogue", people_catalogue(&s)),
        report.check("people.rules", people_rules(&s)),
        report.check("warm", warm(&s)),
        report.check("sandbox.bad-request", sandbox_bad_request(&s)),
        report.check("sandbox.not-allowed", sandbox_not_allowed(&s)),
        report.check("sandbox.none", sandbox_none(&s)),
        report.check("sandbox.idempotent-put", idempotent_put(&s)),
        report.check("sandbox.long-poll", long_poll(&s)),
        report.check("sandbox.person-mismatch", person_mismatch(&s)),
        report.check("sandbox.capabilities", capabilities(&s)),
        report.check("sandbox-surface", sandbox_surface(&s)),
        report.check("tokens", tokens(&s)),
        report.check("workspaces", workspaces(&s)),
    );
    report.check("release-and-feed", release_and_feed(&s, head)).await;
    report.check("feed.retention", feed_retention(&s)).await;
    let snake = s.snake.lock().unwrap_or_else(PoisonError::into_inner).clone();
    report.record(
        "json.camel-case",
        if snake.is_empty() {
            Ok(())
        } else {
            Err(Problem::Fail(format!("snake_case keys: {}", snake.join(", "))))
        },
        Duration::ZERO,
    );
}

async fn auth_missing(s: &Suite) -> Outcome {
    let a = s.call(As::Nobody, "GET", routes::DEPLOYMENT, None).await?;
    a.expect(401, Some("unauthenticated"), "no bearer")?;
    let www = a
        .headers
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    ensure!(
        www.to_ascii_lowercase().starts_with("bearer"),
        "401 without www-authenticate: Bearer"
    );
    let cid = Uuid::now_v7().to_string();
    let a = s
        .call(
            As::Nobody,
            "PUT",
            &routes::fill(routes::SANDBOX, &[&cid]),
            Some(&json!({"person": "x@y.z"})),
        )
        .await?;
    a.expect(401, Some("unauthenticated"), "PUT sandbox without a bearer")?;
    Ok(())
}

async fn auth_wrong_caller(s: &Suite) -> Outcome {
    let a = s.call(As::Tools, "GET", routes::DEPLOYMENT, None).await?;
    a.expect(403, Some("wrong_caller"), "GET /v1/deployment as tools")?;
    let cid = Uuid::now_v7().to_string();
    let a = s
        .call(
            As::Pigeon,
            "PUT",
            &routes::fill(routes::SANDBOX, &[&cid]),
            Some(&json!({"person": "x@y.z"})),
        )
        .await?;
    a.expect(403, Some("wrong_caller"), "PUT sandbox as pigeon")?;
    let a = s.call(As::Pigeon, "GET", routes::SANDBOX_TOKEN_KEYS, None).await?;
    a.expect(403, Some("wrong_caller"), "GET token keys as pigeon")?;
    Ok(())
}

async fn auth_protocol(s: &Suite) -> Outcome {
    let token = s.token(As::Pigeon)?.unwrap_or_default();
    let url = format!("{}{}", s.o.target.trim_end_matches('/'), routes::DEPLOYMENT);
    let res = s
        .http
        .get(&url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ensure!(
        res.status() == StatusCode::BAD_REQUEST,
        "no Aviary-Protocol: {} (want 400 unsupported_protocol)",
        res.status()
    );
    Ok(())
}

async fn deployment(s: &Suite) -> Outcome {
    let a = s.call(As::Pigeon, "GET", routes::DEPLOYMENT, None).await?;
    a.expect(200, None, "GET /v1/deployment")?;
    let _: hermit_api::deployment::Deployment = a.typed("deployment")?;
    let cc = a
        .headers
        .get(reqwest::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    ensure!(cc.contains("no-store"), "cache-control {cc:?}");
    let a = s.call(As::Magpie, "GET", routes::DEPLOYMENT, None).await?;
    a.expect(200, None, "GET /v1/deployment as magpie")?;
    Ok(())
}

async fn people_catalogue(s: &Suite) -> Outcome {
    let c = s.catalogue().await?;
    ensure!(!c.generation.is_empty(), "generation empty");
    for d in &c.domains {
        ensure!(!d.starts_with('@'), "domain {d:?} still has its @");
    }
    for p in &c.explicit {
        ensure!(
            p.email == p.email.trim().to_lowercase(),
            "explicit {:?} not normalised",
            p.email
        );
    }
    Ok(())
}

async fn lookup(s: &Suite, raw: &str) -> Result<Answer, Problem> {
    s.call(As::Pigeon, "GET", &routes::fill(routes::PERSON, &[raw]), None)
        .await
}

async fn people_rules(s: &Suite) -> Outcome {
    let c = s.catalogue().await?;
    for p in &c.explicit {
        let a = lookup(s, &p.email).await?;
        a.expect(200, None, &p.email)?;
        let got: Person = a.typed("person")?;
        ensure!(
            got.allowed && got.role == Some(p.role),
            "{}: {got:?}, catalogue says {:?}",
            p.email,
            p.role
        );
    }
    if let Some(admin) = c.explicit.iter().find(|p| p.role == Role::Admin) {
        let messy = format!("  {}  ", admin.email.to_uppercase());
        let a = lookup(s, &messy).await?;
        a.expect(200, None, "an admin in upper case with spaces")?;
        let got: Person = a.typed("person")?;
        ensure!(
            got.email == admin.email && got.role == Some(Role::Admin) && got.allowed,
            "case and whitespace: {got:?}"
        );
    }
    for d in &c.domains {
        let email = format!("someone.conformance@{d}");
        let a = lookup(s, &email).await?;
        a.expect(200, None, &email)?;
        let got: Person = a.typed("person")?;
        ensure!(
            got.allowed && got.role == Some(Role::Staff),
            "{email} at a listed domain: {got:?}"
        );
        let sub = format!("someone.conformance@sub.{d}");
        let got: Person = lookup(s, &sub).await?.typed("person")?;
        ensure!(!got.allowed, "{sub}: a subdomain is not the domain: {got:?}");
    }
    let a = lookup(s, "nobody.conformance@unlisted.invalid").await?;
    a.expect(200, None, "an unlisted address")?;
    let got: Person = a.typed("person")?;
    ensure!(!got.allowed && got.role.is_none(), "unlisted: {got:?}");
    for bad in ["not-an-email", "a@b@c", "a b@c.d"] {
        lookup(s, bad).await?.expect(400, Some("bad_email"), bad)?;
    }
    Ok(())
}

async fn warm(s: &Suite) -> Outcome {
    let person = s.person().await?;
    let a = s
        .call(
            As::Pigeon,
            "POST",
            routes::WARM,
            Some(&json!({"person": person, "conversationId": null})),
        )
        .await?;
    a.expect(202, None, "warm")?;
    let r: hermit_api::people::WarmReply = a.typed("warm")?;
    let _ = r.hinted;
    let a = s
        .call(
            As::Pigeon,
            "POST",
            routes::WARM,
            Some(&json!({"person": "nobody.conformance@unlisted.invalid", "conversationId": null})),
        )
        .await?;
    a.expect(403, Some("not_allowed"), "warm for an unlisted person")?;
    Ok(())
}

async fn sandbox_bad_request(s: &Suite) -> Outcome {
    let a = s
        .call(
            As::Magpie,
            "PUT",
            "/v1/conversations/not-a-uuid/sandbox",
            Some(&json!({"person": "x@y.z"})),
        )
        .await?;
    a.expect(400, None, "PUT with a bad uuid")?;
    Ok(())
}

async fn sandbox_not_allowed(s: &Suite) -> Outcome {
    let a = s.put(Uuid::now_v7(), "nobody.conformance@unlisted.invalid").await?;
    a.expect(403, Some("not_allowed"), "PUT for an unlisted person")?;
    Ok(())
}

async fn sandbox_none(s: &Suite) -> Outcome {
    let cid = Uuid::now_v7().to_string();
    let a = s
        .call(As::Magpie, "GET", &routes::fill(routes::SANDBOX, &[&cid]), None)
        .await?;
    a.expect(404, Some("no_sandbox"), "GET sandbox of an unknown conversation")?;
    let rel = s
        .call(
            As::Magpie,
            "POST",
            &routes::fill(routes::SANDBOX_RELEASE, &[&cid]),
            Some(&json!({"reason": "deleted"})),
        )
        .await?;
    rel.expect(200, None, "release with no sandbox")?;
    ensure!(rel.json()["released"] == 0, "release with no sandbox: {}", rel.text());
    Ok(())
}

async fn idempotent_put(s: &Suite) -> Outcome {
    let cid = Uuid::now_v7();
    let person = s.person().await?;
    let path = routes::fill(routes::SANDBOX, &[&cid.to_string()]);
    let body = json!({"person": person});
    let (a, b) = tokio::join!(
        s.call_on(&s.http, As::Magpie, "PUT", &path, Some(&body), None, &[]),
        s.call_on(&s.second, As::Magpie, "PUT", &path, Some(&body), None, &[]),
    );
    let (a, b) = (a?, b?);
    for x in [&a, &b] {
        ensure!(matches!(x.status.as_u16(), 200 | 202), "PUT: {} {}", x.status, x.text());
    }
    let (sa, sb): (Sandbox, Sandbox) = (a.typed("PUT")?, b.typed("PUT")?);
    ensure!(
        sa.id == sb.id,
        "two PUTs at once made two sandboxes: {} and {}",
        sa.id,
        sb.id
    );
    let c: Sandbox = s.put(cid, &person).await?.typed("PUT")?;
    ensure!(c.id == sa.id, "a third PUT made another sandbox");
    ensure!(
        sa.person == person.trim().to_lowercase() && sa.conversation_id == cid,
        "the sandbox's person or conversation: {sa:?}"
    );
    for (st, x) in [(a.status, &sa), (b.status, &sb)] {
        ensure!(
            (st == StatusCode::OK) == (x.state == SandboxState::Ready),
            "status {st} with state {:?}",
            x.state
        );
    }
    Ok(())
}

async fn long_poll(s: &Suite) -> Outcome {
    let cid = Uuid::now_v7();
    let person = s.person().await?;
    let first: Sandbox = s.put(cid, &person).await?.typed("PUT")?;
    let path = routes::fill(routes::SANDBOX, &[&cid.to_string()]);
    let mut observed_timeout = false;
    if first.state == SandboxState::Starting {
        let t = Instant::now();
        let a = s
            .call(
                As::Magpie,
                "GET",
                &format!("{path}?waitMs=1000&afterVersion={}", first.version),
                None,
            )
            .await?;
        a.expect(200, None, "GET waitMs=1000")?;
        let x: Sandbox = a.typed("GET")?;
        if x.version == first.version {
            ensure!(
                t.elapsed() >= Duration::from_millis(900),
                "an unchanged answer came after {:?}, before waitMs",
                t.elapsed()
            );
            ensure!(
                x.state == SandboxState::Starting,
                "same version, other state {:?}",
                x.state
            );
            observed_timeout = true;
        } else {
            ensure!(x.version > first.version, "version went back");
        }
    }
    // a wait that ends on the change, well before waitMs
    let t = Instant::now();
    let ready = s.wait_state(cid, first.version, |st| st == SandboxState::Ready).await?;
    ensure!(ready.ready_at.is_some(), "ready without readyAt");
    if first.state == SandboxState::Starting {
        ensure!(
            t.elapsed() < s.o.ready_within,
            "the long-poll did not wake on the change"
        );
    }
    // a ready sandbox answers at once whatever afterVersion says
    let t = Instant::now();
    let a = s
        .call(
            As::Magpie,
            "GET",
            &format!("{path}?waitMs=5000&afterVersion={}", ready.version + 100),
            None,
        )
        .await?;
    a.expect(200, None, "GET a ready sandbox with waitMs")?;
    ensure!(
        t.elapsed() < Duration::from_secs(3),
        "a ready sandbox was held {:?}",
        t.elapsed()
    );
    if !observed_timeout {
        return Err(Problem::Skip(
            "the sandbox became ready before a timeout could be observed (the wake was checked)".into(),
        ));
    }
    Ok(())
}

async fn person_mismatch(s: &Suite) -> Outcome {
    let people = s.allowed_people().await?;
    let [a, b, ..] = people.as_slice() else {
        return Err(Problem::Skip("the catalogue allows only one address".into()));
    };
    let cid = Uuid::now_v7();
    let first = s.put(cid, a).await?;
    ensure!(matches!(first.status.as_u16(), 200 | 202), "PUT: {}", first.status);
    s.put(cid, b)
        .await?
        .expect(409, Some("person_mismatch"), "PUT as another person")?;
    Ok(())
}

async fn capabilities(s: &Suite) -> Outcome {
    let sbx = s.ready_sandbox().await?;
    let cid = sbx.conversation_id.to_string();
    let call = |route: &'static str| {
        let path = routes::fill(route, &[&cid]);
        async move { s.call(As::Magpie, "POST", &path, None).await }
    };
    if s.o.capabilities.pause {
        let p = call(routes::SANDBOX_PAUSE).await?;
        p.expect(202, None, "pause")?;
        let x: Sandbox = p.typed("pause")?;
        ensure!(x.state == SandboxState::Paused, "pause: state {:?}", x.state);
        let r = call(routes::SANDBOX_RESUME).await?;
        r.expect(202, None, "resume")?;
        let y: Sandbox = r.typed("resume")?;
        ensure!(y.id == x.id, "resume made another sandbox");
    } else {
        call(routes::SANDBOX_PAUSE)
            .await?
            .expect(501, Some("not_supported"), "pause (not claimed)")?;
        call(routes::SANDBOX_RESUME)
            .await?
            .expect(501, Some("not_supported"), "resume (not claimed)")?;
    }
    if s.o.capabilities.snapshot {
        let a = call(routes::SANDBOX_SNAPSHOT).await?;
        a.expect(201, None, "snapshot")?;
        let _: hermit_api::sandbox::SnapshotReply = a.typed("snapshot")?;
    } else {
        call(routes::SANDBOX_SNAPSHOT)
            .await?
            .expect(501, Some("not_supported"), "snapshot (not claimed)")?;
    }
    Ok(())
}

fn report_body(state: &str) -> Value {
    let idle = state == "idle";
    json!({
        "state": state, "turnId": if idle { Value::Null } else { json!(Uuid::now_v7()) }, "tasks": 0,
        "idleSince": if idle { json!(hermit_api::Timestamp::now()) } else { Value::Null },
        "lastOutputAt": null, "bootId": "conformance-boot", "protocol": SANDBOX.1, "version": "conformance",
        "restarts": 0, "rssBytes": 1_048_576, "memory": {"limit": 1_073_741_824u64, "used": 1_048_576}, "events": []
    })
}

#[allow(clippy::too_many_lines)] // one sandbox's surface, checked in order
async fn sandbox_surface(s: &Suite) -> Outcome {
    let sbx = s.ready_sandbox().await?;
    let id = Identity::from(&sbx);
    let surface = |who: As,
                   method: &'static str,
                   route: &'static str,
                   body: Option<Value>,
                   ident: Option<Identity>,
                   extra: Vec<(String, String)>| {
        async move {
            let e: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            s.call_on(&s.http, who, method, route, body.as_ref(), ident.as_ref(), &e)
                .await
        }
    };
    let a = surface(As::Gateway, "GET", routes::SANDBOX_SELF, None, Some(id.clone()), vec![]).await?;
    a.expect(200, None, "GET /v1/sandbox/self")?;
    let me: SandboxSelf = a.typed("SandboxSelf")?;
    ensure!(
        me.sandbox.id == sbx.id && me.sandbox.conversation_id == sbx.conversation_id,
        "the descriptor names another sandbox"
    );
    ensure!(
        me.etag.starts_with('"') && me.etag.ends_with('"') && me.etag.len() == 34,
        "etag {:?}",
        me.etag
    );
    ensure!(
        a.body.len() <= hermit_api::sandbox_surface::SELF_MAX_BYTES,
        "descriptor over 64 KiB"
    );
    let header_etag = a
        .headers
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    ensure!(
        header_etag == me.etag,
        "ETag header {header_etag:?} differs from the body's {:?}",
        me.etag
    );
    let a = surface(
        As::Gateway,
        "GET",
        routes::SANDBOX_SELF,
        None,
        Some(id.clone()),
        vec![("if-none-match".into(), me.etag.clone())],
    )
    .await?;
    a.expect(304, None, "If-None-Match with the etag")?;

    surface(As::Pigeon, "GET", routes::SANDBOX_SELF, None, Some(id.clone()), vec![])
        .await?
        .expect(403, Some("wrong_caller"), "/v1/sandbox/self as pigeon")?;
    surface(As::Nobody, "GET", routes::SANDBOX_SELF, None, Some(id.clone()), vec![])
        .await?
        .expect(401, Some("unauthenticated"), "/v1/sandbox/self without a bearer")?;
    let mut wrong = id.clone();
    wrong.conversation = Uuid::now_v7().to_string();
    surface(As::Gateway, "GET", routes::SANDBOX_SELF, None, Some(wrong), vec![])
        .await?
        .expect(404, Some("no_sandbox"), "/v1/sandbox/self with another conversation")?;
    let mut wrong = id.clone();
    wrong.person = "nobody.conformance@unlisted.invalid".into();
    surface(As::Gateway, "GET", routes::SANDBOX_SELF, None, Some(wrong), vec![])
        .await?
        .expect(404, Some("no_sandbox"), "/v1/sandbox/self with another person")?;

    let a = surface(
        As::Gateway,
        "GET",
        routes::SANDBOX_SECRETS,
        None,
        Some(id.clone()),
        vec![],
    )
    .await?;
    a.expect(200, None, "GET /v1/sandbox/secrets")?;
    let keys: SecretKeys = a.typed("SecretKeys")?;
    let missing = (0..=keys.keys.len())
        .map(|i| format!("CONFORMANCE_NOT_GRANTED_{i}"))
        .find(|k| !keys.keys.contains(k))
        .unwrap_or_default();
    let path = routes::fill(routes::SANDBOX_SECRET, &[&missing]);
    let a = s
        .call_on(&s.http, As::Gateway, "GET", &path, None, Some(&id), &[])
        .await?;
    a.expect(404, Some("not_granted"), "a key that was not granted")?;

    // an idle report: the reply says ready, no release
    let a = surface(
        As::Gateway,
        "POST",
        routes::SANDBOX_ACTIVITY,
        Some(report_body("idle")),
        Some(id.clone()),
        vec![],
    )
    .await?;
    a.expect(200, None, "POST activity (idle)")?;
    let reply: ActivityReply = a.typed("ActivityReply")?;
    ensure!(
        reply.state == ActivityReplyState::Ready,
        "idle report: state {:?}",
        reply.state
    );
    ensure!(
        reply.release.is_none(),
        "idle report on a sandbox with no pending release: {:?}",
        reply.release
    );

    // a busy report: busy shows in the sandbox with a new version
    let a = surface(
        As::Gateway,
        "POST",
        routes::SANDBOX_ACTIVITY,
        Some(report_body("running")),
        Some(id.clone()),
        vec![],
    )
    .await?;
    a.expect(200, None, "POST activity (running)")?;
    let now: Sandbox = s
        .call(
            As::Magpie,
            "GET",
            &routes::fill(routes::SANDBOX, &[&id.conversation]),
            None,
        )
        .await?
        .typed("GET sandbox")?;
    ensure!(
        now.busy && now.version > sbx.version,
        "after a running report: busy {} version {} (was {})",
        now.busy,
        now.version,
        sbx.version
    );

    if let Some(hook) = &s.o.hooks.pending_release {
        hook(sbx.conversation_id);
        let a = surface(
            As::Gateway,
            "POST",
            routes::SANDBOX_ACTIVITY,
            Some(report_body("running")),
            Some(id.clone()),
            vec![],
        )
        .await?;
        a.expect(200, None, "POST activity with a release pending")?;
        let reply: ActivityReply = a.typed("ActivityReply")?;
        let Some(r) = reply.release else {
            fail!("a pending release is not in the activity reply");
        };
        ensure!(!r.force, "the pending soft release came back forced");
    }
    Ok(())
}

async fn tokens(s: &Suite) -> Outcome {
    let a = s.call(As::Gateway, "GET", routes::SANDBOX_TOKEN_KEYS, None).await?;
    a.expect(200, None, "GET token keys")?;
    let keys: TokenKeys = a.typed("TokenKeys")?;
    ensure!(!keys.keys.is_empty(), "no token key published");
    ensure!(
        keys.keys.iter().all(|k| k.alg == "Ed25519"),
        "a key that is not Ed25519"
    );
    let set = KeySet::from_keys(&keys);
    ensure!(!set.is_empty(), "no published key decodes as 32 bytes of base64url");
    if s.o.tokens.tools.is_some() {
        s.call(As::Tools, "GET", routes::SANDBOX_TOKEN_KEYS, None)
            .await?
            .expect(200, None, "token keys as tools")?;
    }

    let sbx = s.ready_sandbox().await?;
    let path = routes::fill(routes::SANDBOX_TOKEN, &[&sbx.id.to_string()]);
    let a = s.call(As::Gateway, "GET", &path, None).await?;
    a.expect(200, None, "liveness of a ready sandbox")?;
    let l: TokenLiveness = a.typed("TokenLiveness")?;
    ensure!(
        l.live && l.sandbox_id == sbx.id && l.conversation_id == sbx.conversation_id && l.person == sbx.person,
        "liveness: {l:?}"
    );
    let unknown = routes::fill(routes::SANDBOX_TOKEN, &[&Uuid::now_v7().to_string()]);
    s.call(As::Gateway, "GET", &unknown, None).await?.expect(
        404,
        Some("unknown_sandbox"),
        "liveness of an unknown id",
    )?;

    let Some(hook) = &s.o.hooks.token else {
        return Err(Problem::Skip(
            "no --token-command: the minted-token check did not run".into(),
        ));
    };
    let Some(token) = hook(sbx.id, sbx.conversation_id) else {
        fail!("the token hook gave no token for {}", sbx.id);
    };
    match set.verify(&token, unix_now(), IpAddr::V4(Ipv4Addr::UNSPECIFIED)) {
        Ok(c) => {
            ensure!(
                c.sandbox_id == sbx.id && c.conversation_id == sbx.conversation_id,
                "the token names another sandbox"
            );
            ensure!(c.person == sbx.person, "the token's person {:?}", c.person);
        }
        // the signature and expiry verified; only the source address (the sandbox's own) differs
        Err(VerifyError::WrongSource) => {}
        Err(e) => fail!("the sandbox's token does not verify with the published keys: {e}"),
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // the release and the feed it produces, in order
async fn release_and_feed(s: &Suite, head: Option<u64>) -> Outcome {
    let Some(head) = head else {
        fail!("GET sandbox-events?after=latest failed at the start");
    };
    let sbx = s.ready_sandbox().await?;
    let cid = sbx.conversation_id.to_string();
    let rel = routes::fill(routes::SANDBOX_RELEASE, &[&cid]);
    let a = s
        .call(As::Magpie, "POST", &rel, Some(&json!({"reason": "deleted"})))
        .await?;
    a.expect(200, None, "release")?;
    let r: hermit_api::sandbox::ReleaseSandboxReply = a.typed("release")?;
    ensure!(r.released <= 1, "released {}", r.released);
    let b = s
        .call(As::Magpie, "POST", &rel, Some(&json!({"reason": "deleted"})))
        .await?;
    b.expect(200, None, "release again")?;
    let gone = s
        .wait_state(sbx.conversation_id, 0, |st| st == SandboxState::Gone)
        .await?;
    ensure!(
        gone.id == sbx.id && gone.release.is_some(),
        "gone without its release: {gone:?}"
    );
    // liveness of a gone sandbox: live false, or 404 once hermit has forgotten it
    let a = s
        .call(
            As::Gateway,
            "GET",
            &routes::fill(routes::SANDBOX_TOKEN, &[&sbx.id.to_string()]),
            None,
        )
        .await?;
    if a.status != StatusCode::NOT_FOUND {
        a.expect(200, None, "liveness of a gone sandbox")?;
        let l: TokenLiveness = a.typed("TokenLiveness")?;
        ensure!(!l.live, "a gone sandbox is still live: {l:?}");
    }

    // the feed: in seq order, this sandbox's changes ending gone, limit honoured, long-poll answers at once
    let mut seqs = Vec::new();
    let mut states = Vec::new();
    let mut after = head;
    loop {
        let a = s
            .call(
                As::Magpie,
                "GET",
                &format!("{}?after={after}&limit=500&waitMs=0", routes::SANDBOX_EVENTS),
                None,
            )
            .await?;
        a.expect(200, None, "the feed")?;
        let f: SandboxEvents = a.typed("SandboxEvents")?;
        if f.events.is_empty() {
            break;
        }
        for e in &f.events {
            seqs.push(e.seq);
            if e.sandbox_id == sbx.id {
                states.push(e.state);
            }
        }
        after = f.events.last().map_or(after, |e| e.seq);
    }
    ensure!(seqs.windows(2).all(|w| w[0] < w[1]), "feed out of order: {seqs:?}");
    ensure!(
        seqs.first().is_none_or(|f| *f > head),
        "an event at or before the cursor"
    );
    ensure!(
        states.last() == Some(&SandboxState::Gone),
        "this sandbox's events {states:?} do not end gone"
    );
    let a = s
        .call(
            As::Magpie,
            "GET",
            &format!("{}?after={head}&limit=1", routes::SANDBOX_EVENTS),
            None,
        )
        .await?;
    let f: SandboxEvents = a.typed("SandboxEvents")?;
    ensure!(f.events.len() == 1, "limit=1 gave {} events", f.events.len());
    // a wait with nothing newer times out empty
    let t = Instant::now();
    let a = s
        .call(
            As::Magpie,
            "GET",
            &format!("{}?after={}&waitMs=1000", routes::SANDBOX_EVENTS, u64::MAX / 2),
            None,
        )
        .await?;
    a.expect(200, None, "the feed after the head")?;
    let f: SandboxEvents = a.typed("SandboxEvents")?;
    ensure!(
        f.events.is_empty() && t.elapsed() >= Duration::from_millis(900),
        "a waitMs=1000 with nothing newer: {} events after {:?}",
        f.events.len(),
        t.elapsed()
    );
    Ok(())
}

async fn feed_retention(s: &Suite) -> Outcome {
    let head = s.head_seq().await?;
    if let Some(hook) = &s.o.hooks.expire_feed {
        ensure!(head >= 2, "too few events to expire");
        hook(head);
        let a = s
            .call(As::Magpie, "GET", &format!("{}?after=0", routes::SANDBOX_EVENTS), None)
            .await?;
        a.expect(410, Some("cursor_expired"), "a cursor older than the retained events")?;
        return Ok(());
    }
    let a = s
        .call(
            As::Magpie,
            "GET",
            &format!("{}?after=0&limit=1", routes::SANDBOX_EVENTS),
            None,
        )
        .await?;
    match a.status.as_u16() {
        410 => a.expect(410, Some("cursor_expired"), "after=0"),
        200 => Err(Problem::Skip(
            "no expired cursor to observe (after=0 is still retained)".into(),
        )),
        _ => Err(Problem::Fail(format!("after=0: {} {}", a.status, a.text()))),
    }
}

async fn workspaces(s: &Suite) -> Outcome {
    let wid = Uuid::now_v7().to_string();
    let p = |route: &str, w: &str| routes::fill(route, &[w]);
    let a = s
        .call(As::Magpie, "PUT", &p(routes::WORKSPACE, &wid), Some(&json!({})))
        .await?;
    a.expect(201, None, "PUT a new workspace")?;
    let w: Workspace = a.typed("Workspace")?;
    ensure!(w.id.to_string() == wid, "PUT answered another workspace");
    s.call(As::Magpie, "PUT", &p(routes::WORKSPACE, &wid), Some(&json!({})))
        .await?
        .expect(200, None, "PUT an existing workspace")?;
    let _: Workspace = s
        .call(As::Magpie, "GET", &p(routes::WORKSPACE, &wid), None)
        .await?
        .typed("Workspace")?;
    let a = s
        .call(As::Magpie, "GET", &p(routes::WORKSPACE_ENTRIES, &wid), None)
        .await?;
    a.expect(200, None, "entries of the root")?;
    let _: Entries = a.typed("Entries")?;
    s.call(
        As::Magpie,
        "GET",
        &format!("{}?path=conformance-missing.txt", p(routes::WORKSPACE_FILES, &wid)),
        None,
    )
    .await?
    .expect(404, Some("no_path"), "a missing file")?;
    s.call(
        As::Magpie,
        "GET",
        &format!("{}?path=..%2Fx", p(routes::WORKSPACE_FILES, &wid)),
        None,
    )
    .await?
    .expect(400, Some("bad_path"), "a path with ..")?;
    let copy = Uuid::now_v7().to_string();
    let a = s
        .call(
            As::Magpie,
            "POST",
            &p(routes::WORKSPACE_CLONE, &copy),
            Some(&json!({"from": wid})),
        )
        .await?;
    a.expect(201, None, "clone")?;
    s.call(
        As::Magpie,
        "POST",
        &p(routes::WORKSPACE_CLONE, &copy),
        Some(&json!({"from": wid})),
    )
    .await?
    .expect(409, Some("exists"), "clone onto an existing workspace")?;
    let missing = Uuid::now_v7().to_string();
    s.call(
        As::Magpie,
        "POST",
        &p(routes::WORKSPACE_CLONE, &Uuid::now_v7().to_string()),
        Some(&json!({"from": missing})),
    )
    .await?
    .expect(404, Some("no_workspace"), "clone from a missing workspace")?;
    for w in [&copy, &copy, &wid] {
        s.call(As::Magpie, "DELETE", &p(routes::WORKSPACE, w), None)
            .await?
            .expect(204, None, "DELETE workspace (also when absent)")?;
    }
    s.call(As::Magpie, "GET", &p(routes::WORKSPACE, &copy), None)
        .await?
        .expect(404, Some("no_workspace"), "GET a deleted workspace")?;
    Ok(())
}

/// Parses `--capabilities pause,snapshot`.
///
/// # Errors
/// On an unknown name.
pub fn parse_capabilities(v: &str) -> Result<Capabilities, String> {
    let mut c = Capabilities::default();
    for part in v.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part {
            "pause" | "resume" => c.pause = true,
            "snapshot" => c.snapshot = true,
            other => return Err(format!("unknown capability {other:?} (pause, snapshot)")),
        }
    }
    Ok(c)
}
