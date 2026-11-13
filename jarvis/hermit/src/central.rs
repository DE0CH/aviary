//! hermit-jarvis's gateway listener: the central half of the sandbox gateway (DE0CH/aviary docs/CONTRACTS.md "The
//! sandbox gateway contract"), reached by each session machine's `aviary-gateway` at `chat-gw.deyaochen.com` through
//! Cloudflare Access (src/wire.rs). The machine's half is in the session's own trust domain, so everything that names
//! an identity happens here: the sandbox token is verified, liveness checked, the identity headers and pair bearers
//! added. Plus the machine's boot, health and Jarvis's deliveries.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use futures_util::TryStreamExt as _;
use gateway_api::FixedName;
use hermit_api::sandbox::{ReleaseReason, SandboxPhase, SandboxState};
use hermit_api::token::Claims;
use magpie_api::ops::{HermitCreateRequest, HermitSendRequest};
use magpie_api::turns::SendRequest;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;

use crate::app::{App, LIFETIME};
use crate::db::{self, SandboxRow, SessionRow};
use crate::jarvis::{HARNESS, VerifyRequest};
use crate::wire::{self, BootReply, BootRequest, DeliverRequest, HealthReport, TOKEN_HEADER};

/// The machine gateway's proxy address, which the descriptor's proxy variables name.
pub const MACHINE_PROXY: &str = "127.0.0.1:3128";

/// The router of the gateway listener.
pub fn router(app: App) -> Router {
    Router::new()
        .route(wire::BOOT, post(boot))
        .route(wire::HEALTH, post(health))
        .route(wire::DELIVER, post(deliver))
        .route("/magpie/{*rest}", any(forward_magpie))
        .route("/hermit/{*rest}", any(forward_hermit))
        .with_state(app)
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(json!({"code": code, "message": message})),
    )
        .into_response()
}

fn unavailable(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "gateway listener: internal error");
    refuse(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "try again in a moment",
    )
}

fn hdr<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

/// `POST /jarvis/boot`: a session machine's signed boot.
async fn boot(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    let (Some(machine), Some(at), Some(sig)) = (
        hdr(&headers, "x-machine"),
        hdr(&headers, "x-time"),
        hdr(&headers, "x-sig"),
    ) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "a boot is signed with the machine key",
        );
    };
    let Ok(req) = serde_json::from_slice::<BootRequest>(&body) else {
        return refuse(StatusCode::BAD_REQUEST, "bad_request", "not a boot request");
    };
    let v = VerifyRequest {
        machine: machine.to_owned(),
        method: "POST".into(),
        path: wire::BOOT.into(),
        time: at.to_owned(),
        body_sha256: hex(&Sha256::digest(&body)),
        sig: sig.to_owned(),
    };
    let verified = match app.jarvis.verify(&v).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            return refuse(
                StatusCode::FORBIDDEN,
                "not_allowed",
                "the router does not know this signature",
            );
        }
        Err(e) => return unavailable(e),
    };
    if verified.session != req.session || verified.harness != HARNESS {
        return refuse(
            StatusCode::FORBIDDEN,
            "not_allowed",
            "the signing machine is not this aviary session's",
        );
    }
    match do_boot(&app, machine, &req).await {
        Ok(reply) => (StatusCode::OK, axum::Json(reply)).into_response(),
        Err(e) => unavailable(e),
    }
}

async fn do_boot(app: &App, machine: &str, req: &BootRequest) -> anyhow::Result<BootReply> {
    let session = match db::session(&app.pool, &req.session).await? {
        Some(s) => s,
        None => {
            // the session's conversation, made once (magpie's create is idempotent by the session id too)
            let created = app
                .magpie
                .service(None)
                .hermit_create_conversation(&HermitCreateRequest {
                    person: app.owner.clone(),
                    idempotency_key: format!("session:{}", req.session),
                    message: SendRequest {
                        client_message_id: format!("session:{}:open", req.session),
                        text: String::new(),
                        images: vec![],
                        files: vec![],
                    },
                    model: None,
                })
                .await
                .map_err(|e| anyhow::anyhow!("magpie refused the conversation: {e}"))?
                .into_inner();
            let s = SessionRow {
                session_id: req.session.clone(),
                conversation_id: created.conversation.id,
                person: app.owner.clone(),
            };
            db::put_session(&app.pool, &s).await?;
            db::session(&app.pool, &req.session)
                .await?
                .ok_or_else(|| anyhow::anyhow!("the session row vanished"))?
        }
    };
    // the same machine again (its gateway restarted) keeps its sandbox; a resume this hermit asked for is adopted;
    // anything else live is an earlier boot of the session, which the new one replaces
    let adopt = db::newest(&app.pool, session.conversation_id)
        .await?
        .filter(|r| {
            r.state != SandboxState::Gone
                && (r.machine.as_deref() == Some(machine)
                    || (r.state == SandboxState::Starting && r.machine.is_none()))
        });
    let row = match adopt {
        Some(r) => r,
        None => {
            db::start(
                &app.pool,
                &session,
                Some(machine),
                SandboxPhase::Supervisor,
                LIFETIME,
                ReleaseReason::HostLost,
            )
            .await?
        }
    };
    let claims = Claims {
        sandbox_id: row.id,
        conversation_id: row.conversation_id,
        source: Ipv4Addr::UNSPECIFIED,
        expires_at: u64::try_from(row.expires_at.unix_timestamp()).unwrap_or(0),
        person: row.person.clone(),
    };
    let token = app
        .keys
        .signing
        .sign(&claims)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let env: serde_json::Map<String, Value> = req
        .env
        .iter()
        .filter(|(k, _)| wire::BOOT_ENV_NAMES.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let boot = json!({
        "env": env,
        "memoryBytes": req.memory_bytes,
        "cpus": req.cpus,
        "proxy": gateway_api::credential::proxy_url(&token, MACHINE_PROXY),
    });
    let image = req.image.clone();
    let machine = machine.to_owned();
    db::update(&app.pool, row.id, |r| {
        r.machine = Some(machine.clone());
        r.phase = Some(SandboxPhase::Supervisor);
        r.image = Some(image.clone());
        r.boot = boot.clone();
        true
    })
    .await?;
    Ok(BootReply {
        sandbox_id: row.id,
        conversation_id: row.conversation_id,
        token,
        control_token: app.keys.control_token(row.id),
        settings: Some(app.settings.clone()),
    })
}

/// The caller's verified token and its live row (a refusal is the answer as it is sent).
#[allow(clippy::result_large_err)]
async fn sandbox_of(app: &App, headers: &HeaderMap) -> Result<(Claims, SandboxRow), Response> {
    let Some(token) = hdr(headers, TOKEN_HEADER) else {
        return Err(refuse(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "no sandbox token",
        ));
    };
    let now = u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap_or(0);
    // tokens are minted for any source address (the machine's half is the only presenter)
    let Ok(claims) = app
        .keys
        .verify
        .verify(token, now, IpAddr::V4(Ipv4Addr::LOCALHOST))
    else {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "not_allowed",
            "the sandbox token does not verify",
        ));
    };
    let row = match db::sandbox(&app.pool, claims.sandbox_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(refuse(
                StatusCode::FORBIDDEN,
                "not_allowed",
                "no such sandbox",
            ));
        }
        Err(e) => return Err(unavailable(e)),
    };
    if !crate::hermit::liveness(&row, &app.owner).live {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "not_allowed",
            "the sandbox has ended",
        ));
    }
    Ok((claims, row))
}

/// `POST /jarvis/health`: the supervisor is up and ready on the machine → the sandbox is `ready`.
async fn health(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    let (_, row) = match sandbox_of(&app, &headers).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Ok(h) = serde_json::from_slice::<HealthReport>(&body) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "not a health report",
        );
    };
    let info = hermit_api::sandbox::SupervisorInfo {
        protocol: h.protocol,
        version: h.version.clone(),
        boot_id: h.boot_id.clone(),
    };
    let r = db::update(&app.pool, row.id, |r| {
        let mut changed = false;
        if r.state == SandboxState::Starting && h.ok && h.ready {
            r.state = SandboxState::Ready;
            r.phase = None;
            r.ready_at = Some(OffsetDateTime::now_utc());
            changed = true;
        }
        if h.ok && r.supervisor.as_ref() != Some(&info) {
            r.supervisor = Some(info.clone());
            changed = true;
        }
        changed
    })
    .await;
    match r {
        Ok(Some(r)) => {
            // the conversation's title for the session's card (a failure is no title this time: the next report asks
            // again)
            let title = app
                .magpie
                .service(None)
                .hermit_conversation(r.conversation_id)
                .await
                .ok()
                .and_then(|c| c.title);
            let reply = wire::HealthReply {
                state: r.state.as_str().to_owned(),
                title,
            };
            (StatusCode::OK, axum::Json(reply)).into_response()
        }
        Ok(None) => refuse(StatusCode::FORBIDDEN, "not_allowed", "no such sandbox"),
        Err(e) => unavailable(e),
    }
}

/// `POST /jarvis/deliver`: a prompt from Jarvis into the session's conversation, as its person. While a turn runs
/// it joins that turn (magpie's `joinRunning`: Claude Code takes it in after its next tool result); a refusal
/// magpie still makes (409 `turn_running` from one that predates `joinRunning`) goes back as it is, and Jarvis's
/// inbox delivers it again later.
async fn deliver(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    let (_, row) = match sandbox_of(&app, &headers).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Ok(d) = serde_json::from_slice::<DeliverRequest>(&body) else {
        return refuse(StatusCode::BAD_REQUEST, "bad_request", "not a delivery");
    };
    let sent = app
        .magpie
        .service(None)
        .hermit_send_turn(
            row.conversation_id,
            &HermitSendRequest {
                person: row.person.clone(),
                message: SendRequest {
                    client_message_id: d.id,
                    text: d.text,
                    images: vec![],
                    files: vec![],
                },
                // Jarvis's message joins a turn that runs, as Deyao's own mid-reply messages do: a warning (the
                // pressure watchdog), an Inbox outcome or a wakeup must not wait for a long turn to end
                join_running: true,
            },
        )
        .await;
    match sent {
        Ok(a) => {
            let status = if a.is_new() {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            (status, axum::Json(a.into_inner())).into_response()
        }
        Err(e) => match e.api() {
            Some(r) => {
                let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_GATEWAY);
                (
                    status,
                    axum::Json(json!({"code": r.code.as_str(), "message": r.message})),
                )
                    .into_response()
            }
            None => unavailable(e),
        },
    }
}

async fn forward_magpie(State(app): State<App>, req: Request) -> Response {
    forward(&app, FixedName::Magpie, req).await
}

async fn forward_hermit(State(app): State<App>, req: Request) -> Response {
    forward(&app, FixedName::Hermit, req).await
}

/// Hop-by-hop headers and what Cloudflare and the machine's half add: never forwarded either way.
fn dropped(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
            | TOKEN_HEADER
    ) || name.starts_with("cf-")
        || name.starts_with("x-forwarded-")
        || name == "cdn-loop"
        || name == "true-client-ip"
        || name == "x-real-ip"
}

async fn forward(app: &App, name: FixedName, req: Request) -> Response {
    let (claims, _) = match sandbox_of(app, req.headers()).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let prefix = format!("/{}", name.as_str());
    let pq = req
        .uri()
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .strip_prefix(&prefix)
        .unwrap_or("/")
        .to_owned();
    let method = req.method().clone();
    if !name.allows(method.as_str(), &pq) {
        return refuse(
            StatusCode::FORBIDDEN,
            "not_allowed",
            "not a path a sandbox may use",
        );
    }
    let (base, bearer) = match name {
        FixedName::Magpie => (&app.magpie_url, &app.gateway_to_magpie),
        _ => (&app.hermit_url, &app.gateway_to_hermit),
    };
    let mut out = reqwest::header::HeaderMap::new();
    let mut request_id = None;
    for (k, v) in req.headers() {
        let n = k.as_str();
        if dropped(n) {
            continue;
        }
        if n == "aviary-request" {
            request_id = v
                .to_str()
                .ok()
                .filter(|s| gateway_api::headers::keep_request_id(s))
                .map(str::to_owned);
            continue;
        }
        if gateway_api::headers::is_stripped(n) {
            continue;
        }
        out.append(k.clone(), v.clone());
    }
    for (k, v) in gateway_api::headers::identity(&claims) {
        if let Ok(v) = HeaderValue::from_str(&v) {
            out.insert(k, v);
        }
    }
    let rid = request_id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    if let Ok(v) = HeaderValue::from_str(&rid) {
        out.insert(HeaderName::from_static("aviary-request"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {bearer}")) {
        out.insert(header::AUTHORIZATION, v);
    }
    let limit = usize::try_from(name.body_limit()).unwrap_or(usize::MAX);
    let body = req.into_body();
    let stream = http_body_util::BodyDataStream::new(http_body_util::Limited::new(body, limit))
        .map_err(|e| std::io::Error::other(e.to_string()));
    let mut up = app
        .http
        .request(method.clone(), format!("{base}{pq}"))
        .headers(out);
    if method != Method::GET && method != Method::HEAD {
        up = up.body(reqwest::Body::wrap_stream(stream));
    }
    let r = match up.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(name = name.as_str(), error = %e, "upstream unreachable");
            return refuse(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "the service did not answer",
            );
        }
    };
    let mut resp = Response::builder().status(r.status().as_u16());
    for (k, v) in r.headers() {
        if !dropped(k.as_str()) {
            resp = resp.header(k, v);
        }
    }
    resp.body(Body::from_stream(r.bytes_stream()))
        .unwrap_or_else(|_| {
            refuse(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "a bad upstream answer",
            )
        })
}

pub(crate) fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

/// The outbound client of the gateway listener: no overall timeout (long-polls and streams), keep-alive.
///
/// # Errors
/// When the client cannot be built.
pub fn http() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(aviary_core::http::tls_config())
        .connect_timeout(Duration::from_secs(5))
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent(concat!("hermit-jarvis/", env!("CARGO_PKG_VERSION")))
        .build()?)
}
