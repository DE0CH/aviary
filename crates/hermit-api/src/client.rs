//! The typed hermit client: one method per route, paths built only from [`crate::routes`] constants. The caller
//! passes its `reqwest::Client` (pool, proxy, TLS and `User-Agent` are the caller's, built by `aviary-core::http`
//! or, in a sandbox, with the boot environment's proxy).

use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use crate::admin::{
    AdminEventsQuery, AdminRelease, AdminSandboxesQuery, FreshHost, HostPatch, PurgeReply, PurgeRequest, Recycle,
    SandboxesEnabled,
};
use crate::deployment::Deployment;
use crate::error::ErrorBody;
use crate::exec::{ExecRequest, ExecResult};
use crate::people::{People, Person, WarmReply, WarmRequest};
use crate::routes::{self as r, fill, query};
use crate::sandbox::{
    PutSandbox, PutSandboxRequest, ReleaseSandboxReply, ReleaseSandboxRequest, Sandbox, SandboxEvents,
    SandboxEventsQuery, SandboxQuery, SnapshotReply,
};
use crate::sandbox_surface::{ActivityReply, ActivityReport, SandboxSelf, SecretKeys, SecretValue};
use crate::server::{SandboxIdentity, SelfAnswer};
use crate::time::Timestamp;
use crate::token::{TokenKeys, TokenLiveness};
use crate::workspaces::{
    ByteRange, CloneRequest, Entries, EntriesQuery, FileMeta, FilesQuery, PutWorkspace, PutWorkspaceRequest, Workspace,
};
use crate::{CORE, SANDBOX, headers};

/// Timeouts per call (hermit 3.6.3: 35 s for a 25 s long-poll).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Added to `waitMs` for held requests.
pub const WAIT_SLACK: Duration = Duration::from_secs(10);
/// `DELETE /v1/workspaces/{wid}` waits up to 60 s for a live sandbox to go.
pub const DELETE_WORKSPACE_TIMEOUT: Duration = Duration::from_secs(75);
/// The admin exec route forwards a call of up to 65 s.
pub const ADMIN_EXEC_TIMEOUT: Duration = Duration::from_secs(70);
/// A file read: the time to the response headers. The body has no total limit (a 1 GiB file over a slow link takes
/// as long as it takes), only [`FILE_IDLE_TIMEOUT`] between chunks.
pub const FILE_HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
/// A file read's longest wait for the next chunk of the body.
pub const FILE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    /// A non-2xx answer with the platform's envelope (or a synthesised one when the body was not JSON).
    #[error("{0}")]
    Api(Refusal),
    /// A 2xx answer whose body did not decode.
    #[error("decode {status}: {message}")]
    Decode { status: u16, message: String },
    /// A 2xx answer that says the call did not happen (`{"released": false}`, `{"ready": false}`).
    #[error("not done {status}: {message}")]
    NotDone { status: u16, message: String },
    /// No response headers, or no next chunk of a streamed body, in time.
    #[error("timed out: {0}")]
    Timeout(&'static str),
}

impl ClientError {
    /// The error code of an API answer.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Api(e) => Some(&e.body.code),
            _ => None,
        }
    }

    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api(e) => Some(e.status),
            Self::Decode { status, .. } | Self::NotDone { status, .. } => Some(*status),
            Self::Transport(e) => e.status().map(|s| s.as_u16()),
            Self::Timeout(_) => None,
        }
    }
}

/// A non-2xx answer as the client saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    pub status: u16,
    pub body: ErrorBody,
    /// `retry-after` in seconds.
    pub retry_after: Option<u64>,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.body.code, self.body.message)
    }
}

impl Refusal {
    #[must_use]
    pub fn is(&self, code: &str) -> bool {
        self.body.code == code
    }
}

/// hermit's client. Cheap to clone; the builder methods return a modified copy. Its `Debug` form shows the base URL
/// and header names only: the pair token and the identity headers' values are redacted.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    bearer: Option<String>,
    extra: Vec<(HeaderName, HeaderValue)>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let extra: Vec<(&str, &str)> = self
            .extra
            .iter()
            .map(|(n, v)| {
                let shown = if crate::server::is_sensitive_header(n) {
                    "<redacted>"
                } else {
                    v.to_str().unwrap_or("<binary>")
                };
                (n.as_str(), shown)
            })
            .collect();
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("bearer", &self.bearer.as_ref().map(|_| "<redacted>"))
            .field("headers", &extra)
            .finish_non_exhaustive()
    }
}

/// A file read in progress (`GET /v1/workspaces/{wid}/files`): the typed headers and the body as a stream, never
/// buffered by the client. Read it with [`FileDownload::chunk`] or [`FileDownload::bytes`] (each chunk within
/// [`FILE_IDLE_TIMEOUT`]), or relay it with [`FileDownload::into_response`] (`bytes_stream()`, under the
/// `reqwest::Client`'s own `read_timeout`).
pub struct FileDownload {
    pub meta: FileMeta,
    response: reqwest::Response,
}

impl std::fmt::Debug for FileDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDownload")
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

impl FileDownload {
    /// The next chunk of the body, `None` at the end.
    ///
    /// # Errors
    /// Transport failures; [`ClientError::Timeout`] after [`FILE_IDLE_TIMEOUT`] without a chunk.
    pub async fn chunk(&mut self) -> R<Option<axum::body::Bytes>> {
        tokio::time::timeout(FILE_IDLE_TIMEOUT, self.response.chunk())
            .await
            .map_err(|_| ClientError::Timeout("file body"))?
            .map_err(ClientError::from)
    }

    /// The whole body in memory (small files and tests; a relay uses [`FileDownload::into_response`]).
    ///
    /// # Errors
    /// As [`FileDownload::chunk`].
    pub async fn bytes(mut self) -> R<axum::body::Bytes> {
        let mut out = Vec::with_capacity(usize::try_from(self.meta.content_length()).unwrap_or(0).min(1 << 20));
        while let Some(c) = self.chunk().await? {
            out.extend_from_slice(&c);
        }
        Ok(out.into())
    }

    /// The response itself, for relaying its body as a stream.
    #[must_use]
    pub fn into_response(self) -> reqwest::Response {
        self.response
    }
}

type R<T> = Result<T, ClientError>;

impl Client {
    /// `base` is e.g. `http://hermit` (no trailing slash needed).
    #[must_use]
    pub fn new(http: reqwest::Client, base: impl Into<String>) -> Self {
        let mut base = base.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self {
            http,
            base,
            bearer: None,
            extra: Vec::new(),
        }
    }

    /// The caller's pair token (none for the supervisor, whose credential is the proxy's).
    #[must_use]
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }

    fn header(mut self, name: HeaderName, value: &str) -> Self {
        if let Ok(v) = HeaderValue::from_str(value) {
            self.extra.retain(|(n, _)| *n != name);
            self.extra.push((name, v));
        }
        self
    }

    /// `Aviary-Request` on every call of the returned client.
    #[must_use]
    pub fn request_id(self, id: &str) -> Self {
        self.header(headers::REQUEST, id)
    }

    /// `Aviary-Person` (pigeon on `/ops/*`).
    #[must_use]
    pub fn as_person(self, email: &str) -> Self {
        self.header(headers::PERSON, email)
    }

    /// `Aviary-Admin` (admin routes).
    #[must_use]
    pub fn as_admin(self, email: &str) -> Self {
        self.header(headers::ADMIN, email)
    }

    /// The gateway's three headers (a gateway or a conformance suite forwarding `/v1/sandbox/**`).
    #[must_use]
    pub fn sandbox_identity(self, id: &SandboxIdentity) -> Self {
        self.header(headers::SANDBOX_ID, &id.id.to_string())
            .header(headers::SANDBOX_CONVERSATION, &id.conversation.to_string())
            .header(headers::SANDBOX_PERSON, &id.person)
    }

    fn build(&self, method: Method, path: &str, surface: (&str, u32), timeout: Duration) -> reqwest::RequestBuilder {
        self.build_untimed(method, path, surface).timeout(timeout)
    }

    /// A request with no total timeout (a streamed file read).
    fn build_untimed(&self, method: Method, path: &str, surface: (&str, u32)) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header(headers::PROTOCOL, format!("{}={}", surface.0, surface.1));
        if let Some(t) = &self.bearer {
            rb = rb.bearer_auth(t);
        }
        for (n, v) in &self.extra {
            rb = rb.header(n.clone(), v.clone());
        }
        rb
    }

    async fn send(rb: reqwest::RequestBuilder) -> R<(StatusCode, reqwest::header::HeaderMap, axum::body::Bytes)> {
        let res = rb.send().await?;
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.bytes().await?;
        if status.is_success() || status == StatusCode::NOT_MODIFIED {
            Ok((status, headers, bytes))
        } else {
            Err(ClientError::Api(refusal(status, &headers, &bytes)))
        }
    }

    async fn call<T: DeserializeOwned>(&self, rb: reqwest::RequestBuilder) -> R<(StatusCode, T)> {
        let (status, _, bytes) = Self::send(rb).await?;
        let v = serde_json::from_slice(&bytes).map_err(|e| ClientError::Decode {
            status: status.as_u16(),
            message: e.to_string(),
        })?;
        Ok((status, v))
    }

    fn json<B: Serialize>(rb: reqwest::RequestBuilder, body: &B) -> reqwest::RequestBuilder {
        rb.json(body)
    }

    // H1-H3 and people

    /// H1 `GET /v1/deployment`.
    ///
    /// # Errors
    /// Transport, API or decode failures.
    pub async fn deployment(&self) -> R<Deployment> {
        Ok(self
            .call(self.build(Method::GET, r::DEPLOYMENT, CORE, DEFAULT_TIMEOUT))
            .await?
            .1)
    }

    /// H2 `GET /v1/people/{email}`.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 400 `bad_email`.
    pub async fn person(&self, email: &str) -> R<Person> {
        let p = fill(r::PERSON, &[email]);
        Ok(self.call(self.build(Method::GET, &p, CORE, DEFAULT_TIMEOUT)).await?.1)
    }

    /// `GET /v1/people`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn people(&self) -> R<People> {
        Ok(self
            .call(self.build(Method::GET, r::PEOPLE, CORE, DEFAULT_TIMEOUT))
            .await?
            .1)
    }

    /// H3 `POST /v1/warm`.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 403 `not_allowed`.
    pub async fn warm(&self, body: &WarmRequest) -> R<WarmReply> {
        let rb = Self::json(self.build(Method::POST, r::WARM, CORE, DEFAULT_TIMEOUT), body);
        Ok(self.call(rb).await?.1)
    }

    // sandboxes

    /// `PUT /v1/conversations/{cid}/sandbox`: 200 ready, 202 starting.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 403 `not_allowed`, 409 `person_mismatch`, 503 `sandbox_busy`.
    pub async fn put_sandbox(&self, cid: Uuid, body: &PutSandboxRequest) -> R<PutSandbox> {
        let p = fill(r::SANDBOX, &[&cid.to_string()]);
        let rb = Self::json(self.build(Method::PUT, &p, CORE, DEFAULT_TIMEOUT), body);
        let (status, s): (_, Sandbox) = self.call(rb).await?;
        Ok(if status == StatusCode::OK {
            PutSandbox::Ready(s)
        } else {
            PutSandbox::Starting(s)
        })
    }

    /// `GET /v1/conversations/{cid}/sandbox?waitMs=&afterVersion=` (held up to `waitMs`).
    ///
    /// # Errors
    /// As [`Client::deployment`]; 404 `no_sandbox`.
    pub async fn get_sandbox(&self, cid: Uuid, q: SandboxQuery) -> R<Sandbox> {
        let p = format!(
            "{}{}",
            fill(r::SANDBOX, &[&cid.to_string()]),
            query(&[
                ("waitMs", q.wait_ms.map(|v| v.to_string())),
                ("afterVersion", q.after_version.map(|v| v.to_string())),
            ])
        );
        let timeout = DEFAULT_TIMEOUT.max(Duration::from_millis(q.wait_ms.unwrap_or(0).into()) + WAIT_SLACK);
        Ok(self.call(self.build(Method::GET, &p, CORE, timeout)).await?.1)
    }

    /// `POST /v1/conversations/{cid}/sandbox/release`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn release_sandbox(&self, cid: Uuid, body: &ReleaseSandboxRequest) -> R<ReleaseSandboxReply> {
        let p = fill(r::SANDBOX_RELEASE, &[&cid.to_string()]);
        let rb = Self::json(self.build(Method::POST, &p, CORE, DEFAULT_TIMEOUT), body);
        Ok(self.call(rb).await?.1)
    }

    /// `POST …/sandbox/pause` (202; 501 in implementations without pause).
    ///
    /// # Errors
    /// As [`Client::deployment`]; 501 `not_supported`.
    pub async fn pause_sandbox(&self, cid: Uuid) -> R<Sandbox> {
        let p = fill(r::SANDBOX_PAUSE, &[&cid.to_string()]);
        let rb = Self::json(
            self.build(Method::POST, &p, CORE, DEFAULT_TIMEOUT),
            &serde_json::json!({}),
        );
        Ok(self.call(rb).await?.1)
    }

    /// `POST …/sandbox/resume` (202).
    ///
    /// # Errors
    /// As [`Client::pause_sandbox`].
    pub async fn resume_sandbox(&self, cid: Uuid) -> R<Sandbox> {
        let p = fill(r::SANDBOX_RESUME, &[&cid.to_string()]);
        let rb = Self::json(
            self.build(Method::POST, &p, CORE, DEFAULT_TIMEOUT),
            &serde_json::json!({}),
        );
        Ok(self.call(rb).await?.1)
    }

    /// `POST …/sandbox/snapshot` (201).
    ///
    /// # Errors
    /// As [`Client::pause_sandbox`].
    pub async fn snapshot_sandbox(&self, cid: Uuid) -> R<SnapshotReply> {
        let p = fill(r::SANDBOX_SNAPSHOT, &[&cid.to_string()]);
        let rb = Self::json(
            self.build(Method::POST, &p, CORE, DEFAULT_TIMEOUT),
            &serde_json::json!({}),
        );
        Ok(self.call(rb).await?.1)
    }

    /// `GET /v1/sandbox-events?after=&limit=&waitMs=`.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 410 `cursor_expired`.
    pub async fn sandbox_events(&self, q: SandboxEventsQuery) -> R<SandboxEvents> {
        let p = format!(
            "{}{}",
            r::SANDBOX_EVENTS,
            query(&[
                ("after", Some(q.after.to_string())),
                ("limit", q.limit.map(|v| v.to_string())),
                ("waitMs", q.wait_ms.map(|v| v.to_string())),
            ])
        );
        let timeout = DEFAULT_TIMEOUT.max(Duration::from_millis(q.wait_ms.unwrap_or(0).into()) + WAIT_SLACK);
        Ok(self.call(self.build(Method::GET, &p, CORE, timeout)).await?.1)
    }

    // workspaces

    /// `PUT /v1/workspaces/{wid}`: 201 made, 200 existed.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 503 `workspace_unavailable`.
    pub async fn put_workspace(&self, wid: Uuid) -> R<PutWorkspace> {
        let p = fill(r::WORKSPACE, &[&wid.to_string()]);
        let rb = Self::json(
            self.build(Method::PUT, &p, CORE, DEFAULT_TIMEOUT),
            &PutWorkspaceRequest {},
        );
        let (status, w): (_, Workspace) = self.call(rb).await?;
        Ok(if status == StatusCode::CREATED {
            PutWorkspace::Created(w)
        } else {
            PutWorkspace::Existed(w)
        })
    }

    /// `GET /v1/workspaces/{wid}`.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 404 `no_workspace`.
    pub async fn get_workspace(&self, wid: Uuid) -> R<Workspace> {
        let p = fill(r::WORKSPACE, &[&wid.to_string()]);
        Ok(self.call(self.build(Method::GET, &p, CORE, DEFAULT_TIMEOUT)).await?.1)
    }

    /// `DELETE /v1/workspaces/{wid}` (204, also when absent).
    ///
    /// # Errors
    /// As [`Client::deployment`]; 503 `workspace_unavailable`.
    pub async fn delete_workspace(&self, wid: Uuid) -> R<()> {
        let p = fill(r::WORKSPACE, &[&wid.to_string()]);
        Self::send(self.build(Method::DELETE, &p, CORE, DELETE_WORKSPACE_TIMEOUT)).await?;
        Ok(())
    }

    /// `GET /v1/workspaces/{wid}/entries?path=&cursor=`.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 404 `no_path`, 400 `bad_path`, 409 `not_a_dir`.
    pub async fn workspace_entries(&self, wid: Uuid, q: &EntriesQuery) -> R<Entries> {
        let p = format!(
            "{}{}",
            fill(r::WORKSPACE_ENTRIES, &[&wid.to_string()]),
            query(&[("path", q.path.clone()), ("cursor", q.cursor.clone())])
        );
        Ok(self.call(self.build(Method::GET, &p, CORE, DEFAULT_TIMEOUT)).await?.1)
    }

    /// `GET /v1/workspaces/{wid}/files?path=` with an optional single range. Waits at most
    /// [`FILE_HEADERS_TIMEOUT`] for the headers; the body is a stream with no total timeout.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 404 `no_path`, 409 `not_a_file`, 416 `bad_range`; [`ClientError::Timeout`].
    pub async fn workspace_file(&self, wid: Uuid, q: &FilesQuery, range: Option<ByteRange>) -> R<FileDownload> {
        let p = format!(
            "{}{}",
            fill(r::WORKSPACE_FILES, &[&wid.to_string()]),
            query(&[("path", Some(q.path.clone()))])
        );
        let mut rb = self.build_untimed(Method::GET, &p, CORE);
        if let Some(range) = range {
            rb = rb.header(reqwest::header::RANGE, range.header());
        }
        let res = tokio::time::timeout(FILE_HEADERS_TIMEOUT, rb.send())
            .await
            .map_err(|_| ClientError::Timeout("file headers"))??;
        let status = res.status();
        if !status.is_success() {
            let headers = res.headers().clone();
            let bytes = res.bytes().await?;
            return Err(ClientError::Api(refusal(status, &headers, &bytes)));
        }
        let h = res.headers();
        let text = |name: reqwest::header::HeaderName| h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let decode = |message: &str| ClientError::Decode {
            status: status.as_u16(),
            message: message.to_owned(),
        };
        let mtime = text(reqwest::header::LAST_MODIFIED)
            .and_then(|v| Timestamp::parse_http_date(&v).ok())
            .ok_or_else(|| decode("last-modified"))?;
        let etag = text(reqwest::header::ETAG).unwrap_or_default();
        let (size, served) = if status == StatusCode::PARTIAL_CONTENT {
            let cr = text(reqwest::header::CONTENT_RANGE).ok_or_else(|| decode("content-range"))?;
            parse_content_range(&cr).ok_or_else(|| decode("content-range"))?
        } else {
            let len = text(reqwest::header::CONTENT_LENGTH)
                .and_then(|v| v.trim().parse().ok())
                .ok_or_else(|| decode("content-length"))?;
            (len, None)
        };
        Ok(FileDownload {
            meta: FileMeta {
                size,
                mtime,
                etag,
                served,
            },
            response: res,
        })
    }

    /// `POST /v1/workspaces/{wid}/clone` (201).
    ///
    /// # Errors
    /// As [`Client::deployment`]; 404 `no_workspace`, 409 `exists`.
    pub async fn clone_workspace(&self, wid: Uuid, body: &CloneRequest) -> R<Workspace> {
        let p = fill(r::WORKSPACE_CLONE, &[&wid.to_string()]);
        let rb = Self::json(self.build(Method::POST, &p, CORE, DEFAULT_TIMEOUT), body);
        Ok(self.call(rb).await?.1)
    }

    // token keys and liveness

    /// `GET /v1/sandbox-token-keys`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn token_keys(&self) -> R<TokenKeys> {
        Ok(self
            .call(self.build(Method::GET, r::SANDBOX_TOKEN_KEYS, CORE, DEFAULT_TIMEOUT))
            .await?
            .1)
    }

    /// `GET /v1/sandbox-tokens/{sandboxId}` with the caller's own timeout (the gateway uses 1 s).
    ///
    /// # Errors
    /// As [`Client::deployment`]; 404 `unknown_sandbox`.
    pub async fn token_liveness(&self, sandbox_id: Uuid, timeout: Duration) -> R<TokenLiveness> {
        let p = fill(r::SANDBOX_TOKEN, &[&sandbox_id.to_string()]);
        Ok(self.call(self.build(Method::GET, &p, CORE, timeout)).await?.1)
    }

    // the sandbox's own routes (surface hermit-sandbox)

    /// `GET /v1/sandbox/self` with an optional `If-None-Match`.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 404 `no_sandbox`.
    pub async fn sandbox_self(&self, if_none_match: Option<&str>) -> R<SelfAnswer> {
        let mut rb = self.build(Method::GET, r::SANDBOX_SELF, SANDBOX, DEFAULT_TIMEOUT);
        if let Some(etag) = if_none_match {
            rb = rb.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let (status, h, bytes) = Self::send(rb).await?;
        if status == StatusCode::NOT_MODIFIED {
            let etag = h
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .or(if_none_match)
                .unwrap_or_default()
                .to_owned();
            return Ok(SelfAnswer::NotModified { etag });
        }
        let d: SandboxSelf = serde_json::from_slice(&bytes).map_err(|e| ClientError::Decode {
            status: status.as_u16(),
            message: e.to_string(),
        })?;
        Ok(SelfAnswer::Fresh(Box::new(d)))
    }

    /// `GET /v1/sandbox/secrets`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn sandbox_secrets(&self) -> R<SecretKeys> {
        Ok(self
            .call(self.build(Method::GET, r::SANDBOX_SECRETS, SANDBOX, DEFAULT_TIMEOUT))
            .await?
            .1)
    }

    /// `GET /v1/sandbox/secrets/{key}`.
    ///
    /// # Errors
    /// As [`Client::deployment`]; 404 `not_granted`.
    pub async fn sandbox_secret(&self, key: &str) -> R<SecretValue> {
        let p = fill(r::SANDBOX_SECRET, &[key]);
        Ok(self
            .call(self.build(Method::GET, &p, SANDBOX, DEFAULT_TIMEOUT))
            .await?
            .1)
    }

    /// `POST /v1/sandbox/activity`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn sandbox_activity(&self, body: &ActivityReport) -> R<ActivityReply> {
        let rb = Self::json(
            self.build(Method::POST, r::SANDBOX_ACTIVITY, SANDBOX, DEFAULT_TIMEOUT),
            body,
        );
        Ok(self.call(rb).await?.1)
    }

    // operator page

    /// `GET /ops/` (the HTML).
    ///
    /// # Errors
    /// As [`Client::deployment`]; 403 `not_admin`.
    pub async fn ops_page(&self) -> R<String> {
        let (_, _, bytes) = Self::send(self.build(Method::GET, r::OPS_PAGE, CORE, DEFAULT_TIMEOUT)).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `GET /ops/snapshot?tz=`.
    ///
    /// # Errors
    /// As [`Client::ops_page`].
    pub async fn ops_snapshot(&self, tz: Option<&str>) -> R<Value> {
        let p = format!("{}{}", r::OPS_SNAPSHOT, query(&[("tz", tz.map(str::to_owned))]));
        Ok(self.call(self.build(Method::GET, &p, CORE, DEFAULT_TIMEOUT)).await?.1)
    }

    // admin

    /// `GET /v1/admin/hosts`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_hosts(&self) -> R<Value> {
        Ok(self
            .call(self.build(Method::GET, r::ADMIN_HOSTS, CORE, DEFAULT_TIMEOUT))
            .await?
            .1)
    }

    /// `PATCH /v1/admin/hosts/{name}`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_patch_host(&self, name: &str, body: &HostPatch) -> R<Value> {
        let p = fill(r::ADMIN_HOST, &[name]);
        let rb = Self::json(self.build(Method::PATCH, &p, CORE, DEFAULT_TIMEOUT), body);
        Ok(self.call(rb).await?.1)
    }

    /// `PUT /v1/admin/sandboxes-enabled`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_sandboxes_enabled(&self, body: &SandboxesEnabled) -> R<Value> {
        let rb = Self::json(
            self.build(Method::PUT, r::ADMIN_SANDBOXES_ENABLED, CORE, DEFAULT_TIMEOUT),
            body,
        );
        Ok(self.call(rb).await?.1)
    }

    /// `POST /v1/admin/fresh-host`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_fresh_host(&self, body: &FreshHost) -> R<Value> {
        let rb = Self::json(
            self.build(Method::POST, r::ADMIN_FRESH_HOST, CORE, DEFAULT_TIMEOUT),
            body,
        );
        Ok(self.call(rb).await?.1)
    }

    /// `GET /v1/admin/sandboxes?conversation=&person=&since=`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_sandboxes(&self, q: &AdminSandboxesQuery) -> R<Value> {
        let p = format!(
            "{}{}",
            r::ADMIN_SANDBOXES,
            query(&[
                ("conversation", q.conversation.map(|v| v.to_string())),
                ("person", q.person.clone()),
                ("since", q.since.map(|v| v.to_string())),
            ])
        );
        Ok(self.call(self.build(Method::GET, &p, CORE, DEFAULT_TIMEOUT)).await?.1)
    }

    /// `GET /v1/admin/sandboxes/{id}/log`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_sandbox_log(&self, id: Uuid) -> R<Value> {
        let p = fill(r::ADMIN_SANDBOX_LOG, &[&id.to_string()]);
        Ok(self.call(self.build(Method::GET, &p, CORE, DEFAULT_TIMEOUT)).await?.1)
    }

    /// `POST /v1/admin/sandboxes/{id}/release`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_release(&self, id: Uuid, body: &AdminRelease) -> R<Value> {
        let p = fill(r::ADMIN_SANDBOX_RELEASE, &[&id.to_string()]);
        let rb = Self::json(self.build(Method::POST, &p, CORE, DEFAULT_TIMEOUT), body);
        Ok(self.call(rb).await?.1)
    }

    /// `POST /v1/admin/sandboxes/{id}/exec`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_exec(&self, id: Uuid, body: &ExecRequest) -> R<ExecResult> {
        let p = fill(r::ADMIN_SANDBOX_EXEC, &[&id.to_string()]);
        let rb = Self::json(self.build(Method::POST, &p, CORE, ADMIN_EXEC_TIMEOUT), body);
        Ok(self.call(rb).await?.1)
    }

    /// `POST /v1/admin/recycle`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_recycle(&self, body: &Recycle) -> R<Value> {
        let rb = Self::json(self.build(Method::POST, r::ADMIN_RECYCLE, CORE, DEFAULT_TIMEOUT), body);
        Ok(self.call(rb).await?.1)
    }

    /// `POST /v1/admin/people/{email}/purge` (202).
    ///
    /// # Errors
    /// As [`Client::deployment`]; 409 `still_allowed`.
    pub async fn admin_purge(&self, email: &str, body: &PurgeRequest) -> R<PurgeReply> {
        let p = fill(r::ADMIN_PERSON_PURGE, &[email]);
        let rb = Self::json(self.build(Method::POST, &p, CORE, DEFAULT_TIMEOUT), body);
        Ok(self.call(rb).await?.1)
    }

    /// `GET /v1/admin/events?limit=`.
    ///
    /// # Errors
    /// As [`Client::deployment`].
    pub async fn admin_events(&self, q: AdminEventsQuery) -> R<Value> {
        let p = format!(
            "{}{}",
            r::ADMIN_EVENTS,
            query(&[("limit", q.limit.map(|v| v.to_string()))])
        );
        Ok(self.call(self.build(Method::GET, &p, CORE, DEFAULT_TIMEOUT)).await?.1)
    }
}

/// Reads a non-2xx answer; a body that is not the envelope gives code `http_<status>`.
#[must_use]
pub fn refusal(status: StatusCode, headers: &reqwest::header::HeaderMap, bytes: &[u8]) -> Refusal {
    let body = serde_json::from_slice::<ErrorBody>(bytes).unwrap_or_else(|_| ErrorBody {
        code: format!("http_{}", status.as_u16()),
        message: String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned(),
        fields: serde_json::Map::new(),
    });
    let retry_after = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    Refusal {
        status: status.as_u16(),
        body,
        retry_after,
    }
}

/// `bytes first-last/size` → `(size, Some((first, last)))`.
fn parse_content_range(v: &str) -> Option<(u64, Option<(u64, u64)>)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, size) = rest.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    Some((size.parse().ok()?, Some((a.parse().ok()?, b.parse().ok()?))))
}
