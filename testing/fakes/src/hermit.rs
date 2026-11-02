//! fake-hermit: `hermit_api::server::Server` behind hermit-api's own guarded router (the caller tokens of hermit
//! 3.2), with scripted answers on every route and a model of the contract as hermit 3.3-3.9 states it:
//!
//! - deployment facts and the people catalogue (`@domain` entries, admins always allowed, emails trimmed and
//!   lowercased, `bad_email`);
//! - one sandbox per conversation: `PUT` is idempotent (200 ready, 202 starting; `not_allowed`, `person_mismatch`),
//!   the long-poll `GET` (wakes on a change, times out with the same version), release (idempotent; `releasing`
//!   then `gone` on the feed), pause/resume/snapshot answering 501 `not_supported` (the YYC behaviour) unless
//!   scripted;
//! - the change feed (`latest`, order, `limit`, the long-poll, 410 `cursor_expired` after
//!   [`FakeHermit::expire_events_before`]);
//! - signed sandbox tokens ([`FakeHermit::mint_token`]), the published keys, liveness (`live: false` for gone or
//!   not-allowed, 404 for unknown sandboxes);
//! - the sandbox's own routes for a live sandbox whose identity headers match (`no_sandbox` otherwise): the
//!   descriptor with its etag and 304, secrets by grant (`not_granted`), activity reports updating `busy` and
//!   answering a pending release;
//! - workspaces as in-memory files (put, get, delete, entries, files with ranges, clone).
//!
//! A starting sandbox becomes ready after [`HermitConfig::ready_after`] (default 0: on the next read) or only when
//! the test calls [`FakeHermit::make_ready`] (`ready_after: None`). The operator page and the admin routes have no
//! model: unscripted, they answer 501.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aviary_core::auth::{Caller, Callers};
use aviary_core::http::ApiError;
use axum::Router;
use axum::body::Bytes;
use axum::http::Method;
use hermit_api::admin::{
    AdminEventsQuery, AdminRelease, AdminSandboxesQuery, FreshHost, HostPatch, PurgeReply, PurgeRequest, Recycle,
    SandboxesEnabled,
};
use hermit_api::deployment::Deployment;
use hermit_api::error::{self, codes};
use hermit_api::exec::{ExecRequest, ExecResult};
use hermit_api::ops::SnapshotQuery;
use hermit_api::people::{ExplicitPerson, People, Person, Role, WarmReply, WarmRequest, normalise_email};
use hermit_api::sandbox::{
    Cursor, MAX_WAIT_MS, PutSandbox, PutSandboxRequest, Release, ReleaseReason, ReleaseSandboxReply,
    ReleaseSandboxRequest, Sandbox, SandboxEvent, SandboxEvents, SandboxEventsQuery, SandboxPhase, SandboxQuery,
    SandboxState, SnapshotReply, SupervisorInfo,
};
use hermit_api::sandbox_surface::{
    ActivityReply, ActivityReplyState, ActivityReport, PendingRelease, SandboxSelf, SecretKeys, SecretValue,
    valid_secret_key,
};
use hermit_api::server::{Ctx, RouteGuard, SelfAnswer, Server};
use hermit_api::token::{Claims, MasterKey, TokenKeys, TokenLiveness, sha256_hex};
use hermit_api::workspaces::{
    Amount, ByteRange, CloneRequest, ENTRIES_PAGE, Entries, EntriesQuery, Entry, EntryType, FileContent, FileMeta,
    FilesQuery, PutWorkspace, Workspace,
};
use hermit_api::{Timestamp, routes};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::Digest;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::answer::not_supported;
use crate::fault::{self, Faults, Names};
use crate::script::{Call, Reply, Script};
use crate::serve::{Served, serve};

const DEFAULT_DEPLOYMENT: &str = include_str!("../../../crates/hermit-api/fixtures/deployment.json");
const DEFAULT_SELF: &str = include_str!("../../../crates/hermit-api/fixtures/sandbox-self.json");

/// The model's configuration.
#[derive(Clone, Debug)]
pub struct HermitConfig {
    /// `GET /v1/deployment`.
    pub deployment: Deployment,
    /// The catalogue's `users`: emails or `@domain` entries.
    pub users: Vec<String>,
    /// The catalogue's `admins`.
    pub admins: Vec<String>,
    /// How long a new sandbox stays `starting`; `None`: until [`FakeHermit::make_ready`].
    pub ready_after: Option<Duration>,
    /// Lifetime (`expiresAt - createdAt`).
    pub lifetime: Duration,
    /// The descriptor template (`sandbox`, `control`, `secrets` and `etag` are filled per sandbox).
    pub descriptor: SandboxSelf,
    /// Granted secrets by key (hermit 7; YYC grants none).
    pub secrets: BTreeMap<String, String>,
    /// The quota of a new workspace.
    pub workspace_quota: Amount,
}

impl Default for HermitConfig {
    fn default() -> Self {
        Self {
            deployment: serde_json::from_str(DEFAULT_DEPLOYMENT).unwrap_or_else(|e| panic!("deployment fixture: {e}")),
            users: vec!["@example.org".into(), "admin@example.com".into()],
            admins: vec!["admin@example.com".into()],
            ready_after: Some(Duration::ZERO),
            lifetime: Duration::from_hours(8),
            descriptor: serde_json::from_str(DEFAULT_SELF).unwrap_or_else(|e| panic!("sandbox-self fixture: {e}")),
            secrets: BTreeMap::new(),
            workspace_quota: Amount {
                bytes: 1 << 30,
                inodes: 1000,
            },
        }
    }
}

impl HermitConfig {
    /// The catalogue's answer for one address (hermit 3.4.1), `None` for a malformed one.
    #[must_use]
    pub fn person(&self, raw: &str) -> Option<Person> {
        let email = normalise_email(raw)?;
        let domain = email.rsplit('@').next().unwrap_or_default();
        let norm = |s: &String| s.trim().to_lowercase();
        let listed = self
            .users
            .iter()
            .map(norm)
            .any(|u| u == email || u == format!("@{domain}"));
        let admin = self.admins.iter().map(norm).any(|a| a == email);
        let role = if admin {
            Some(Role::Admin)
        } else if listed {
            Some(Role::Staff)
        } else {
            None
        };
        Some(Person {
            email,
            allowed: role.is_some(),
            role,
        })
    }

    fn allowed(&self, email: &str) -> bool {
        self.person(email).is_some_and(|p| p.allowed)
    }
}

#[derive(Debug, Clone)]
struct Sbx {
    s: Sandbox,
    due: Option<tokio::time::Instant>,
    pending: Option<PendingRelease>,
}

#[derive(Debug, Default)]
struct Ws {
    created_at: Option<Timestamp>,
    files: BTreeMap<String, (Vec<u8>, Timestamp)>,
}

#[derive(Debug, Default)]
struct Model {
    sandboxes: Vec<Sbx>,
    events: Vec<SandboxEvent>,
    next_seq: u64,
    oldest_kept: u64,
    workspaces: HashMap<Uuid, Ws>,
    hints: Vec<WarmRequest>,
}

/// The fake hermit.
pub struct FakeHermit {
    config: Mutex<HermitConfig>,
    script: Script,
    faults: Arc<Faults>,
    model: Mutex<Model>,
    changed: Notify,
    key: MasterKey,
    tokens: Option<HashMap<Caller, String>>,
    loaded_at: Timestamp,
}

impl std::fmt::Debug for FakeHermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeHermit")
            .field("script", &self.script)
            .finish_non_exhaustive()
    }
}

/// A [`FakeHermit`] served on a local port.
#[derive(Debug)]
pub struct Running {
    /// The fake.
    pub fake: Arc<FakeHermit>,
    /// The listener.
    pub served: Served,
}

impl Running {
    /// The base URL.
    #[must_use]
    pub fn url(&self) -> String {
        self.served.url()
    }

    /// A hermit-api client speaking as `caller` (its pair token set).
    #[must_use]
    pub fn client(&self, http: reqwest::Client, caller: Caller) -> hermit_api::Client {
        let c = hermit_api::Client::new(http, self.url());
        match self.fake.token(caller) {
            Some(t) => c.bearer(t),
            None => c,
        }
    }
}

fn names() -> Names {
    Arc::new(|method: &Method, template: &str| {
        NAMES
            .iter()
            .find(|(m, t, _)| *m == method.as_str() && *t == template)
            .map(|(_, _, n)| *n)
    })
}

/// Each route of hermit-api as (method, template, the trait method's name).
pub const NAMES: &[(&str, &str, &str)] = &[
    ("GET", routes::DEPLOYMENT, "deployment"),
    ("GET", routes::PEOPLE, "people"),
    ("GET", routes::PERSON, "person"),
    ("POST", routes::WARM, "warm"),
    ("PUT", routes::SANDBOX, "put_sandbox"),
    ("GET", routes::SANDBOX, "get_sandbox"),
    ("POST", routes::SANDBOX_RELEASE, "release_sandbox"),
    ("POST", routes::SANDBOX_PAUSE, "pause_sandbox"),
    ("POST", routes::SANDBOX_RESUME, "resume_sandbox"),
    ("POST", routes::SANDBOX_SNAPSHOT, "snapshot_sandbox"),
    ("GET", routes::SANDBOX_EVENTS, "sandbox_events"),
    ("PUT", routes::WORKSPACE, "put_workspace"),
    ("GET", routes::WORKSPACE, "get_workspace"),
    ("DELETE", routes::WORKSPACE, "delete_workspace"),
    ("GET", routes::WORKSPACE_ENTRIES, "workspace_entries"),
    ("GET", routes::WORKSPACE_FILES, "workspace_file"),
    ("POST", routes::WORKSPACE_CLONE, "clone_workspace"),
    ("GET", routes::SANDBOX_TOKEN_KEYS, "token_keys"),
    ("GET", routes::SANDBOX_TOKEN, "token_liveness"),
    ("GET", routes::SANDBOX_SELF, "sandbox_self"),
    ("GET", routes::SANDBOX_SECRETS, "sandbox_secrets"),
    ("GET", routes::SANDBOX_SECRET, "sandbox_secret"),
    ("POST", routes::SANDBOX_ACTIVITY, "sandbox_activity"),
    ("GET", routes::OPS_PAGE, "ops_page"),
    ("GET", routes::OPS_SNAPSHOT, "ops_snapshot"),
    ("GET", routes::ADMIN_HOSTS, "admin_hosts"),
    ("PATCH", routes::ADMIN_HOST, "admin_patch_host"),
    ("PUT", routes::ADMIN_SANDBOXES_ENABLED, "admin_sandboxes_enabled"),
    ("POST", routes::ADMIN_FRESH_HOST, "admin_fresh_host"),
    ("GET", routes::ADMIN_SANDBOXES, "admin_sandboxes"),
    ("GET", routes::ADMIN_SANDBOX_LOG, "admin_sandbox_log"),
    ("POST", routes::ADMIN_SANDBOX_RELEASE, "admin_release"),
    ("POST", routes::ADMIN_SANDBOX_EXEC, "admin_exec"),
    ("POST", routes::ADMIN_RECYCLE, "admin_recycle"),
    ("POST", routes::ADMIN_PERSON_PURGE, "admin_purge"),
    ("GET", routes::ADMIN_EVENTS, "admin_events"),
];

fn ser<T: Serialize>(v: &T) -> Option<Value> {
    serde_json::to_value(v).ok()
}

fn random_32() -> [u8; 32] {
    let mut out = [0; 32];
    out[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    out[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    out
}

fn later(at: Timestamp, d: Duration) -> Timestamp {
    let ms = i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    Timestamp::from_unix_millis(at.unix_millis().saturating_add(ms)).unwrap_or(at)
}

fn no_sandbox() -> ApiError {
    error::not_found(codes::NO_SANDBOX, "no live sandbox")
}

fn no_workspace() -> ApiError {
    error::not_found(codes::NO_WORKSPACE, "no such workspace")
}

fn release_of(reason: ReleaseReason, message: &str) -> Option<Release> {
    Some(Release {
        reason,
        note: None,
        code: reason.code()?,
        message: message.to_owned(),
    })
}

impl FakeHermit {
    /// A fake with the default configuration, its own signing key and a pair token per caller.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_config(HermitConfig::default())
    }

    /// A fake with `config`.
    #[must_use]
    pub fn with_config(config: HermitConfig) -> Arc<Self> {
        let tokens = Caller::ALL
            .iter()
            .map(|c| (*c, format!("fake-hermit-{}-{}", c.as_str(), Uuid::new_v4().simple())))
            .collect();
        Arc::new(Self::build(config, Some(tokens)))
    }

    /// A fake whose router checks no caller tokens (hermit-api's plain `router`).
    #[must_use]
    pub fn unguarded(config: HermitConfig) -> Arc<Self> {
        Arc::new(Self::build(config, None))
    }

    fn build(config: HermitConfig, tokens: Option<HashMap<Caller, String>>) -> Self {
        Self {
            config: Mutex::new(config),
            script: Script::default(),
            faults: Arc::new(Faults::default()),
            model: Mutex::new(Model {
                next_seq: 1,
                ..Model::default()
            }),
            changed: Notify::new(),
            key: MasterKey::new(1, random_32()),
            tokens,
            loaded_at: Timestamp::now(),
        }
    }

    /// The pair token `caller` presents to this fake (`None` when unguarded).
    #[must_use]
    pub fn token(&self, caller: Caller) -> Option<String> {
        self.tokens.as_ref().and_then(|t| t.get(&caller).cloned())
    }

    /// The router: hermit-api's guarded router (or the plain one), with the faults applied.
    ///
    /// # Panics
    /// Never: the generated tokens are distinct and non-empty.
    pub fn router(self: &Arc<Self>) -> Router {
        let router = match &self.tokens {
            Some(t) => {
                let callers = Callers::new(t.iter().map(|(c, tok)| (*c, vec![tok.clone()])))
                    .unwrap_or_else(|e| panic!("fake-hermit tokens: {e}"));
                hermit_api::guarded_router(self.clone(), RouteGuard::new(callers))
            }
            None => hermit_api::router(self.clone()),
        };
        fault::layer(router, self.faults.clone(), names())
    }

    /// Serve on a fresh local port.
    ///
    /// # Errors
    /// When no port can be bound.
    pub async fn serve(self: &Arc<Self>) -> std::io::Result<Running> {
        Ok(Running {
            fake: self.clone(),
            served: serve(self.router()).await?,
        })
    }

    /// The scripted answers and the call log.
    #[must_use]
    pub fn script(&self) -> &Script {
        &self.script
    }

    /// The faults.
    #[must_use]
    pub fn faults(&self) -> &Faults {
        &self.faults
    }

    fn lock(&self) -> MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn config(&self) -> HermitConfig {
        self.config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Change the configuration (the catalogue, the readiness delay, …) while running.
    pub fn configure(&self, f: impl FnOnce(&mut HermitConfig)) {
        f(&mut self.config.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        self.changed.notify_waiters();
    }

    fn record(
        &self,
        route: &'static str,
        ctx: &Ctx,
        params: Value,
        query: Option<Value>,
        body: Option<Value>,
    ) -> Option<Reply> {
        self.script.take(Call {
            route,
            params,
            query,
            body,
            bytes: None,
            headers: ctx.headers.clone(),
        })
    }

    // ---- test controls ----

    /// The sandboxes of every generation, oldest first.
    #[must_use]
    pub fn sandboxes(&self) -> Vec<Sandbox> {
        self.lock().sandboxes.iter().map(|s| s.s.clone()).collect()
    }

    /// The live sandbox of `cid`.
    #[must_use]
    pub fn live(&self, cid: Uuid) -> Option<Sandbox> {
        self.lock()
            .sandboxes
            .iter()
            .rev()
            .find(|s| s.s.conversation_id == cid && s.s.state.is_live())
            .map(|s| s.s.clone())
    }

    /// Make `cid`'s starting sandbox ready now.
    pub fn make_ready(&self, cid: Uuid) {
        let mut m = self.lock();
        if let Some(i) = Self::live_index(&m, cid)
            && m.sandboxes[i].s.state == SandboxState::Starting
        {
            Self::set_ready(&mut m, i);
        }
        drop(m);
        self.changed.notify_waiters();
    }

    /// End `cid`'s live sandbox now (`releasing` then `gone` with `reason`), as hermit's janitor or a host loss
    /// would.
    pub fn end(&self, cid: Uuid, reason: ReleaseReason, message: &str) {
        let mut m = self.lock();
        if let Some(i) = Self::live_index(&m, cid) {
            Self::finish(&mut m, i, reason, message);
        }
        drop(m);
        self.changed.notify_waiters();
    }

    /// Ask `cid`'s supervisor to release (hermit 13.12): the next activity reply carries it.
    pub fn request_release(&self, cid: Uuid, reason: ReleaseReason, force: bool) {
        let mut m = self.lock();
        if let Some(i) = Self::live_index(&m, cid)
            && let Some(code) = reason.call_code()
        {
            m.sandboxes[i].pending = Some(PendingRelease {
                reason,
                code,
                force,
                deadline_at: later(Timestamp::now(), Duration::from_secs(30)),
            });
        }
    }

    /// Drop every feed event with `seq < seq` (retention): an `after` older than that answers 410.
    pub fn expire_events_before(&self, seq: u64) {
        let mut m = self.lock();
        m.events.retain(|e| e.seq >= seq);
        m.oldest_kept = seq;
    }

    /// A sandbox token for `cid`'s live sandbox, presented from `source` (hermit 6.1).
    #[must_use]
    pub fn mint_token(&self, cid: Uuid, source: Ipv4Addr) -> Option<String> {
        let s = self.live(cid)?;
        let claims = Claims {
            sandbox_id: s.id,
            conversation_id: s.conversation_id,
            source,
            expires_at: u64::try_from(s.expires_at.unix_seconds()).unwrap_or(0),
            person: s.person.clone(),
        };
        self.key.signing_key().ok()?.sign(&claims).ok()
    }

    /// The control token hermit presents to `sandbox`'s supervisor (hermit 6.2).
    #[must_use]
    pub fn control_token(&self, sandbox: Uuid) -> Option<String> {
        Some(self.key.control_key().ok()?.control_token(sandbox))
    }

    /// Put a file into workspace `wid` (made if missing).
    pub fn write_file(&self, wid: Uuid, path: &str, data: &[u8]) {
        let mut m = self.lock();
        let ws = m.workspaces.entry(wid).or_default();
        ws.created_at.get_or_insert_with(Timestamp::now);
        ws.files.insert(path.to_owned(), (data.to_vec(), Timestamp::now()));
    }

    /// A workspace file's bytes.
    #[must_use]
    pub fn read_file(&self, wid: Uuid, path: &str) -> Option<Vec<u8>> {
        self.lock()
            .workspaces
            .get(&wid)?
            .files
            .get(path)
            .map(|(d, _)| d.clone())
    }

    /// The warm hints received.
    #[must_use]
    pub fn hints(&self) -> Vec<WarmRequest> {
        self.lock().hints.clone()
    }

    // ---- the model ----

    fn live_index(m: &Model, cid: Uuid) -> Option<usize> {
        m.sandboxes
            .iter()
            .rposition(|s| s.s.conversation_id == cid && s.s.state.is_live())
    }

    fn event(m: &mut Model, s: &Sandbox) {
        let seq = m.next_seq;
        m.next_seq += 1;
        m.events.push(SandboxEvent {
            seq,
            at: Timestamp::now(),
            sandbox_id: s.id,
            conversation_id: s.conversation_id,
            person: s.person.clone(),
            state: s.state,
            phase: s.phase,
            version: s.version,
            busy: s.busy,
            release: s.release.clone(),
        });
    }

    fn set_ready(m: &mut Model, i: usize) {
        let now = Timestamp::now();
        let s = &mut m.sandboxes[i];
        s.s.state = SandboxState::Ready;
        s.s.phase = None;
        s.s.ready_at = Some(now);
        s.s.version += 1;
        s.due = None;
        let snap = s.s.clone();
        Self::event(m, &snap);
    }

    fn finish(m: &mut Model, i: usize, reason: ReleaseReason, message: &str) {
        let now = Timestamp::now();
        {
            let s = &mut m.sandboxes[i];
            s.s.state = SandboxState::Releasing;
            s.s.phase = None;
            s.s.release = release_of(reason, message);
            s.s.version += 1;
            s.due = None;
            s.pending = None;
        }
        let snap = m.sandboxes[i].s.clone();
        Self::event(m, &snap);
        let s = &mut m.sandboxes[i];
        s.s.state = SandboxState::Gone;
        s.s.busy = false;
        s.s.released_at = Some(now);
        s.s.version += 1;
        let snap = s.s.clone();
        Self::event(m, &snap);
    }

    /// Starting sandboxes whose delay has passed become ready.
    fn advance(&self) {
        let now = tokio::time::Instant::now();
        let mut m = self.lock();
        let due: Vec<usize> = m
            .sandboxes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.s.state == SandboxState::Starting && s.due.is_some_and(|d| d <= now))
            .map(|(i, _)| i)
            .collect();
        let any = !due.is_empty();
        for i in due {
            Self::set_ready(&mut m, i);
        }
        drop(m);
        if any {
            self.changed.notify_waiters();
        }
    }

    /// Wait for a change (or a due readiness) until `deadline`; false at the deadline.
    async fn wait_change(&self, deadline: tokio::time::Instant) -> bool {
        let changed = self.changed.notified();
        let tick = tokio::time::Instant::now() + Duration::from_millis(20);
        tokio::select! {
            () = changed => true,
            () = tokio::time::sleep_until(tick.min(deadline)) => tokio::time::Instant::now() < deadline,
        }
    }

    fn model_put(&self, cid: Uuid, body: &PutSandboxRequest) -> Result<PutSandbox, ApiError> {
        let config = self.config();
        let person = config
            .person(&body.person)
            .ok_or_else(|| error::api(400, codes::BAD_EMAIL, "not an email address"))?;
        if !person.allowed {
            return Err(error::not_allowed());
        }
        self.advance();
        let mut m = self.lock();
        if let Some(i) = Self::live_index(&m, cid) {
            if m.sandboxes[i].s.person != person.email {
                return Err(error::api(
                    409,
                    codes::PERSON_MISMATCH,
                    "the live sandbox belongs to another person",
                ));
            }
            match m.sandboxes[i].s.state {
                SandboxState::Ready => {
                    let s = &mut m.sandboxes[i];
                    s.pending = None;
                    s.s.last_active_at = Timestamp::now();
                    return Ok(PutSandbox::Ready(s.s.clone()));
                }
                SandboxState::Starting => {
                    let s = &mut m.sandboxes[i];
                    s.s.last_active_at = Timestamp::now();
                    return Ok(PutSandbox::Starting(s.s.clone()));
                }
                _ => Self::finish(&mut m, i, ReleaseReason::Requested, "replaced"),
            }
        }
        let now = Timestamp::now();
        let s = Sandbox {
            id: Uuid::now_v7(),
            conversation_id: cid,
            person: person.email,
            state: SandboxState::Starting,
            phase: Some(SandboxPhase::Creating),
            version: 1,
            created_at: now,
            ready_at: None,
            last_active_at: now,
            expires_at: later(now, config.lifetime),
            released_at: None,
            busy: false,
            image: Some(config.descriptor.sandbox.image.clone()),
            supervisor: None,
            release: None,
        };
        Self::event(&mut m, &s);
        m.sandboxes.push(Sbx {
            s: s.clone(),
            due: config.ready_after.map(|d| tokio::time::Instant::now() + d),
            pending: None,
        });
        drop(m);
        self.changed.notify_waiters();
        Ok(PutSandbox::Starting(s))
    }

    fn current(&self, cid: Uuid) -> Option<Sandbox> {
        let m = self.lock();
        let live = m
            .sandboxes
            .iter()
            .rev()
            .find(|s| s.s.conversation_id == cid && s.s.state.is_live());
        let recent_gone = || {
            m.sandboxes.iter().rev().find(|s| {
                s.s.conversation_id == cid
                    && s.s
                        .released_at
                        .is_some_and(|r| Timestamp::now().unix_millis() - r.unix_millis() < 600_000)
            })
        };
        live.or_else(recent_gone).map(|s| s.s.clone())
    }

    async fn model_get(&self, cid: Uuid, q: SandboxQuery) -> Result<Sandbox, ApiError> {
        self.advance();
        let first = self.current(cid).ok_or_else(no_sandbox)?;
        let wait = q.wait_ms.unwrap_or(0).min(MAX_WAIT_MS);
        if wait == 0 {
            return Ok(first);
        }
        let after = q.after_version.unwrap_or(first.version);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait.into());
        loop {
            self.advance();
            let s = self.current(cid).ok_or_else(no_sandbox)?;
            if s.version > after || s.state != SandboxState::Starting || s.id != first.id {
                return Ok(s);
            }
            if !self.wait_change(deadline).await {
                return self.current(cid).ok_or_else(no_sandbox);
            }
        }
    }

    fn model_release(&self, cid: Uuid, body: ReleaseSandboxRequest) -> ReleaseSandboxReply {
        let mut m = self.lock();
        let reply = match Self::live_index(&m, cid) {
            Some(i) => {
                Self::finish(&mut m, i, body.reason.into(), "released by request");
                ReleaseSandboxReply {
                    released: 1,
                    sandbox: Some(m.sandboxes[i].s.clone()),
                }
            }
            None => ReleaseSandboxReply {
                released: 0,
                sandbox: None,
            },
        };
        drop(m);
        self.changed.notify_waiters();
        reply
    }

    async fn model_events(&self, q: SandboxEventsQuery) -> Result<SandboxEvents, ApiError> {
        let limit = q.limit.unwrap_or(200).clamp(1, 500) as usize;
        let after = match q.after {
            Cursor::Latest => {
                let m = self.lock();
                return Ok(SandboxEvents {
                    head_seq: m.next_seq - 1,
                    events: Vec::new(),
                });
            }
            Cursor::Seq(n) => n,
        };
        if after.saturating_add(1) < self.lock().oldest_kept {
            return Err(error::api(
                410,
                codes::CURSOR_EXPIRED,
                "the cursor is older than the retained events",
            ));
        }
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(q.wait_ms.unwrap_or(0).min(MAX_WAIT_MS).into());
        loop {
            self.advance();
            let page = {
                let m = self.lock();
                SandboxEvents {
                    head_seq: m.next_seq - 1,
                    events: m.events.iter().filter(|e| e.seq > after).take(limit).cloned().collect(),
                }
            };
            if !page.events.is_empty() || tokio::time::Instant::now() >= deadline {
                return Ok(page);
            }
            if !self.wait_change(deadline).await {
                let m = self.lock();
                return Ok(SandboxEvents {
                    head_seq: m.next_seq - 1,
                    events: m.events.iter().filter(|e| e.seq > after).take(limit).cloned().collect(),
                });
            }
        }
    }

    /// The live sandbox matching the gateway's identity headers.
    fn own(&self, ctx: &Ctx) -> Result<Sandbox, ApiError> {
        self.advance();
        let id = ctx.sandbox().ok_or_else(no_sandbox)?;
        let m = self.lock();
        m.sandboxes
            .iter()
            .find(|s| {
                s.s.id == id.id
                    && s.s.conversation_id == id.conversation
                    && s.s.person == id.person.trim().to_lowercase()
                    && s.s.state.is_live()
            })
            .map(|s| s.s.clone())
            .ok_or_else(no_sandbox)
    }

    fn descriptor(&self, s: &Sandbox) -> SandboxSelf {
        let config = self.config();
        let mut d = config.descriptor;
        d.sandbox.id = s.id;
        d.sandbox.conversation_id = s.conversation_id;
        d.sandbox.person.clone_from(&s.person);
        d.sandbox.created_at = s.created_at;
        d.sandbox.expires_at = s.expires_at;
        d.control.token_sha256 = self
            .control_token(s.id)
            .map(|t| vec![sha256_hex(&t)])
            .unwrap_or_default();
        d.secrets = SecretKeys {
            keys: config.secrets.keys().cloned().collect(),
        };
        d.with_etag()
    }

    fn model_activity(&self, ctx: &Ctx, body: &ActivityReport) -> Result<ActivityReply, ApiError> {
        let own = self.own(ctx)?;
        let mut m = self.lock();
        let i = m
            .sandboxes
            .iter()
            .position(|s| s.s.id == own.id)
            .ok_or_else(no_sandbox)?;
        let busy = body.busy();
        let flipped = {
            let s = &mut m.sandboxes[i];
            s.s.supervisor = Some(SupervisorInfo {
                protocol: body.protocol,
                version: body.version.clone(),
                boot_id: body.boot_id.clone(),
            });
            if busy {
                s.s.last_active_at = Timestamp::now();
            }
            let flipped = s.s.busy != busy;
            if flipped {
                s.s.busy = busy;
                s.s.version += 1;
            }
            flipped
        };
        if flipped {
            let snap = m.sandboxes[i].s.clone();
            Self::event(&mut m, &snap);
        }
        let s = &m.sandboxes[i];
        let reply = ActivityReply {
            state: if s.s.state == SandboxState::Releasing {
                ActivityReplyState::Releasing
            } else {
                ActivityReplyState::Ready
            },
            release: s.pending.clone(),
        };
        drop(m);
        if flipped {
            self.changed.notify_waiters();
        }
        Ok(reply)
    }

    fn workspace_of(ws: &Ws, id: Uuid, quota: Amount) -> Workspace {
        Workspace {
            id,
            created_at: ws.created_at.unwrap_or_else(Timestamp::now),
            quota,
            usage: Amount {
                bytes: ws.files.values().map(|(d, _)| d.len() as u64).sum(),
                inodes: ws.files.len() as u64,
            },
        }
    }

    fn model_entries(&self, wid: Uuid, q: &EntriesQuery) -> Result<Entries, ApiError> {
        let path = q.path.clone().unwrap_or_default();
        let m = self.lock();
        let ws = m.workspaces.get(&wid).ok_or_else(no_workspace)?;
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        if !path.is_empty() && ws.files.contains_key(&path) {
            return Err(error::api(409, codes::NOT_A_DIR, "not a directory"));
        }
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for (p, (d, at)) in &ws.files {
            let Some(rest) = p.strip_prefix(&prefix) else { continue };
            let (name, dir) = match rest.split_once('/') {
                Some((n, _)) => (n, true),
                None => (rest, false),
            };
            entries.entry(name.to_owned()).or_insert(Entry {
                name: name.to_owned(),
                kind: if dir { EntryType::Dir } else { EntryType::File },
                size: if dir { 0 } else { d.len() as u64 },
                mtime: *at,
            });
        }
        if entries.is_empty() && !path.is_empty() {
            return Err(error::not_found(codes::NO_PATH, "no such path"));
        }
        let start: Vec<Entry> = entries
            .into_values()
            .filter(|e| q.cursor.as_ref().is_none_or(|c| e.name.as_str() > c.as_str()))
            .collect();
        let next = (start.len() > ENTRIES_PAGE).then(|| start[ENTRIES_PAGE - 1].name.clone());
        Ok(Entries {
            path,
            entries: start.into_iter().take(ENTRIES_PAGE).collect(),
            next_cursor: next,
        })
    }

    fn model_file(&self, wid: Uuid, q: &FilesQuery, range: Option<ByteRange>) -> Result<FileContent, ApiError> {
        let m = self.lock();
        let ws = m.workspaces.get(&wid).ok_or_else(no_workspace)?;
        let (data, at) = ws
            .files
            .get(&q.path)
            .ok_or_else(|| error::not_found(codes::NO_PATH, "no such file"))?;
        let size = data.len() as u64;
        let served = match range {
            None => None,
            Some(r) => Some(r.resolve(size).ok_or_else(|| error::range_not_satisfiable(size))?),
        };
        #[allow(clippy::cast_possible_truncation)] // in-memory sizes
        let bytes = match served {
            Some((a, b)) => data[a as usize..=b as usize].to_vec(),
            None => data.clone(),
        };
        let meta = FileMeta {
            size,
            mtime: *at,
            etag: FileMeta::etag_for(size, i128::from(at.unix_millis()) * 1_000_000),
            served,
        };
        Ok(FileContent::from_bytes(meta, Bytes::from(bytes)))
    }
}

fn typed<T: serde::de::DeserializeOwned>(r: &Reply, route: &str) -> Result<T, ApiError> {
    r.typed(route)
}

macro_rules! answer {
    ($s:ident, $route:literal, $ctx:expr, $params:expr, $query:expr, $body:expr, $model:expr) => {
        match $s.record($route, &$ctx, $params, $query, $body) {
            Some(r) => typed(&r, $route),
            None => $model,
        }
    };
}

fn cid(v: Uuid) -> Value {
    json!({"cid": v.to_string()})
}

fn wid(v: Uuid) -> Value {
    json!({"wid": v.to_string()})
}

#[allow(clippy::too_many_lines, clippy::unused_async_trait_impl)] // every method is async, as the trait
impl Server for FakeHermit {
    async fn deployment(&self, ctx: Ctx) -> Result<Deployment, ApiError> {
        answer!(
            self,
            "deployment",
            ctx,
            json!({}),
            None,
            None,
            Ok(self.config().deployment)
        )
    }

    async fn person(&self, ctx: Ctx, email: String) -> Result<Person, ApiError> {
        let model = self
            .config()
            .person(&email)
            .ok_or_else(|| error::api(400, codes::BAD_EMAIL, "not an email address"));
        answer!(self, "person", ctx, json!({"email": email}), None, None, model)
    }

    async fn people(&self, ctx: Ctx) -> Result<People, ApiError> {
        let config = self.config();
        let catalogue = json!({"users": config.users, "admins": config.admins}).to_string();
        let explicit = config
            .users
            .iter()
            .filter(|u| !u.starts_with('@'))
            .filter_map(|u| config.person(u))
            .filter_map(|p| {
                Some(ExplicitPerson {
                    email: p.email,
                    role: p.role?,
                })
            })
            .collect();
        let domains = config
            .users
            .iter()
            .filter_map(|u| u.strip_prefix('@'))
            .map(str::to_lowercase)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let model = Ok(People {
            generation: crate::magpie::hex_digest(&sha2::Sha256::digest(catalogue.as_bytes())),
            loaded_at: self.loaded_at,
            explicit,
            domains,
        });
        answer!(self, "people", ctx, json!({}), None, None, model)
    }

    async fn warm(&self, ctx: Ctx, body: WarmRequest) -> Result<WarmReply, ApiError> {
        if let Some(r) = self.record("warm", &ctx, json!({}), None, ser(&body)) {
            return typed(&r, "warm");
        }
        if !self.config().allowed(&body.person) {
            return Err(error::not_allowed());
        }
        let live = body.conversation_id.is_some_and(|c| self.live(c).is_some());
        self.lock().hints.push(body);
        Ok(WarmReply { hinted: !live })
    }

    async fn put_sandbox(&self, ctx: Ctx, cid_: Uuid, body: PutSandboxRequest) -> Result<PutSandbox, ApiError> {
        match self.record("put_sandbox", &ctx, cid(cid_), None, ser(&body)) {
            Some(r) => {
                let s: Sandbox = r.typed("put_sandbox")?;
                Ok(if r.status == 200 {
                    PutSandbox::Ready(s)
                } else {
                    PutSandbox::Starting(s)
                })
            }
            None => self.model_put(cid_, &body),
        }
    }

    async fn get_sandbox(&self, ctx: Ctx, cid_: Uuid, q: SandboxQuery) -> Result<Sandbox, ApiError> {
        match self.record("get_sandbox", &ctx, cid(cid_), ser(&q), None) {
            Some(r) => r.typed("get_sandbox"),
            None => self.model_get(cid_, q).await,
        }
    }

    async fn release_sandbox(
        &self,
        ctx: Ctx,
        cid_: Uuid,
        body: ReleaseSandboxRequest,
    ) -> Result<ReleaseSandboxReply, ApiError> {
        answer!(
            self,
            "release_sandbox",
            ctx,
            cid(cid_),
            None,
            ser(&body),
            Ok(self.model_release(cid_, body))
        )
    }

    async fn pause_sandbox(&self, ctx: Ctx, cid_: Uuid) -> Result<Sandbox, ApiError> {
        answer!(
            self,
            "pause_sandbox",
            ctx,
            cid(cid_),
            None,
            None,
            Err(error::not_supported())
        )
    }

    async fn resume_sandbox(&self, ctx: Ctx, cid_: Uuid) -> Result<Sandbox, ApiError> {
        answer!(
            self,
            "resume_sandbox",
            ctx,
            cid(cid_),
            None,
            None,
            Err(error::not_supported())
        )
    }

    async fn snapshot_sandbox(&self, ctx: Ctx, cid_: Uuid) -> Result<SnapshotReply, ApiError> {
        answer!(
            self,
            "snapshot_sandbox",
            ctx,
            cid(cid_),
            None,
            None,
            Err(error::not_supported())
        )
    }

    async fn sandbox_events(&self, ctx: Ctx, q: SandboxEventsQuery) -> Result<SandboxEvents, ApiError> {
        match self.record("sandbox_events", &ctx, json!({}), ser(&q), None) {
            Some(r) => r.typed("sandbox_events"),
            None => self.model_events(q).await,
        }
    }

    async fn put_workspace(&self, ctx: Ctx, wid_: Uuid) -> Result<PutWorkspace, ApiError> {
        if let Some(r) = self.record("put_workspace", &ctx, wid(wid_), None, Some(json!({}))) {
            let w: Workspace = r.typed("put_workspace")?;
            return Ok(if r.status == 200 {
                PutWorkspace::Existed(w)
            } else {
                PutWorkspace::Created(w)
            });
        }
        let quota = self.config().workspace_quota;
        let mut m = self.lock();
        let existed = m.workspaces.contains_key(&wid_);
        let ws = m.workspaces.entry(wid_).or_default();
        ws.created_at.get_or_insert_with(Timestamp::now);
        let w = Self::workspace_of(ws, wid_, quota);
        Ok(if existed {
            PutWorkspace::Existed(w)
        } else {
            PutWorkspace::Created(w)
        })
    }

    async fn get_workspace(&self, ctx: Ctx, wid_: Uuid) -> Result<Workspace, ApiError> {
        let quota = self.config().workspace_quota;
        let model = self
            .lock()
            .workspaces
            .get(&wid_)
            .map(|ws| Self::workspace_of(ws, wid_, quota))
            .ok_or_else(no_workspace);
        answer!(self, "get_workspace", ctx, wid(wid_), None, None, model)
    }

    async fn delete_workspace(&self, ctx: Ctx, wid_: Uuid) -> Result<(), ApiError> {
        if let Some(r) = self.record("delete_workspace", &ctx, wid(wid_), None, None) {
            return r.unit();
        }
        self.lock().workspaces.remove(&wid_);
        Ok(())
    }

    async fn workspace_entries(&self, ctx: Ctx, wid_: Uuid, q: EntriesQuery) -> Result<Entries, ApiError> {
        answer!(
            self,
            "workspace_entries",
            ctx,
            wid(wid_),
            ser(&q),
            None,
            self.model_entries(wid_, &q)
        )
    }

    async fn workspace_file(
        &self,
        ctx: Ctx,
        wid_: Uuid,
        q: FilesQuery,
        range: Option<ByteRange>,
    ) -> Result<FileContent, ApiError> {
        match self.record("workspace_file", &ctx, wid(wid_), ser(&q), None) {
            Some(r) => {
                if r.is_error() {
                    return Err(r.api_error());
                }
                let data = match &r.body {
                    crate::script::ReplyBody::Bytes(b) => b.clone(),
                    _ => Bytes::new(),
                };
                let size = data.len() as u64;
                let meta = FileMeta {
                    size,
                    mtime: Timestamp::now(),
                    etag: FileMeta::etag_for(size, 0),
                    served: None,
                };
                Ok(FileContent::from_bytes(meta, data))
            }
            None => self.model_file(wid_, &q, range),
        }
    }

    async fn clone_workspace(&self, ctx: Ctx, wid_: Uuid, body: CloneRequest) -> Result<Workspace, ApiError> {
        if let Some(r) = self.record("clone_workspace", &ctx, wid(wid_), None, ser(&body)) {
            return r.typed("clone_workspace");
        }
        let quota = self.config().workspace_quota;
        let mut m = self.lock();
        let files = m.workspaces.get(&body.from).ok_or_else(no_workspace)?.files.clone();
        if m.workspaces.contains_key(&wid_) {
            return Err(error::api(409, codes::EXISTS, "the target workspace exists"));
        }
        let ws = Ws {
            created_at: Some(Timestamp::now()),
            files,
        };
        let w = Self::workspace_of(&ws, wid_, quota);
        m.workspaces.insert(wid_, ws);
        Ok(w)
    }

    async fn token_keys(&self, ctx: Ctx) -> Result<TokenKeys, ApiError> {
        let model = self
            .key
            .signing_key()
            .map(|k| TokenKeys {
                keys: vec![k.public_key()],
            })
            .map_err(|e| ApiError::internal(e.to_string()));
        answer!(self, "token_keys", ctx, json!({}), None, None, model)
    }

    async fn token_liveness(&self, ctx: Ctx, sandbox_id: Uuid) -> Result<TokenLiveness, ApiError> {
        if let Some(r) = self.record(
            "token_liveness",
            &ctx,
            json!({"sandboxId": sandbox_id.to_string()}),
            None,
            None,
        ) {
            return r.typed("token_liveness");
        }
        self.advance();
        let config = self.config();
        let s = self
            .lock()
            .sandboxes
            .iter()
            .find(|s| s.s.id == sandbox_id)
            .map(|s| s.s.clone())
            .ok_or_else(|| error::not_found(codes::UNKNOWN_SANDBOX, "unknown sandbox"))?;
        Ok(TokenLiveness {
            sandbox_id: s.id,
            conversation_id: s.conversation_id,
            live: s.state.is_live() && config.allowed(&s.person),
            person: s.person,
            state: s.state,
            expires_at: s.expires_at,
        })
    }

    async fn sandbox_self(&self, ctx: Ctx, if_none_match: Option<String>) -> Result<SelfAnswer, ApiError> {
        if let Some(r) = self.record("sandbox_self", &ctx, json!({}), None, None) {
            if r.status == 304 {
                let etag = r
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("etag"))
                    .map(|(_, v)| v.clone());
                return Ok(SelfAnswer::NotModified {
                    etag: etag.unwrap_or_default(),
                });
            }
            return r.typed("sandbox_self").map(|d| SelfAnswer::Fresh(Box::new(d)));
        }
        let s = self.own(&ctx)?;
        let d = self.descriptor(&s);
        if if_none_match.as_deref() == Some(d.etag.as_str()) {
            return Ok(SelfAnswer::NotModified { etag: d.etag });
        }
        Ok(SelfAnswer::Fresh(Box::new(d)))
    }

    async fn sandbox_secrets(&self, ctx: Ctx) -> Result<SecretKeys, ApiError> {
        if let Some(r) = self.record("sandbox_secrets", &ctx, json!({}), None, None) {
            return r.typed("sandbox_secrets");
        }
        self.own(&ctx)?;
        Ok(SecretKeys {
            keys: self.config().secrets.keys().cloned().collect(),
        })
    }

    async fn sandbox_secret(&self, ctx: Ctx, key: String) -> Result<SecretValue, ApiError> {
        if let Some(r) = self.record("sandbox_secret", &ctx, json!({"key": key}), None, None) {
            return r.typed("sandbox_secret");
        }
        self.own(&ctx)?;
        if !valid_secret_key(&key) {
            return Err(ApiError::bad_request("bad key"));
        }
        let value = self
            .config()
            .secrets
            .get(&key)
            .cloned()
            .ok_or_else(|| error::not_found(codes::NOT_GRANTED, "key not granted"))?;
        Ok(SecretValue { key, value })
    }

    async fn sandbox_activity(&self, ctx: Ctx, body: ActivityReport) -> Result<ActivityReply, ApiError> {
        answer!(
            self,
            "sandbox_activity",
            ctx,
            json!({}),
            None,
            ser(&body),
            self.model_activity(&ctx, &body)
        )
    }

    async fn ops_page(&self, ctx: Ctx) -> Result<String, ApiError> {
        match self.record("ops_page", &ctx, json!({}), None, None) {
            Some(r) if r.is_error() => Err(r.api_error()),
            Some(r) => Ok(match r.body {
                crate::script::ReplyBody::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
                crate::script::ReplyBody::Json(Value::String(s)) => s,
                _ => String::new(),
            }),
            None => Err(not_supported("ops_page")),
        }
    }

    async fn ops_snapshot(&self, ctx: Ctx, q: SnapshotQuery) -> Result<Value, ApiError> {
        answer!(
            self,
            "ops_snapshot",
            ctx,
            json!({}),
            ser(&q),
            None,
            Err(not_supported("ops_snapshot"))
        )
    }

    async fn admin_hosts(&self, ctx: Ctx) -> Result<Value, ApiError> {
        answer!(
            self,
            "admin_hosts",
            ctx,
            json!({}),
            None,
            None,
            Err(not_supported("admin_hosts"))
        )
    }

    async fn admin_patch_host(&self, ctx: Ctx, name: String, body: HostPatch) -> Result<Value, ApiError> {
        let params = json!({"name": name});
        answer!(
            self,
            "admin_patch_host",
            ctx,
            params,
            None,
            ser(&body),
            Err(not_supported("admin_patch_host"))
        )
    }

    async fn admin_sandboxes_enabled(&self, ctx: Ctx, body: SandboxesEnabled) -> Result<Value, ApiError> {
        let fallback = Err(not_supported("admin_sandboxes_enabled"));
        answer!(
            self,
            "admin_sandboxes_enabled",
            ctx,
            json!({}),
            None,
            ser(&body),
            fallback
        )
    }

    async fn admin_fresh_host(&self, ctx: Ctx, body: FreshHost) -> Result<Value, ApiError> {
        answer!(
            self,
            "admin_fresh_host",
            ctx,
            json!({}),
            None,
            ser(&body),
            Err(not_supported("admin_fresh_host"))
        )
    }

    async fn admin_sandboxes(&self, ctx: Ctx, q: AdminSandboxesQuery) -> Result<Value, ApiError> {
        answer!(
            self,
            "admin_sandboxes",
            ctx,
            json!({}),
            ser(&q),
            None,
            Err(not_supported("admin_sandboxes"))
        )
    }

    async fn admin_sandbox_log(&self, ctx: Ctx, id: Uuid) -> Result<Value, ApiError> {
        let params = json!({"id": id.to_string()});
        answer!(
            self,
            "admin_sandbox_log",
            ctx,
            params,
            None,
            None,
            Err(not_supported("admin_sandbox_log"))
        )
    }

    async fn admin_release(&self, ctx: Ctx, id: Uuid, body: AdminRelease) -> Result<Value, ApiError> {
        let params = json!({"id": id.to_string()});
        answer!(
            self,
            "admin_release",
            ctx,
            params,
            None,
            ser(&body),
            Err(not_supported("admin_release"))
        )
    }

    async fn admin_exec(&self, ctx: Ctx, id: Uuid, body: ExecRequest) -> Result<ExecResult, ApiError> {
        let params = json!({"id": id.to_string()});
        answer!(
            self,
            "admin_exec",
            ctx,
            params,
            None,
            ser(&body),
            Err(not_supported("admin_exec"))
        )
    }

    async fn admin_recycle(&self, ctx: Ctx, body: Recycle) -> Result<Value, ApiError> {
        answer!(
            self,
            "admin_recycle",
            ctx,
            json!({}),
            None,
            ser(&body),
            Err(not_supported("admin_recycle"))
        )
    }

    async fn admin_purge(&self, ctx: Ctx, email: String, body: PurgeRequest) -> Result<PurgeReply, ApiError> {
        let params = json!({"email": email});
        answer!(
            self,
            "admin_purge",
            ctx,
            params,
            None,
            ser(&body),
            Err(not_supported("admin_purge"))
        )
    }

    async fn admin_events(&self, ctx: Ctx, q: AdminEventsQuery) -> Result<Value, ApiError> {
        answer!(
            self,
            "admin_events",
            ctx,
            json!({}),
            ser(&q),
            None,
            Err(not_supported("admin_events"))
        )
    }
}
